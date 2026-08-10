use std::collections::VecDeque;
use std::mem;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crossbeam_queue::ArrayQueue;
use lanplay_protocol::FrameId;
use parking_lot::{Condvar, Mutex};

use crate::clock::{ClockDomain, Nanos, Timestamp};
use crate::recorder::{Channel, Event, Recorder};
use crate::stage::Stage;
use crate::stats::{Counters, Histograms, Percentiles, Snapshot, Window};
use crate::timeline::{FrameTimeline, Mark, Segment};

/// Called on the collector thread whenever a periodic report is due.
///
/// This is the sanctioned place to log: it runs off the hot path, on a thread
/// that owns no frame resources, with a consistent snapshot in hand.
pub type Reporter = Box<dyn FnMut(&Snapshot) + Send>;

pub struct TelemetryConfig {
    /// Marks that can be in flight before the recorder starts dropping.
    pub queue_capacity: usize,
    /// Frames the collector can assemble concurrently. At 120 fps, 256 slots
    /// is roughly two seconds of tolerance for a frame that never presents.
    pub ring_slots: usize,
    /// Completed timelines kept for per-frame reports.
    pub recent_frames: usize,
    /// How long the collector sleeps when the queue is empty.
    pub poll_interval: Duration,
    /// How often `reporter` is invoked, if at all.
    pub report_interval: Option<Duration>,
    pub reporter: Option<Reporter>,
    /// Clock the local recorder stamps its marks with.
    pub clock_domain: ClockDomain,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        TelemetryConfig {
            queue_capacity: 8192,
            ring_slots: 256,
            recent_frames: 256,
            poll_interval: Duration::from_micros(250),
            report_interval: None,
            reporter: None,
            clock_domain: ClockDomain::local(),
        }
    }
}

/// Owns the collector thread. Hand [`Telemetry::recorder`] to producers and
/// keep this alive for the length of the session.
pub struct Telemetry {
    recorder: Recorder,
    channel: Arc<Channel>,
    shared: Arc<Shared>,
    handle: Option<JoinHandle<()>>,
}

struct Shared {
    clock_domain: ClockDomain,
    inner: Mutex<Inner>,
    running: AtomicBool,
    folds: AtomicU64,
    /// Guards the collector's idle wait so that a shutdown between the
    /// `running` check and the wait cannot be missed.
    idle: Mutex<()>,
    wake: Condvar,
}

struct Inner {
    histograms: Histograms,
    counters: Counters,
    recent: VecDeque<FrameTimeline>,
    recent_capacity: usize,
    first_present: Option<Timestamp>,
    last_present: Option<Timestamp>,
    last_source: Option<Timestamp>,
    /// When the current rolling window started.
    window_opened: Option<Timestamp>,
}

/// Timelines live inline in the ring on purpose: 256 slots is ~73 KB held for
/// the whole session, where boxing would trade that for an allocation and a
/// free per frame on the collector thread.
#[allow(clippy::large_enum_variant)]
#[derive(Clone)]
enum Slot {
    Empty,
    Collecting(FrameTimeline),
    Finalized(FrameId),
}

#[derive(Default)]
struct Pending {
    started: u64,
    duplicates: u64,
    late: u64,
    events: u64,
}

impl Telemetry {
    pub fn start(config: TelemetryConfig) -> Telemetry {
        let TelemetryConfig {
            queue_capacity,
            ring_slots,
            recent_frames,
            poll_interval,
            report_interval,
            reporter,
            clock_domain,
        } = config;

        let channel = Arc::new(Channel {
            queue: ArrayQueue::new(queue_capacity.max(1)),
            dropped: AtomicU64::new(0),
        });
        let shared = Arc::new(Shared {
            clock_domain,
            inner: Mutex::new(Inner {
                histograms: Histograms::new(),
                counters: Counters::default(),
                recent: VecDeque::with_capacity(recent_frames.max(1)),
                recent_capacity: recent_frames.max(1),
                first_present: None,
                last_present: None,
                last_source: None,
                window_opened: Some(Timestamp::now()),
            }),
            running: AtomicBool::new(true),
            folds: AtomicU64::new(0),
            idle: Mutex::new(()),
            wake: Condvar::new(),
        });

        let thread_channel = Arc::clone(&channel);
        let thread_shared = Arc::clone(&shared);
        let handle = thread::Builder::new()
            .name("telemetry".into())
            .spawn(move || {
                collect(
                    thread_channel,
                    thread_shared,
                    ring_slots.max(1),
                    poll_interval,
                    report_interval,
                    reporter,
                )
            })
            .expect("spawn telemetry collector");

        Telemetry {
            recorder: Recorder::new(Arc::clone(&channel), clock_domain),
            channel,
            shared,
            handle: Some(handle),
        }
    }

    /// Cheap clone for producer threads.
    pub fn recorder(&self) -> Recorder {
        self.recorder.clone()
    }

    pub fn snapshot(&self) -> Snapshot {
        self.shared.snapshot(self.dropped())
    }

    /// Percentiles over the frames finalised since the previous call, then
    /// starts a fresh window. Cumulative statistics are untouched.
    ///
    /// A ten-minute run whose middle ten seconds collapsed still reports a
    /// healthy cumulative p99, because percentiles cannot be differenced.
    /// Sampling this on a timer is the only way to see the collapse.
    pub fn take_window(&self) -> Window {
        let mut inner = self.shared.inner.lock();
        let now = Timestamp::now();
        let span = inner
            .window_opened
            .map_or(Nanos::ZERO, |opened| now.saturating_since(opened));
        inner.window_opened = Some(now);

        let window = &inner.histograms.window;
        let taken = Window {
            local_age: Percentiles::from_histogram("local age", &window.local_age),
            present_interval: Percentiles::from_histogram(
                "present interval",
                &window.present_interval,
            ),
            source_interval: Percentiles::from_histogram(
                "source interval",
                &window.source_interval,
            ),
            presented: window.presented,
            span,
        };
        inner.histograms.window.reset();
        taken
    }

    /// Most recently finalised frame.
    pub fn last_frame(&self) -> Option<FrameTimeline> {
        self.shared.inner.lock().recent.back().cloned()
    }

    pub fn frame(&self, frame: FrameId) -> Option<FrameTimeline> {
        self.shared
            .inner
            .lock()
            .recent
            .iter()
            .find(|timeline| timeline.frame() == frame)
            .cloned()
    }

    pub fn recent_frames(&self) -> Vec<FrameTimeline> {
        self.shared.inner.lock().recent.iter().cloned().collect()
    }

    /// Blocks until every mark recorded so far has been folded into the stats.
    /// Frames still missing `present_submit` stay in flight; they are only
    /// counted once presented or evicted.
    ///
    /// Returns false if the collector did not catch up within `timeout`.
    pub fn flush(&self, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        let target = self.shared.folds.load(Ordering::Acquire) + 2;
        loop {
            if self.channel.queue.is_empty() && self.shared.folds.load(Ordering::Acquire) >= target
            {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(Duration::from_micros(200));
        }
    }

    /// Stops the collector, folds everything still in flight, and returns the
    /// final snapshot. Frames that never presented are counted as incomplete.
    pub fn shutdown(mut self) -> Snapshot {
        self.stop();
        self.shared.snapshot(self.dropped())
    }

    fn dropped(&self) -> u64 {
        self.channel.dropped.load(Ordering::Relaxed)
    }

    fn stop(&mut self) {
        if let Some(handle) = self.handle.take() {
            {
                // Held so a collector about to sleep observes the store.
                let _idle = self.shared.idle.lock();
                self.shared.running.store(false, Ordering::Release);
            }
            self.shared.wake.notify_all();
            let _ = handle.join();
        }
    }
}

impl Drop for Telemetry {
    fn drop(&mut self) {
        self.stop();
    }
}

impl Shared {
    fn snapshot(&self, dropped: u64) -> Snapshot {
        let inner = self.inner.lock();
        let mut counters = inner.counters;
        counters.events_dropped = dropped;
        counters.clipped_samples = inner.histograms.clipped;

        let window = match (inner.first_present, inner.last_present) {
            (Some(first), Some(last)) => last.saturating_since(first),
            _ => Nanos::ZERO,
        };

        Snapshot {
            clock_domain: self.clock_domain,
            segments: Segment::ALL
                .iter()
                .zip(&inner.histograms.segments)
                .map(|(segment, histogram)| Percentiles::from_histogram(segment.label(), histogram))
                .collect(),
            frame_age: Percentiles::from_histogram("frame age", &inner.histograms.frame_age),
            unattributed_gap: Percentiles::from_histogram(
                "unattributed gap",
                &inner.histograms.unattributed_gap,
            ),
            present_interval: Percentiles::from_histogram(
                "present interval",
                &inner.histograms.present_interval,
            ),
            local_age: Percentiles::from_histogram("local age", &inner.histograms.local_age),
            source_interval: Percentiles::from_histogram(
                "source interval",
                &inner.histograms.source_interval,
            ),
            counters,
            window,
        }
    }
}

fn collect(
    channel: Arc<Channel>,
    shared: Arc<Shared>,
    ring_slots: usize,
    poll_interval: Duration,
    report_interval: Option<Duration>,
    mut reporter: Option<Reporter>,
) {
    let mut slots = vec![Slot::Empty; ring_slots];
    let mut completed: Vec<FrameTimeline> = Vec::with_capacity(64);
    let mut pending = Pending::default();
    let mut next_report = report_interval.map(|interval| Instant::now() + interval);

    while shared.running.load(Ordering::Acquire) {
        let drained = drain(&channel, &mut slots, &mut completed, &mut pending);
        fold(&shared, &mut completed, &mut pending);

        if let (Some(due), Some(reporter)) = (next_report.as_mut(), reporter.as_mut())
            && Instant::now() >= *due
        {
            let snapshot = shared.snapshot(channel.dropped.load(Ordering::Relaxed));
            reporter(&snapshot);
            *due += report_interval.expect("interval present when due is set");
        }

        if drained == 0 {
            let mut idle = shared.idle.lock();
            if shared.running.load(Ordering::Acquire) {
                shared.wake.wait_for(&mut idle, poll_interval);
            }
        }
    }

    // Final pass: whatever is still queued, plus every frame stuck mid-flight.
    drain(&channel, &mut slots, &mut completed, &mut pending);
    for slot in &mut slots {
        if let Slot::Collecting(timeline) = mem::replace(slot, Slot::Empty)
            && !timeline.is_empty()
        {
            completed.push(timeline);
        }
    }
    fold(&shared, &mut completed, &mut pending);
}

/// Moves every queued event into the slot ring. Returns how many were drained.
fn drain(
    channel: &Channel,
    slots: &mut [Slot],
    completed: &mut Vec<FrameTimeline>,
    pending: &mut Pending,
) -> u64 {
    let mut drained = 0;
    while let Some(event) = channel.queue.pop() {
        ingest(event, slots, completed, pending);
        drained += 1;
    }
    pending.events += drained;
    drained
}

fn ingest(
    event: Event,
    slots: &mut [Slot],
    completed: &mut Vec<FrameTimeline>,
    pending: &mut Pending,
) {
    let index = (event.frame.get() % slots.len() as u64) as usize;
    let mark = Mark {
        at: event.at,
        domain: event.domain,
    };

    let is_new_frame = match &mut slots[index] {
        Slot::Collecting(timeline) if timeline.frame() == event.frame => {
            if !timeline.set(event.stage, mark) {
                pending.duplicates += 1;
            }
            false
        }
        Slot::Finalized(frame) if *frame == event.frame => {
            pending.late += 1;
            return;
        }
        _ => true,
    };

    if is_new_frame {
        // The slot belonged to an older frame: that frame never presented, so
        // retire it as incomplete rather than letting it linger.
        if let Slot::Collecting(previous) = mem::replace(&mut slots[index], Slot::Empty) {
            completed.push(previous);
        }
        let mut timeline = FrameTimeline::new(event.frame);
        timeline.set(event.stage, mark);
        pending.started += 1;
        slots[index] = Slot::Collecting(timeline);
    }

    if event.stage == Stage::PresentSubmit
        && let Slot::Collecting(timeline) =
            mem::replace(&mut slots[index], Slot::Finalized(event.frame))
    {
        completed.push(timeline);
    }
}

fn fold(shared: &Shared, completed: &mut Vec<FrameTimeline>, pending: &mut Pending) {
    if completed.is_empty() && pending.events == 0 {
        shared.folds.fetch_add(1, Ordering::Release);
        return;
    }

    let mut guard = shared.inner.lock();
    let inner = &mut *guard;

    inner.counters.events_recorded += mem::take(&mut pending.events);
    inner.counters.frames_started += mem::take(&mut pending.started);
    inner.counters.duplicate_marks += mem::take(&mut pending.duplicates);
    inner.counters.late_events += mem::take(&mut pending.late);

    for timeline in completed.drain(..) {
        if timeline.is_complete() {
            inner.counters.frames_presented += 1;
            inner.histograms.window.presented += 1;
        } else {
            inner.counters.frames_incomplete += 1;
        }

        let histograms = &mut inner.histograms;
        for sample in timeline.segments().chain(timeline.diagnostics()) {
            Histograms::record(
                &mut histograms.segments[sample.segment.index()],
                sample.duration,
                &mut histograms.clipped,
            );
            if sample.cross_domain {
                inner.counters.cross_domain_segments += 1;
            }
        }
        if let Some(age) = timeline.frame_age() {
            Histograms::record(&mut histograms.frame_age, age, &mut histograms.clipped);
        }
        if let Some(age) = timeline.local_age(shared.clock_domain) {
            Histograms::record(&mut histograms.local_age, age, &mut histograms.clipped);
            Histograms::record(
                &mut histograms.window.local_age,
                age,
                &mut histograms.clipped,
            );
        }
        if let Some(gap) = timeline.unattributed_gap(shared.clock_domain) {
            Histograms::record(
                &mut histograms.unattributed_gap,
                gap,
                &mut histograms.clipped,
            );
        }

        if let Some(present) = timeline.at(Stage::PresentSubmit) {
            if let Some(previous) = inner.last_present
                && let Some(delta) = present.since(previous)
            {
                Histograms::record(
                    &mut histograms.present_interval,
                    delta,
                    &mut histograms.clipped,
                );
                Histograms::record(
                    &mut histograms.window.present_interval,
                    delta,
                    &mut histograms.clipped,
                );
            }
            inner.first_present.get_or_insert(present);
            inner.last_present = Some(match inner.last_present {
                Some(previous) => previous.max(present),
                None => present,
            });
        }
        // The source of work for this machine is whatever it first saw of the
        // frame: a capture on the host, a datagram on the client.
        if let Some(source) = timeline.first_local(shared.clock_domain) {
            if let Some(previous) = inner.last_source
                && let Some(delta) = source.since(previous)
            {
                Histograms::record(
                    &mut histograms.source_interval,
                    delta,
                    &mut histograms.clipped,
                );
                Histograms::record(
                    &mut histograms.window.source_interval,
                    delta,
                    &mut histograms.clipped,
                );
            }
            inner.last_source = Some(match inner.last_source {
                Some(previous) => previous.max(source),
                None => source,
            });
        }

        if inner.recent.len() == inner.recent_capacity {
            inner.recent.pop_front();
        }
        inner.recent.push_back(timeline);
    }

    drop(guard);
    shared.folds.fetch_add(1, Ordering::Release);
}
