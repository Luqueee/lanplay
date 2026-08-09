use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use crossbeam_queue::ArrayQueue;
use lanplay_protocol::FrameId;

use crate::clock::Timestamp;
use crate::stage::Stage;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Event {
    pub frame: FrameId,
    pub stage: Stage,
    pub at: Timestamp,
}

pub(crate) struct Channel {
    pub queue: ArrayQueue<Event>,
    pub dropped: AtomicU64,
}

/// The hot-path handle. Clone it into every thread that touches a frame.
///
/// A mark is one clock read plus one lock-free push: no allocation, no
/// formatting, no syscall, no blocking. When the queue is full the event is
/// dropped and counted, because stalling a capture or encode thread to keep a
/// measurement would corrupt the very thing being measured.
#[derive(Clone)]
pub struct Recorder {
    channel: Arc<Channel>,
}

impl Recorder {
    pub(crate) fn new(channel: Arc<Channel>) -> Self {
        Recorder { channel }
    }

    #[inline]
    pub fn mark(&self, frame: FrameId, stage: Stage) {
        self.mark_at(frame, stage, Timestamp::now());
    }

    /// Records a mark with a timestamp taken earlier, for stages whose real
    /// time is known only after the fact (a GPU query result, a completion
    /// callback that reports its own submit time).
    #[inline]
    pub fn mark_at(&self, frame: FrameId, stage: Stage, at: Timestamp) {
        let event = Event { frame, stage, at };
        if self.channel.queue.push(event).is_err() {
            self.channel.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl core::fmt::Debug for Recorder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Recorder")
            .field("queued", &self.channel.queue.len())
            .field("capacity", &self.channel.queue.capacity())
            .finish()
    }
}
