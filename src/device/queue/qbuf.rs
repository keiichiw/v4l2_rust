//! Provides types related to queuing buffers on a `Queue` object.
use super::{BufferState, BufferStateFuse, BuffersAllocated, PlaneHandles, Queue};
use super::{Capture, Direction, Output};
use crate::ioctl;
use crate::memory::*;
use crate::Error;
use std::cmp::Ordering;
use std::fmt::{self, Debug, Display};

/// Error that can occur when queuing a buffer. It wraps a regular error and also
/// returns the plane handles back to the user.
pub struct QueueError<M: Memory> {
    pub error: Error,
    pub plane_handles: PlaneHandles<M>,
}

impl<M: Memory> Display for QueueError<M> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Display::fmt(&self.error, f)
    }
}

impl<M: Memory> Debug for QueueError<M> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        Debug::fmt(&self.error, f)
    }
}

impl<M: Memory> std::error::Error for QueueError<M> {}

#[allow(type_alias_bounds)]
pub type QueueResult<M: Memory, R> = std::result::Result<R, QueueError<M>>;

/// A free buffer that has just been obtained from `Queue::get_buffer()` and
/// which is being prepared to the queued.
///
/// The necessary setup depends on the kind of direction of the buffer:
///
/// * Capture buffers are to be filled by the driver, so we just need to attach
///   one memory handle per plane before submitting them (MMAP buffers don't
///   need this step).
/// * Output buffers on the other hand are filled by us ; so on top of one valid
///   memory handle per plane, we also need to specify how much data we have
///   written in each of them, and possibly set a few flags on the buffer.
///
/// This struct is specialized on both the direction and type of memory so
/// mandatory data is always specified, and irrelevant data is inaccessible.
///
/// Once a buffer is ready, it can be queued using the queue() method. Failures
/// occur if the QBUF ioctl failed, or if the number of specified planes does
/// not match the number of planes in the format. A queued buffer remains
/// inaccessible for further queuing until it has been dequeued and dropped.
///
/// If a QBuffer object is destroyed before being queued, its buffer returns
/// to the pool of available buffers and can be requested again with
/// `Queue::get_buffer()`.
///
/// A QBuffer holds a strong reference to its queue, therefore the state of the
/// queue or device cannot be changed while it is being used. Contrary to
/// DQBuffer which can be freely duplicated and passed around, instances of this
/// struct are supposed to be short-lived.
pub struct QBuffer<'a, D: Direction, M: Memory> {
    queue: &'a Queue<D, BuffersAllocated<M>>,
    index: usize,
    num_planes: usize,
    qbuffer: ioctl::QBuffer<M::HandleType>,
    plane_handles: PlaneHandles<M>,
    fuse: BufferStateFuse<M>,
}

impl<'a, D: Direction, M: Memory> QBuffer<'a, D, M> {
    pub(super) fn new(
        queue: &'a Queue<D, BuffersAllocated<M>>,
        index: usize,
        num_planes: usize,
        fuse: BufferStateFuse<M>,
    ) -> Self {
        QBuffer {
            queue,
            index,
            num_planes,
            qbuffer: Default::default(),
            plane_handles: Vec::new(),
            fuse,
        }
    }

    /// Returns the V4L2 index of this buffer.
    pub fn index(&self) -> usize {
        self.index
    }

    /// Returns the number of planes expected to be specified before this buffer
    /// can be queued.
    pub fn num_expected_planes(&self) -> usize {
        self.num_planes
    }

    /// Returns the number of planes that have been specified so far.
    pub fn num_set_planes(&self) -> usize {
        self.qbuffer.planes.len()
    }

    /// Specify the next plane of this buffer.
    pub fn add_plane(mut self, plane: Plane<D, M>) -> Self {
        self.qbuffer.planes.push(plane.plane);
        self.plane_handles.push(M::build_dqbuftype(plane.backing));
        self
    }

    /// Queue the buffer. The QBuffer object is consumed and the buffer won't
    /// be available again until it has been dequeued and dropped, or a
    /// `streamoff()` is performed.
    pub fn queue(mut self) -> QueueResult<M, ()> {
        let plane_handles = self.plane_handles;

        // First check that the number of provided planes is what we expect.
        match self.qbuffer.planes.len().cmp(&self.num_planes) {
            Ordering::Less => {
                return Err(QueueError {
                    error: Error::NotEnoughPlanes,
                    plane_handles,
                })
            }
            Ordering::Greater => {
                return Err(QueueError {
                    error: Error::TooManyPlanes,
                    plane_handles,
                })
            }
            Ordering::Equal => (),
        };

        match ioctl::qbuf(
            &self.queue.inner,
            self.queue.inner.type_,
            self.index,
            self.qbuffer,
        ) {
            Ok(_) => (),
            Err(error) => {
                return Err(QueueError {
                    error,
                    plane_handles,
                })
            }
        };

        // We got this now.
        self.fuse.disarm();

        let mut buffers_state = self.queue.state.buffers_state.lock().unwrap();
        std::mem::replace(&mut buffers_state.buffers_state[self.index], BufferState::Queued(plane_handles));
        // TODO this indicates that we should probably use treemaps for each buffer state
        // (or bitmaps for simple state and a treemap for the queued one) instead of a global
        // array?
        buffers_state.num_queued_buffers += 1;
        drop(buffers_state);

        Ok(())
    }
}

impl<'a> QBuffer<'a, Capture, MMAP> {
    /// For Capture MMAP buffers, there is no point requesting the user to
    /// provide as many empty handles as there are planes in the buffer. This
    /// methods allows to queue them as soon as they are obtained.
    pub fn auto_queue(mut self) -> QueueResult<MMAP, ()> {
        while self.num_set_planes() < self.num_expected_planes() {
            self = self.add_plane(Plane::<Capture, MMAP>::cap(().into()));
        }
        self.queue()
    }
}

/// Used to build plane information for a buffer about to be queued. This
/// struct is specialized on direction and buffer type to only the relevant
/// data can be set according to the current context.
pub struct Plane<D: Direction, M: Memory> {
    backing: M::QBufType,
    plane: ioctl::QBufPlane<M::HandleType>,
    _d: std::marker::PhantomData<D>,
}

impl<M: Memory> Plane<Capture, M> {
    /// Creates a new plane builder suitable for a capture queue.
    /// Mandatory information is just a valid memory handle for the driver to
    /// write into.
    pub fn cap(backing: M::QBufType) -> Self {
        // Safe because we are storing `backing` at least until the buffer is
        // dequeued.
        let handle = unsafe { M::build_handle(&backing) };

        Self {
            backing,
            plane: ioctl::QBufPlane {
                bytesused: 0,
                data_offset: 0,
                handle,
            },
            _d: std::marker::PhantomData,
        }
    }
}

impl<M: Memory> Plane<Output, M> {
    /// Creates a new plane builder suitable for an output queue.
    /// Mandatory information include a memory handle, and the number of bytes
    /// used within it.
    pub fn out(backing: M::QBufType, bytes_used: usize) -> Self {
        // Safe because we are storing `backing` at least until the buffer is
        // dequeued.
        let handle = unsafe { M::build_handle(&backing) };

        Self {
            backing,
            plane: ioctl::QBufPlane {
                bytesused: bytes_used as u32,
                data_offset: 0,
                handle,
            },
            _d: std::marker::PhantomData,
        }
    }

    /// Set the data offset in the handle at which the actual data starts.
    ///
    /// This parameter is valid only when using the multi-planar API.
    pub fn set_data_offset(mut self, data_offset: usize) -> Self {
        self.plane.data_offset = data_offset as u32;
        self
    }
}
