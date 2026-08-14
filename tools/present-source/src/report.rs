//! What the run achieved, in the terms the capture comparison needs.
//!
//! One line per statistic and nothing else on stdout: a capture run pipes this
//! straight into its own report, and a table would have to be parsed back
//! apart to get there.

use core::fmt;

use lanplay_telemetry::{Nanos, Snapshot};

use crate::pace::PhaseShifts;

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
    /// What the viewer asked of the schedule and what became of it. Requests
    /// and applications are both reported because they differ: one that folded
    /// to nothing, or that a newer one displaced, arrived and moved nothing.
    pub phase: PhaseShifts,
}

impl Report {
    /// `missed` is counted by the present loop; the rest comes from the
    /// telemetry collector, which owns the histograms for the whole project.
    pub fn from_snapshot(
        snapshot: &Snapshot,
        requested_fps: u32,
        missed: u64,
        phase: PhaseShifts,
    ) -> Report {
        Report {
            frames_presented: snapshot.counters.frames_presented,
            requested_fps,
            achieved_fps: snapshot.presented_per_second(),
            interval_p50: snapshot.present_interval.p50,
            interval_p99: snapshot.present_interval.p99,
            interval_max: snapshot.present_interval.max,
            missed_deadlines: missed,
            phase,
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
        writeln!(f, "missed deadlines: {}", self.missed_deadlines)?;
        writeln!(f, "phase requests: {}", self.phase.requested)?;
        writeln!(f, "phase shifts applied: {}", self.phase.applied)?;
        writeln!(f, "phase moved ms: {:.3}", self.phase.moved.as_millis_f64())?;
        // Non-zero says the asker's period and this producer's disagree: a
        // delay is only ever a fraction of a period, so one that reached a
        // whole one was computed against a different rate. The remainder that
        // survives the fold is then arithmetically sound and physically
        // meaningless, which nothing else in this report would reveal.
        writeln!(f, "phase requests folded: {}", self.phase.folded)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The report is parsed by whatever runs the producer, so its shape is a
    /// contract: eleven lines, in this order, `name: value`.
    #[test]
    fn renders_one_labelled_line_per_statistic() {
        let report = Report {
            frames_presented: 14_400,
            requested_fps: 120,
            achieved_fps: 119.9713,
            interval_p50: Nanos(8_335_000),
            interval_p99: Nanos(9_119_000),
            interval_max: Nanos(15_402_000),
            missed_deadlines: 3,
            phase: PhaseShifts {
                requested: 12,
                superseded: 0,
                taken: 12,
                applied: 11,
                folded: 0,
                moved: Nanos(45_832_000),
            },
        };

        assert_eq!(
            report.to_string(),
            "frames presented: 14400\n\
             requested fps: 120\n\
             achieved fps: 119.97\n\
             present interval p50 ms: 8.335\n\
             present interval p99 ms: 9.119\n\
             present interval max ms: 15.402\n\
             missed deadlines: 3\n\
             phase requests: 12\n\
             phase shifts applied: 11\n\
             phase moved ms: 45.832\n\
             phase requests folded: 0\n"
        );
    }

    /// A request that arrived and moved nothing is not the same as no request,
    /// and the report has to be able to say so.
    #[test]
    fn a_request_that_did_nothing_is_visible_against_the_ones_that_did() {
        let report = Report {
            frames_presented: 0,
            requested_fps: 120,
            achieved_fps: 0.0,
            interval_p50: Nanos::ZERO,
            interval_p99: Nanos::ZERO,
            interval_max: Nanos::ZERO,
            missed_deadlines: 0,
            phase: PhaseShifts {
                requested: 3,
                superseded: 1,
                taken: 2,
                applied: 0,
                folded: 2,
                moved: Nanos::ZERO,
            },
        };

        let text = report.to_string();
        assert!(text.contains("phase requests: 3"), "{text}");
        assert!(text.contains("phase shifts applied: 0"), "{text}");
        assert!(text.contains("phase moved ms: 0.000"), "{text}");
        // The only line that says the asker was working from a different
        // period than this producer.
        assert!(text.contains("phase requests folded: 2"), "{text}");
    }
}
