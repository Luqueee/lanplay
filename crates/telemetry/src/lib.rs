//! Per-frame timing for the whole pipeline.
//!
//! The design constraint is that measuring must not perturb what is measured:
//! producer threads only read a monotonic clock and push a 24-byte record into
//! a lock-free queue. Assembling timelines, computing percentiles, and any
//! logging happen on a dedicated collector thread.
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
mod recorder;
mod stage;
mod stats;
mod timeline;

pub use clock::{Nanos, Timestamp};
pub use collector::{Reporter, Telemetry, TelemetryConfig};
pub use recorder::Recorder;
pub use stage::{STAGE_COUNT, Side, Stage};
pub use stats::{Counters, Snapshot, SpanStats};
pub use timeline::{FRAME_AGE, FrameTimeline, SPANS, Span};
