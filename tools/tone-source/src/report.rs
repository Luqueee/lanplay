//! What the run played, in the terms the loopback measurement needs.
//!
//! One labelled line per statistic and nothing else on stdout, matching
//! `tools/present-source`: whatever drives a joint run reads these back, and a
//! table would have to be parsed apart to get there.
//!
//! The endpoint and its mix format are in here rather than only in the banner
//! because they are the finding. A run whose numbers were captured without the
//! format they were produced at cannot be compared against anything.

use core::fmt;

use lanplay_telemetry::Nanos;

use crate::format::MixFormat;

/// The whole result of a run.
pub struct Report {
    pub endpoint: String,
    pub format: MixFormat,
    pub buffers_filled: u64,
    pub frames_rendered: u64,
    /// Wakes at which the device buffer was found empty, meaning the engine had
    /// consumed everything written and there was a moment with nothing to play.
    /// Non-zero means the gap is the source's, and a gap the source produced
    /// would be read on the capture side as a gap the capture lost. Those need
    /// opposite fixes, which is the whole reason this is counted separately
    /// rather than left to be inferred from a short frame count.
    pub underruns: u64,
    pub span: Nanos,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "endpoint: {}", self.endpoint)?;
        writeln!(f, "mix format: {}", self.format)?;
        match self.format.channel_mask {
            Some(mask) => writeln!(
                f,
                "channel mask: 0x{mask:08X} ({})",
                self.format.channel_mask_note()
            )?,
            None => writeln!(f, "channel mask: {}", self.format.channel_mask_note())?,
        }
        writeln!(f, "buffers filled: {}", self.buffers_filled)?;
        writeln!(f, "frames rendered: {}", self.frames_rendered)?;
        writeln!(f, "underruns: {}", self.underruns)?;
        writeln!(f, "span seconds: {:.3}", self.span.as_secs_f64())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::Sample;

    fn report() -> Report {
        Report {
            endpoint: "LG ULTRAWIDE (NVIDIA High Definition Audio)".into(),
            format: MixFormat {
                rate: 48_000,
                channels: 2,
                bits: 32,
                sample: Sample::Float,
                block_align: 8,
                channel_mask: Some(0x3),
            },
            buffers_filled: 3_001,
            frames_rendered: 1_441_440,
            underruns: 0,
            span: Nanos(30_030_000_000),
        }
    }

    /// The report is read by whoever runs the source alongside a capture, so its
    /// shape is a contract: seven lines, in this order, `name: value`.
    #[test]
    fn renders_one_labelled_line_per_statistic() {
        assert_eq!(
            report().to_string(),
            "endpoint: LG ULTRAWIDE (NVIDIA High Definition Audio)\n\
             mix format: 48000 Hz 2 ch 32 bit float\n\
             channel mask: 0x00000003 (front left, front right)\n\
             buffers filled: 3001\n\
             frames rendered: 1441440\n\
             underruns: 0\n\
             span seconds: 30.030\n"
        );
    }

    /// An endpoint that reports no mask still gets a line, because a reader
    /// counting lines must not have to guess whether one went missing.
    #[test]
    fn says_so_when_there_is_no_channel_mask() {
        let report = Report {
            format: MixFormat {
                channel_mask: None,
                ..report().format
            },
            ..report()
        };
        assert!(report.to_string().contains("channel mask: none reported\n"));
    }
}
