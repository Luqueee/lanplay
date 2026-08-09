//! Per-frame timing for the whole pipeline.
//!
//! The design constraint is that measuring must not perturb what is measured:
//! producer threads only read a monotonic clock and push a 24-byte record into
//! a lock-free queue. Assembling timelines, computing percentiles, and any
//! logging happen on a dedicated collector thread.
//!
//! A frame's life is tiled into named [`Segment`]s, each measured between two
//! real marks. Time the segments do not cover is reported as
//! [`FrameTimeline::unattributed_gap`] and is a debt to be paid with more
//! instrumentation; no metric here is defined as a residue of the others.
//!
//! ```no_run
//! use lanplay_protocol::FrameIdSource;
//! use lanplay_telemetry::{Stage, Telemetry, TelemetryConfig};
//!
//! let telemetry = Telemetry::start(TelemetryConfig::default());
//! let recorder = telemetry.recorder();
//! let frames = FrameIdSource::new();
//!
//! let frame = frames.next();
//! recorder.mark(frame, Stage::FrameCreated);
//! // ... pipeline runs ...
//! recorder.mark(frame, Stage::PresentSubmit);
//!
//! let snapshot = telemetry.shutdown();
//! println!("{snapshot}");
//! ```

mod clock;
mod collector;
mod memory;
mod recorder;
mod stage;
mod stats;
mod timeline;
mod trend;

pub use clock::{ClockDomain, Nanos, Timestamp, wait_until};
pub use collector::{Reporter, Telemetry, TelemetryConfig};
pub use memory::resident_bytes;
pub use recorder::Recorder;
pub use stage::{STAGE_COUNT, Stage};
pub use stats::{Counters, P99_SOAK_FRAMES, Percentiles, Snapshot, Window};
pub use timeline::{
    FRAME_AGE, FrameTimeline, Mark, SEGMENT_COUNT, Segment, SegmentKind, SegmentSample, Segments,
};
pub use trend::Trend;
