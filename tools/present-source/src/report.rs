//! What the run achieved, in the terms the capture comparison needs.
//!
//! One line per statistic and nothing else on stdout: a capture run pipes this
//! straight into its own report, and a table would have to be parsed back
//! apart to get there.

use core::fmt;

use lanplay_telemetry::{Nanos, Snapshot};

/// The whole result of a run.
pub struct Report {
    pub frames_presented: u64,
    pub requested_fps: u32,
    pub achieved_fps: f64,
    pub interval_p50: Nanos,
    pub interval_p99: Nanos,
    pub interval_max: Nanos,
    /// Frames whose deadline had already passed when the loop reached them.
    /// Non-zero means the producer, not the capturer, is the bottleneck, and
    /// every capture number taken alongside it is suspect.
    pub missed_deadlines: u64,
}

impl Report {
    /// `missed` is counted by the present loop; the rest comes from the
    /// telemetry collector, which owns the histograms for the whole project.
    pub fn from_snapshot(snapshot: &Snapshot, requested_fps: u32, missed: u64) -> Report {
        Report {
            frames_presented: snapshot.counters.frames_presented,
            requested_fps,
            achieved_fps: snapshot.presented_per_second(),
            interval_p50: snapshot.present_interval.p50,
            interval_p99: snapshot.present_interval.p99,
            interval_max: snapshot.present_interval.max,
            missed_deadlines: missed,
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "frames presented: {}", self.frames_presented)?;
        writeln!(f, "requested fps: {}", self.requested_fps)?;
        writeln!(f, "achieved fps: {:.2}", self.achieved_fps)?;
        writeln!(
            f,
            "present interval p50 ms: {:.3}",
            self.interval_p50.as_millis_f64()
        )?;
        writeln!(
            f,
            "present interval p99 ms: {:.3}",
            self.interval_p99.as_millis_f64()
        )?;
        writeln!(
            f,
            "present interval max ms: {:.3}",
            self.interval_max.as_millis_f64()
        )?;
        writeln!(f, "missed deadlines: {}", self.missed_deadlines)
    }
}
