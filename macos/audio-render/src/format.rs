//! The output format as the device states it, decoded from an
//! `AudioStreamBasicDescription` and printed the way the Windows side prints
//! its mix format.
//!
//! Deliberately not converted to anything. What this phase has to establish is
//! whether the two endpoints agree, and a type that normalised a 44100 Hz
//! device into the project's assumption would have thrown the finding away
//! before anyone read it. The wording of the line is copied from the capture
//! crate's mix format on purpose, so the two machines' reports can be read
//! against each other without translating between them.

use core::fmt;

/// Whether samples are floating point or integer, which is the difference
/// between a buffer this crate can write and one it cannot.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SampleKind {
    Float,
    Int,
}

impl fmt::Display for SampleKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SampleKind::Float => write!(f, "float"),
            SampleKind::Int => write!(f, "int"),
        }
    }
}

/// How a stream's frames are laid out across the buffers an IOProc is handed.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Layout {
    /// One buffer, channels alternating inside it. The ring's own layout, so
    /// the callback is a single copy.
    Interleaved,
    /// One buffer per channel. The callback has to scatter, which it does
    /// straight out of the ring rather than through anything in between.
    Planar,
}

impl fmt::Display for Layout {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Layout::Interleaved => write!(f, "interleaved"),
            Layout::Planar => write!(f, "planar"),
        }
    }
}

/// One linear PCM format, as an audio stream reported it.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct OutputFormat {
    pub sample_rate: u32,
    pub channels: u16,
    /// Bits in the container. A 24-in-32 device says 32 here and 24 in
    /// [`OutputFormat::valid_bits`], and the difference matters to whatever
    /// eventually writes integers.
    pub bits: u16,
    pub valid_bits: u16,
    pub kind: SampleKind,
    pub layout: Layout,
}

impl OutputFormat {
    /// Bytes one frame occupies in an interleaved buffer.
    pub fn frame_bytes(&self) -> usize {
        usize::from(self.bits / 8) * usize::from(self.channels)
    }

    /// Whether this is the 48000 Hz stereo the Windows endpoint mixes at, which
    /// is the whole question of whether a later phase needs a converter.
    ///
    /// Not a requirement and not checked anywhere: only something the report
    /// says out loud.
    pub fn matches_contract(&self) -> bool {
        self.sample_rate == 48_000 && self.channels == 2
    }

    /// Whether this crate can write into buffers of this format at all.
    ///
    /// The ring carries 32-bit float because that is what the tone generator
    /// produces and what the HAL presents for every mixable device. Anything
    /// else would need a conversion, and a conversion is the one thing this
    /// phase must not quietly perform.
    pub fn is_writable(&self) -> bool {
        self.kind == SampleKind::Float && self.bits == 32 && self.channels > 0
    }
}

impl fmt::Display for OutputFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} Hz {} ch {} bit {}",
            self.sample_rate, self.channels, self.bits, self.kind
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stereo_float() -> OutputFormat {
        OutputFormat {
            sample_rate: 48_000,
            channels: 2,
            bits: 32,
            valid_bits: 32,
            kind: SampleKind::Float,
            layout: Layout::Interleaved,
        }
    }

    /// The line has to read exactly as the Windows endpoint's does, because the
    /// two reports are compared by eye and by grep.
    #[test]
    fn the_contract_format_prints_as_the_windows_side_prints_it() {
        let format = stereo_float();
        assert_eq!(format.to_string(), "48000 Hz 2 ch 32 bit float");
        assert!(format.matches_contract());
        assert!(format.is_writable());
        assert_eq!(format.frame_bytes(), 8);
    }

    #[test]
    fn a_device_at_another_rate_is_a_finding_and_still_writable() {
        let format = OutputFormat {
            sample_rate: 44_100,
            ..stereo_float()
        };
        assert_eq!(format.to_string(), "44100 Hz 2 ch 32 bit float");
        assert!(!format.matches_contract());
        assert!(format.is_writable());
    }

    #[test]
    fn an_integer_device_is_not_something_a_float_ring_may_write() {
        let format = OutputFormat {
            bits: 16,
            valid_bits: 16,
            kind: SampleKind::Int,
            ..stereo_float()
        };
        assert_eq!(format.to_string(), "48000 Hz 2 ch 16 bit int");
        assert!(!format.is_writable());
        assert_eq!(format.frame_bytes(), 4);
    }
}
