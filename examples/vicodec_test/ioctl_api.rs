use super::framegen;
use nix::fcntl::{open, OFlag};
use nix::sys::stat::Mode;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Write};
use std::os::unix::io::FromRawFd;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use v4l2::ioctl::*;
use v4l2::memory::{MMAPHandle, MemoryType, UserPtrHandle};
use v4l2::{Format, QueueType::*};

/// Run a sample encoder on device `device_path`, which must be a `vicodec`
/// encoder instance. `lets_quit` will turn to true when Ctrl+C is pressed.
pub fn run(device_path: &Path, lets_quit: Arc<AtomicBool>) {
    let mut fd = unsafe {
        File::from_raw_fd(
            open(device_path, OFlag::O_RDWR | OFlag::O_CLOEXEC, Mode::empty())
                .expect(&format!("Cannot open {}", device_path.display())),
        )
    };

    // Check that we are dealing with vicodec.
    let caps: Capability = querycap(&fd).expect("Failed to get device capacities");
    println!(
        "Opened device: {}\n\tdriver: {}\n\tbus: {}\n\tcapabilities: {}",
        caps.card, caps.driver, caps.bus_info, caps.capabilities
    );

    if caps.driver != "vicodec" {
        panic!(
            "This device is {}, but this test is designed to work with the vicodec driver.",
            caps.driver
        );
    }

    // Check whether the driver uses the single or multi-planar API by
    // requesting 0 MMAP buffers on the OUTPUT queue. The working queue will
    // return a success.
    let use_multi_planar =
        if let Ok(_) = reqbufs::<(), _>(&mut fd, VideoOutput, MemoryType::MMAP, 0) {
            false
        } else if let Ok(_) = reqbufs::<(), _>(&mut fd, VideoOutputMplane, MemoryType::MMAP, 0) {
            true
        } else {
            panic!("Both single-planar and multi-planar queues are unusable.");
        };
    println!(
        "Multi-planar: {}",
        if use_multi_planar { "yes" } else { "no" }
    );

    let (output_queue, capture_queue) = match use_multi_planar {
        false => (VideoOutput, VideoCapture),
        true => (VideoOutputMplane, VideoCaptureMplane),
    };

    // List the output formats.
    let out_formats = FormatIterator::new(&fd, output_queue)
        .map(|f| (f.pixelformat, f))
        .collect::<BTreeMap<_, _>>();
    println!("Output formats:");
    for (_, fmtdesc) in out_formats.iter() {
        println!("\t{}", fmtdesc);
    }

    // List the capture formats.
    let cap_formats = FormatIterator::new(&fd, capture_queue)
        .map(|f| (f.pixelformat, f))
        .collect::<BTreeMap<_, _>>();
    println!("Capture formats:");
    for (_, fmtdesc) in cap_formats.iter() {
        println!("\t{}", fmtdesc);
    }

    // We will encode from RGB3 to FWHT.
    if !out_formats.contains_key(&b"RGB3".into()) {
        panic!("RGB3 format not supported on OUTPUT queue.");
    }

    if !cap_formats.contains_key(&b"FWHT".into()) {
        panic!("FWHT format not supported on CAPTURE queue.");
    }

    // We will be happy with 640x480 resolution.
    let output_format = Format {
        width: 640,
        height: 480,
        pixelformat: b"RGB3".into(),
        ..Default::default()
    };

    println!("Setting output format: {:?}", output_format);
    let output_format: Format =
        s_fmt(&mut fd, output_queue, output_format).expect("Failed setting output format");
    println!("Adjusted output format: {:?}", output_format);

    let mut capture_format: Format =
        g_fmt(&fd, capture_queue).expect("Failed getting capture format");
    // Let's just make sure the encoding format on the CAPTURE queue is FWHT.
    capture_format.pixelformat = b"FWHT".into();
    println!("Setting capture format: {:?}", capture_format);
    let capture_format: Format =
        s_fmt(&mut fd, capture_queue, capture_format).expect("Failed setting capture format");
    println!("Adjusted capture format: {:?}", capture_format);

    // We could run this with as little as one buffer, but let's cycle between
    // two for the sake of it.
    // For simplicity the OUTPUT buffers will use user memory.
    let num_output_buffers: usize = reqbufs(&mut fd, output_queue, MemoryType::UserPtr, 2)
        .expect("Failed to allocate output buffers");
    let num_capture_buffers: usize = reqbufs(&mut fd, capture_queue, MemoryType::MMAP, 2)
        .expect("Failed to allocate capture buffers");
    println!(
        "Using {} output and {} capture buffers.",
        num_output_buffers, num_capture_buffers
    );

    let output_image_size = output_format.plane_fmt[0].sizeimage as usize;
    let output_image_bytesperline = output_format.plane_fmt[0].bytesperline as usize;
    let mut output_buffers: Vec<Vec<u8>> = std::iter::repeat(vec![0u8; output_image_size])
        .take(num_output_buffers)
        .collect();

    // Start streaming.
    streamon(&mut fd, output_queue).expect("Failed to start output queue");
    streamon(&mut fd, capture_queue).expect("Failed to start capture queue");

    let mut cpt = 0usize;
    let mut total_size = 0usize;
    // Encode generated frames until Ctrl+c is pressed.
    while !lets_quit.load(Ordering::SeqCst) {
        let output_buffer_index = cpt % num_output_buffers;
        let capture_buffer_index = cpt % num_output_buffers;
        let output_buffer = &mut output_buffers[output_buffer_index];

        // Generate the frame data.
        framegen::gen_pattern(
            &mut output_buffer[..],
            output_image_bytesperline,
            cpt as u32,
        );

        // Queue the work to be encoded.
        let out_qbuf = QBuffer::<UserPtrHandle> {
            planes: vec![QBufPlane::new(
                // Safe because we are keeping output_buffer until we dequeue.
                unsafe { UserPtrHandle::new(&output_buffer) },
                output_buffer.len(),
            )],
            ..Default::default()
        };
        qbuf(&fd, output_queue, output_buffer_index, out_qbuf)
            .expect("Error queueing output buffer");

        let cap_qbuf = QBuffer::<MMAPHandle> {
            planes: vec![QBufPlane {
                ..Default::default()
            }],
            ..Default::default()
        };
        qbuf(&fd, capture_queue, capture_buffer_index, cap_qbuf)
            .expect("Error queueing capture buffer");

        // Now dequeue the work that we just scheduled.

        // We can disregard the OUTPUT buffer since it does not contain any
        // useful data for us.
        dqbuf::<(), _>(&fd, output_queue).expect("Failed to dequeue output buffer");

        // The CAPTURE buffer, on the other hand, we want to examine more closely.
        let cap_dqbuf: DQBuffer =
            dqbuf(&fd, capture_queue).expect("Failed to dequeue capture buffer");

        total_size = total_size.wrapping_add(cap_dqbuf.planes[0].bytesused as usize);
        print!(
            "\rEncoded buffer {:#5}, index: {:#2}), bytes used:{:#6} total encoded size:{:#8}",
            cap_dqbuf.sequence, cap_dqbuf.index, cap_dqbuf.planes[0].bytesused, total_size
        );
        io::stdout().flush().unwrap();

        cpt = cpt.wrapping_add(1);
    }

    // Stop streaming.
    streamoff(&mut fd, capture_queue).expect("Failed to stop capture queue");
    streamoff(&mut fd, output_queue).expect("Failed to stop output queue");

    // Free the buffers.
    reqbufs::<(), _>(&mut fd, capture_queue, MemoryType::MMAP, 0)
        .expect("Failed to release capture buffers");
    reqbufs::<(), _>(&mut fd, output_queue, MemoryType::UserPtr, 0)
        .expect("Failed to release output buffers");

    // The fd will be closed as the File instance gets out of scope.
}
