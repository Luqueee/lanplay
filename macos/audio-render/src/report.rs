//! What the run found, in the exact lines the gate reads.
//!
//! A value built once at the end rather than a scatter of prints, for two
//! reasons. The render callback must not format anything — a `println!` takes a
//! lock on stdout and can block on a pipe, which on a real-time thread is an
//! audible click — so every figure has to survive as a number until the device
//! has stopped. And a report that is a value can be checked on a machine with
//! no audio device at all, which is the only way the wording of the refusal
//! below ever gets tested.
//!
//! The keyed lines come first, in a fixed order; what follows them is for a
//! person asking why those numbers rather than for a parser.
//!
//! The one line that matters more than the rest is the last. A run whose
//! callback never fired has measured nothing, and its underrun count is zero
//! for the same reason its callback count is: nothing happened. This project
//! has read that shape as success five times, so it is named here and it
//! carries an exit code.

use core::fmt;

use lanplay_audio_capture::Percentiles;

use crate::format::{Layout, OutputFormat};

/// What the run amounted to, once counted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Verdict {
    /// The device never asked for a buffer. Nothing in the report is evidence
    /// of anything.
    Silent,
    /// Audio was rendered, and some of it was silence this program had to
    /// invent because the ring was empty.
    Underran,
    /// Audio was rendered and the ring was never short.
    Rendered,
}

/// Everything one run establishes.
#[derive(Clone, Debug)]
pub struct Report {
    pub device: String,
    /// The format the callback writes: the stream's virtual format, which is
    /// what the HAL mixes in.
    pub format: OutputFormat,
    /// What the hardware itself is set to, which can differ from the virtual
    /// format by bit depth without any of it being visible to the callback.
    pub physical: Option<OutputFormat>,
    /// Frames per callback the device settled on after being asked.
    pub buffer_frames: u32,
    /// Frames per callback that were asked for, kept so the report can say
    /// whether the request was honoured.
    pub requested_buffer_frames: u32,
    /// Why the device refused the size that was asked for, when it said so at
    /// all. A device is entitled to decline, and a report that only printed the
    /// size in force would leave a reader unable to tell a request that was
    /// clamped from one that was never made.
    pub buffer_request_refused: Option<String>,
    /// What the device says it will accept, if it would say.
    pub buffer_frame_range: Option<(u32, u32)>,
    pub ring_frames: usize,
    pub ring_multiple: u32,
    /// Occupancy the producer aims to hold, in frames.
    pub producer_target_frames: usize,
    pub callbacks: u64,
    /// Cycles whose buffer list was not the shape the stream format promised,
    /// and which were therefore left as the silence the HAL handed over.
    pub odd_cycles: u64,
    pub frames_requested: Option<Percentiles>,
    /// Intervals between successive IO cycles, from the host timestamps the HAL
    /// hands the callback rather than from a clock this program read.
    pub interval_us: Option<Percentiles>,
    /// Ring occupancy as the callback found it, sampled before it drained.
    pub occupancy_frames: Option<Percentiles>,
    pub underruns: u64,
    pub underrun_frames: u64,
    pub overruns: u64,
    pub overrun_frames: u64,
    pub frames_produced: u64,
    pub frames_consumed: u64,
    /// First to last IO cycle, from the device's own timestamps.
    pub span_seconds: f64,
    /// Frames the device asked for inside that span, which is every cycle but
    /// the last: the span runs from the first cycle's timestamp to the last
    /// one's, so the last cycle's frames had not been played when it ended.
    /// Dividing the whole run's frames by this span would report a device
    /// running one cycle per run too fast, which at five seconds is fifty
    /// frames a second of pure arithmetic error.
    pub frames_in_span: u64,
    pub requested_seconds: f64,
    pub level_dbfs: f64,
    pub left_hz: f64,
    pub right_hz: f64,
    /// Measurements the fixed-size sample stores had no room for.
    pub samples_dropped: u64,
}

impl Report {
    pub fn verdict(&self) -> Verdict {
        if self.callbacks == 0 || self.frames_consumed == 0 {
            Verdict::Silent
        } else if self.underruns > 0 {
            Verdict::Underran
        } else {
            Verdict::Rendered
        }
    }

    /// Frames the ring is still holding, which is the only amount by which the
    /// two totals are allowed to disagree.
    pub fn residual_frames(&self) -> i64 {
        self.frames_produced as i64 - self.frames_consumed as i64
    }

    /// Frames that went missing, as opposed to frames that were merely still in
    /// the ring when the stream stopped.
    ///
    /// Reported apart from the residual because they mean opposite things and one
    /// of them is arithmetic. A single number covering both cannot distinguish a
    /// run that lost audio from a run that ended, and it was printed as
    /// "unaccounted frames" for exactly as long as it took somebody to read 768
    /// and go looking for a leak that was the ring being full.
    pub fn missing_frames(&self) -> i64 {
        let residual = self.residual_frames();
        if residual < 0 {
            // More consumed than produced is not a residual at all: the sink was
            // handed frames nobody wrote.
            return -residual;
        }
        (residual - self.ring_frames as i64).max(0)
    }

    /// Frames per second the device actually took, as opposed to the rate it
    /// declares. A device drifting against its own nominal rate shows up here
    /// and nowhere else in the report.
    pub fn measured_rate(&self) -> f64 {
        if self.span_seconds > 0.0 {
            self.frames_in_span as f64 / self.span_seconds
        } else {
            0.0
        }
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// One percentile line, or a statement that there is no distribution rather
/// than a row of zeros. Zeros would be indistinguishable from a device that
/// answered instantly for a hundred thousand cycles.
fn series(f: &mut fmt::Formatter<'_>, key: &str, values: Option<Percentiles>) -> fmt::Result {
    match values {
        Some(values) => writeln!(
            f,
            "{key} p50 {} p95 {} p99 {} max {}",
            values.p50, values.p95, values.p99, values.max
        ),
        None => writeln!(f, "{key} none measured"),
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "device {}", self.device)?;
        writeln!(f, "output format {}", self.format)?;
        writeln!(f, "io buffer frames {}", self.buffer_frames)?;
        writeln!(f, "callbacks {}", self.callbacks)?;
        series(f, "frames requested", self.frames_requested)?;
        series(f, "callback interval us", self.interval_us)?;
        series(f, "ring occupancy frames", self.occupancy_frames)?;
        writeln!(f, "underruns {}", self.underruns)?;
        writeln!(f, "underrun frames {}", self.underrun_frames)?;
        writeln!(f, "overruns {}", self.overruns)?;
        writeln!(f, "frames produced {}", self.frames_produced)?;
        writeln!(f, "frames consumed {}", self.frames_consumed)?;
        writeln!(f, "span seconds {:.3}", self.span_seconds)?;

        // Everything below is for a person reading the run rather than for the
        // harness parsing it, in the order somebody asking "why those numbers"
        // would want it.
        writeln!(f, "overrun frames {}", self.overrun_frames)?;
        writeln!(f, "frames still in the ring {}", self.residual_frames())?;
        writeln!(f, "frames missing {}", self.missing_frames())?;
        writeln!(
            f,
            "io buffer frames requested {}",
            self.requested_buffer_frames
        )?;
        match self.buffer_frame_range {
            Some((low, high)) => {
                writeln!(f, "io buffer frames range {low} to {high}")?;
            }
            None => writeln!(f, "io buffer frames range unknown")?,
        }
        if let Some(refusal) = &self.buffer_request_refused {
            writeln!(
                f,
                "the device would not be told what buffer size to use: {refusal}"
            )?;
        }
        writeln!(f, "odd cycles {}", self.odd_cycles)?;
        writeln!(f, "io buffer ms {:.3}", self.buffer_ms())?;
        writeln!(f, "stream layout {}", self.format.layout)?;
        match &self.physical {
            Some(physical) => writeln!(f, "physical format {physical}")?,
            None => writeln!(f, "physical format unknown")?,
        }
        writeln!(
            f,
            "ring frames {} being {} io buffers, {:.3} ms",
            self.ring_frames,
            self.ring_multiple,
            self.frames_ms(self.ring_frames as f64)
        )?;
        writeln!(
            f,
            "producer target frames {} being {:.3} ms",
            self.producer_target_frames,
            self.frames_ms(self.producer_target_frames as f64)
        )?;
        if let Some(occupancy) = self.occupancy_frames {
            writeln!(
                f,
                "ring occupancy frames min {} over {} samples",
                occupancy.min, occupancy.count
            )?;
        }
        if let Some(interval) = self.interval_us {
            writeln!(
                f,
                "callback interval us min {} over {} samples",
                interval.min, interval.count
            )?;
        }
        if let Some(frames) = self.frames_requested {
            writeln!(f, "frames requested min {}", frames.min)?;
        }
        writeln!(f, "measured frames per second {:.2}", self.measured_rate())?;
        writeln!(f, "requested seconds {:.3}", self.requested_seconds)?;
        writeln!(
            f,
            "tone {} Hz left {} Hz right at {} dBFS",
            self.left_hz, self.right_hz, self.level_dbfs
        )?;
        writeln!(
            f,
            "format matches contract {}",
            yes_no(self.format.matches_contract())
        )?;
        if !self.format.matches_contract() {
            writeln!(
                f,
                "this device is not the 48000 Hz 2 ch the Windows endpoint mixes at, which is a \
                 finding and not a fault: a later phase needs a converter on this path"
            )?;
        }
        if self.format.layout == Layout::Planar {
            writeln!(
                f,
                "this device hands one buffer per channel, so the callback scatters out of the \
                 ring instead of copying a block"
            )?;
        }
        if self.samples_dropped > 0 {
            writeln!(
                f,
                "{} measurements did not fit their sample store, so the distributions above \
                 describe only the part of the run that did",
                self.samples_dropped
            )?;
        }

        match self.verdict() {
            Verdict::Silent => writeln!(
                f,
                "rendered nothing: the device never asked for a buffer, so no figure above is \
                 evidence that this output path works and the zero underruns mean only that \
                 nothing was ever due"
            ),
            Verdict::Underran => writeln!(
                f,
                "underran: {} callbacks could not be filled and {} frames of silence went to the \
                 device in place of audio",
                self.underruns, self.underrun_frames
            ),
            Verdict::Rendered => Ok(()),
        }
    }
}

impl Report {
    fn frames_ms(&self, frames: f64) -> f64 {
        if self.format.sample_rate == 0 {
            0.0
        } else {
            frames * 1_000.0 / f64::from(self.format.sample_rate)
        }
    }

    fn buffer_ms(&self) -> f64 {
        self.frames_ms(f64::from(self.buffer_frames))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SampleKind;

    fn percentiles(count: usize, value: u64) -> Percentiles {
        Percentiles {
            count,
            min: value,
            p50: value,
            p95: value,
            p99: value,
            max: value,
        }
    }

    fn clean() -> Report {
        Report {
            device: "MacBook Pro Speakers".to_string(),
            format: OutputFormat {
                sample_rate: 48_000,
                channels: 2,
                bits: 32,
                valid_bits: 32,
                kind: SampleKind::Float,
                layout: Layout::Interleaved,
            },
            physical: None,
            buffer_frames: 512,
            requested_buffer_frames: 256,
            buffer_request_refused: None,
            buffer_frame_range: Some((14, 4096)),
            ring_frames: 2_048,
            ring_multiple: 4,
            producer_target_frames: 1_024,
            callbacks: 28_125,
            odd_cycles: 0,
            frames_requested: Some(percentiles(28_125, 512)),
            interval_us: Some(percentiles(28_124, 10_667)),
            occupancy_frames: Some(percentiles(28_125, 1_024)),
            underruns: 0,
            underrun_frames: 0,
            overruns: 0,
            overrun_frames: 0,
            frames_produced: 14_401_024,
            frames_consumed: 14_400_000,
            span_seconds: 300.0,
            frames_in_span: 14_400_000,
            requested_seconds: 300.0,
            level_dbfs: -40.0,
            left_hz: 997.0,
            right_hz: 1997.0,
            samples_dropped: 0,
        }
    }

    #[test]
    fn a_clean_run_prints_the_keyed_lines_the_gate_reads() {
        let text = clean().to_string();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines[0], "device MacBook Pro Speakers");
        assert_eq!(lines[1], "output format 48000 Hz 2 ch 32 bit float");
        assert_eq!(lines[2], "io buffer frames 512");
        assert_eq!(lines[3], "callbacks 28125");
        assert_eq!(lines[4], "frames requested p50 512 p95 512 p99 512 max 512");
        assert_eq!(
            lines[5],
            "callback interval us p50 10667 p95 10667 p99 10667 max 10667"
        );
        assert_eq!(
            lines[6],
            "ring occupancy frames p50 1024 p95 1024 p99 1024 max 1024"
        );
        assert_eq!(lines[7], "underruns 0");
        assert_eq!(lines[8], "underrun frames 0");
        assert_eq!(lines[9], "overruns 0");
        assert_eq!(lines[10], "frames produced 14401024");
        assert_eq!(lines[11], "frames consumed 14400000");
        assert_eq!(lines[12], "span seconds 300.000");
        assert_eq!(clean().verdict(), Verdict::Rendered);
    }

    /// The whole point of the accounting: the two totals differ by the ring's
    /// occupancy and not by a frame more.
    #[test]
    fn a_clean_run_accounts_for_every_frame() {
        let report = clean();
        assert_eq!(report.residual_frames(), 1_024);
        assert_eq!(
            report.missing_frames(),
            0,
            "a residual inside the ring is the ring being full, not a leak"
        );
        assert_eq!(report.residual_frames(), report.ring_frames as i64 / 2);
        assert!((report.measured_rate() - 48_000.0).abs() < 0.5);
    }

    /// And the complement, which is the reason the two are separate numbers: a gap
    /// wider than the ring cannot be the ring, so it is audio that went missing.
    #[test]
    fn a_gap_wider_than_the_ring_is_missing_audio_and_not_a_residual() {
        let mut report = clean();
        let ring = report.ring_frames as i64;
        let before = report.residual_frames();
        report.frames_produced += 4_096;
        // Written against the ring rather than against a remembered constant: the
        // first version of this test asserted 4096 because it assumed the fixture's
        // ring, and the arithmetic it was checking is exactly the arithmetic it got
        // wrong.
        assert_eq!(report.residual_frames(), before + 4_096);
        assert_eq!(report.missing_frames(), before + 4_096 - ring);
        // Consumed beyond produced is not a residual at all: the sink was handed
        // frames nobody wrote, which is a different bug and must not read as zero.
        let mut inverted = clean();
        inverted.frames_consumed = inverted.frames_produced + 512;
        assert_eq!(inverted.missing_frames(), 512);
    }

    /// The shape this project has been burnt by: everything zero, nothing
    /// wrong, nothing measured.
    #[test]
    fn a_run_that_saw_no_callback_says_so_and_is_not_clean() {
        let report = Report {
            callbacks: 0,
            frames_requested: None,
            interval_us: None,
            occupancy_frames: None,
            frames_produced: 1_024,
            frames_consumed: 0,
            span_seconds: 0.0,
            frames_in_span: 0,
            ..clean()
        };
        assert_eq!(report.verdict(), Verdict::Silent);
        let text = report.to_string();
        assert!(text.contains("callbacks 0\n"));
        assert!(text.contains("frames requested none measured\n"));
        assert!(text.contains("callback interval us none measured\n"));
        assert!(text.contains("ring occupancy frames none measured\n"));
        assert!(
            text.contains("rendered nothing: the device never asked for a buffer"),
            "a run that measured nothing must say so:\n{text}"
        );
        assert!(
            text.contains("zero underruns mean only that nothing was ever due"),
            "the refusal has to name the trap it exists for:\n{text}"
        );
        assert!(
            !text.contains("p50 0"),
            "an unmeasured distribution must never print as zeros:\n{text}"
        );
    }

    /// A device that fired but was starved is a different failure from one that
    /// never fired, and the report must not read the same for both.
    #[test]
    fn a_starved_run_names_its_underruns() {
        let report = Report {
            underruns: 12,
            underrun_frames: 3_640,
            ..clean()
        };
        assert_eq!(report.verdict(), Verdict::Underran);
        let text = report.to_string();
        assert!(text.contains("underruns 12\n"));
        assert!(text.contains("underrun frames 3640\n"));
        assert!(text.contains(
            "underran: 12 callbacks could not be filled and 3640 frames of silence went to the \
             device"
        ));
        assert!(!text.contains("rendered nothing"));
    }

    #[test]
    fn a_device_off_the_contract_format_is_reported_as_a_finding() {
        let report = Report {
            format: OutputFormat {
                sample_rate: 44_100,
                ..clean().format
            },
            ..clean()
        };
        let text = report.to_string();
        assert!(text.contains("output format 44100 Hz 2 ch 32 bit float\n"));
        assert!(text.contains("format matches contract no\n"));
        assert!(text.contains("which is a finding and not a fault"));
        // Still a real run: the cadence was measured whatever the rate was.
        assert_eq!(report.verdict(), Verdict::Rendered);
    }

    #[test]
    fn overruns_are_printed_apart_from_underruns() {
        let report = Report {
            overruns: 3,
            overrun_frames: 900,
            ..clean()
        };
        let text = report.to_string();
        assert!(text.contains("overruns 3\n"));
        assert!(text.contains("overrun frames 900\n"));
        assert!(text.contains("underruns 0\n"));
        // An overrun is the producer's failure and does not make the audio that
        // did play any less real.
        assert_eq!(report.verdict(), Verdict::Rendered);
    }

    #[test]
    fn a_dropped_measurement_is_admitted_rather_than_hidden() {
        let report = Report {
            samples_dropped: 7,
            ..clean()
        };
        assert!(report.to_string().contains(
            "7 measurements did not fit their sample store, so the distributions above describe \
             only the part of the run that did"
        ));
    }

    #[test]
    fn the_buffer_the_device_gave_is_printed_beside_the_one_asked_for() {
        let text = clean().to_string();
        assert!(text.contains("io buffer frames 512\n"));
        assert!(text.contains("io buffer frames requested 256\n"));
        assert!(text.contains("io buffer frames range 14 to 4096\n"));
        assert!(text.contains("io buffer ms 10.667\n"));
    }
}
