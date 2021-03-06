//! Safe wrapper for the `VIDIOC_REQBUFS` ioctl.
use crate::bindings;
use crate::memory::MemoryType;
use crate::QueueType;
use crate::Result;
use bitflags::bitflags;
use std::mem;
use std::os::unix::io::AsRawFd;

/// Implementors can receive the result from the `reqbufs` ioctl.
pub trait ReqBufs {
    fn from(reqbufs: bindings::v4l2_requestbuffers) -> Self;
}

bitflags! {
    /// Flags returned by the `VIDIOC_REQBUFS` ioctl into the `capabilities`
    /// field of `struct v4l2_requestbuffers`.
    pub struct BufferCapabilities: u32 {
        const SUPPORTS_MMAP = bindings::V4L2_BUF_CAP_SUPPORTS_MMAP;
        const SUPPORTS_USERPTR = bindings::V4L2_BUF_CAP_SUPPORTS_USERPTR;
        const SUPPORTS_DMABUF = bindings::V4L2_BUF_CAP_SUPPORTS_DMABUF;
        const SUPPORTS_REQUESTS = bindings::V4L2_BUF_CAP_SUPPORTS_REQUESTS;
        const SUPPORTS_ORPHANED_BUFS = bindings::V4L2_BUF_CAP_SUPPORTS_ORPHANED_BUFS;
        //const SUPPORTS_M2M_HOLD_CAPTURE_BUF = bindings::V4L2_BUF_CAP_SUPPORTS_M2M_HOLD_CAPTURE_BUF;
    }
}

impl ReqBufs for () {
    fn from(_reqbufs: bindings::v4l2_requestbuffers) -> Self {
        ()
    }
}

/// In case we are just interested in the number of buffers that `reqbufs`
/// created.
impl ReqBufs for usize {
    fn from(reqbufs: bindings::v4l2_requestbuffers) -> Self {
        reqbufs.count as usize
    }
}

/// If we just want to query the buffer capabilities.
impl ReqBufs for BufferCapabilities {
    fn from(reqbufs: bindings::v4l2_requestbuffers) -> Self {
        BufferCapabilities::from_bits_truncate(reqbufs.capabilities)
    }
}

/// Full result of the `reqbufs` ioctl.
pub struct RequestBuffers {
    pub count: u32,
    pub capabilities: BufferCapabilities,
}

impl ReqBufs for RequestBuffers {
    fn from(reqbufs: bindings::v4l2_requestbuffers) -> Self {
        RequestBuffers {
            count: reqbufs.count,
            capabilities: BufferCapabilities::from_bits_truncate(reqbufs.capabilities),
        }
    }
}

#[doc(hidden)]
mod ioctl {
    use crate::bindings::v4l2_requestbuffers;
    nix::ioctl_readwrite!(vidioc_reqbufs, b'V', 8, v4l2_requestbuffers);
}

/// Safe wrapper around the `VIDIOC_REQBUFS` ioctl.
pub fn reqbufs<T: ReqBufs, F: AsRawFd>(
    fd: &mut F,
    queue: QueueType,
    memory: MemoryType,
    count: u32,
) -> Result<T> {
    let mut reqbufs = bindings::v4l2_requestbuffers {
        count,
        type_: queue as u32,
        memory: memory as u32,
        ..unsafe { mem::zeroed() }
    };
    unsafe { ioctl::vidioc_reqbufs(fd.as_raw_fd(), &mut reqbufs) }?;

    Ok(T::from(reqbufs))
}
