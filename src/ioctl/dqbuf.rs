use super::{is_multi_planar, BufferFlags, PlaneData};
use crate::bindings;
use crate::QueueType;
use crate::Result;

use std::mem;
use std::os::unix::io::AsRawFd;

/// Implementors can receive the result from the `dqbuf` ioctl.
pub trait DQBuf: Sized {
    /// Try to retrieve the data from `v4l2_buf`. If `v4l2_planes` is `None`,
    /// then the buffer is single-planar. If it has data, the buffer is
    /// multi-planar and the array of `struct v4l2_plane` shall be used to
    /// retrieve the plane data.
    fn from_v4l2_buffer(
        v4l2_buf: &bindings::v4l2_buffer,
        v4l2_planes: Option<&PlaneData>,
    ) -> Result<Self>;
}

/// Allows to dequeue a buffer without caring for any of its data.
impl DQBuf for () {
    fn from_v4l2_buffer(
        _v4l2_buf: &bindings::v4l2_buffer,
        _v4l2_planes: Option<&PlaneData>,
    ) -> Result<Self> {
        Ok(())
    }
}

/// Useful for the case where we are only interested in the index of a dequeued
/// buffer
impl DQBuf for u32 {
    fn from_v4l2_buffer(
        v4l2_buf: &bindings::v4l2_buffer,
        _v4l2_planes: Option<&PlaneData>,
    ) -> Result<Self> {
        Ok(v4l2_buf.index)
    }
}

#[derive(Debug)]
pub struct DQBufPlane {
    pub length: u32,
    pub bytesused: u32,
    pub data_offset: u32,
}

/// Contains all the information from a dequeued buffer. Safe variant of
/// `struct v4l2_buffer`.
#[derive(Debug, Default)]
pub struct DQBuffer {
    pub index: u32,
    pub flags: BufferFlags,
    pub field: u32,
    pub sequence: u32,
    pub planes: Vec<DQBufPlane>,
}

impl DQBuf for DQBuffer {
    fn from_v4l2_buffer(
        v4l2_buf: &bindings::v4l2_buffer,
        v4l2_planes: Option<&PlaneData>,
    ) -> Result<Self> {
        let planes = match v4l2_planes {
            None => vec![DQBufPlane {
                length: v4l2_buf.length,
                bytesused: v4l2_buf.bytesused,
                data_offset: 0,
            }],
            Some(v4l2_planes) => v4l2_planes
                .iter()
                .take(v4l2_buf.length as usize)
                .map(|v4l2_plane| DQBufPlane {
                    length: v4l2_plane.length,
                    bytesused: v4l2_plane.bytesused,
                    data_offset: v4l2_plane.data_offset,
                })
                .collect(),
        };

        Ok(DQBuffer {
            index: v4l2_buf.index as u32,
            flags: BufferFlags::from_bits_truncate(v4l2_buf.flags),
            field: v4l2_buf.field,
            sequence: v4l2_buf.sequence,
            planes,
        })
    }
}

#[doc(hidden)]
mod ioctl {
    use crate::bindings::v4l2_buffer;
    nix::ioctl_readwrite!(vidioc_dqbuf, b'V', 17, v4l2_buffer);
}

/// Safe wrapper around the `VIDIOC_DQBUF` ioctl.
pub fn dqbuf<T: DQBuf, F: AsRawFd>(fd: &F, queue: QueueType) -> Result<T> {
    let mut v4l2_buf = bindings::v4l2_buffer {
        type_: queue as u32,
        ..unsafe { mem::zeroed() }
    };

    if is_multi_planar(queue) {
        let mut plane_data: PlaneData = Default::default();
        v4l2_buf.m.planes = plane_data.as_mut_ptr();
        v4l2_buf.length = plane_data.len() as u32;

        unsafe { ioctl::vidioc_dqbuf(fd.as_raw_fd(), &mut v4l2_buf) }?;
        Ok(T::from_v4l2_buffer(&v4l2_buf, Some(&plane_data))?)
    } else {
        unsafe { ioctl::vidioc_dqbuf(fd.as_raw_fd(), &mut v4l2_buf) }?;
        Ok(T::from_v4l2_buffer(&v4l2_buf, None)?)
    }
}
