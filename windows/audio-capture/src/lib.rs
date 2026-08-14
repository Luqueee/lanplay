//! What the Windows audio endpoint actually delivers, measured rather than
//! assumed.
//!
//! Every later decision about audio on this project -- whether anything needs
//! resampling, how deep a jitter buffer has to be, what a packetiser should
//! treat as one unit -- depends on the shape of what WASAPI hands over: the
//! format, the size of a packet, how often one arrives, and whether the stream
//! is continuous. None of that is worth guessing when the machine will say. So
//! this crate is an instrument, not a component: it opens the default render
//! endpoint in loopback, reads packets for a while, and prints what it saw.
//!
//! Three things are measured that a naive capture would not distinguish.
//!
//! Frames are accounted for against the device position `GetBuffer` reports,
//! not against a running total this code keeps. Consecutive packets have to
//! satisfy an exact identity, and where they do not, the hole is reported in
//! frames. A count of suspicious packets would say that something went wrong;
//! this says how much audio is missing.
//!
//! Discontinuity and silence are counted separately, because the flags mean
//! different things: one is the engine admitting it lost data, the other is the
//! host playing nothing. A capture that pooled them could not tell a glitching
//! machine from an idle one.
//!
//! And the content itself is measured. A frame count cannot tell captured audio
//! from captured silence, so a filter tuned to each channel reports the
//! dominant frequency it found, which is also what proves the two channels
//! carry different signals and are therefore not one channel read twice.
//!
//! What is deliberately absent: no resampling, no format conversion, no
//! encoding, no process loopback, no network. Learning the real format is the
//! point, and code that converted it away would have thrown the answer out
//! before printing it.
//!
//! Everything except the WASAPI calls themselves is platform independent and
//! tested off Windows, because the arithmetic that matters -- position
//! accounting, format decoding, tone detection, the wording of the report --
//! is arithmetic, and needing an audio endpoint to check it would mean nobody
//! checked it.

pub mod accounting;
pub mod analysis;
pub mod format;
pub mod goertzel;
pub mod probe;
pub mod report;
pub mod wav;

#[cfg(windows)]
pub mod capture;

pub use accounting::{Accounting, Deviation, Packet, Percentiles, Samples, Totals};
pub use analysis::{ToneReport, analyse};
pub use format::{MixFormat, RawWaveFormat, SampleKind};
pub use goertzel::Tone;
pub use report::{Report, Verdict, Wakeup};

#[cfg(windows)]
pub use capture::{CaptureError, Captured, Request};
