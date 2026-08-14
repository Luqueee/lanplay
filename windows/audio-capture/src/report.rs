//! The run's findings, in the exact lines the gate reads.
//!
//! The report is a plain value built from the account rather than a set of
//! `println!` calls scattered through the capture loop, which is what lets
//! every line be checked on a machine with no audio endpoint. It also keeps the
//! loop free of formatting work it would otherwise do while a packet was
//! waiting.
//!
//! The keyed lines come first and in a fixed order; everything after them is
//! for a person reading the run rather than a machine parsing it. Two of those
//! trailing lines are the important ones. A run that captured no packets at all
//! says `captured nothing`, and a run whose every packet was flagged silent
//! says `captured only silence`, both with an exit code to match. Neither
//! condition is an error in the capture path -- an idle endpoint produces
//! exactly the first and a muted one exactly the second -- but both have
//! already been read as success by a gate on this project, and a probe that
//! exits zero having measured nothing is how that happens.

use core::fmt;

use crate::accounting::{Percentiles, Totals};
use crate::analysis::{ToneReport, hertz};
use crate::format::MixFormat;

/// How the capture loop was woken.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Wakeup {
    /// The endpoint signalled an event each time a buffer was ready.
    Event,
    /// The loop slept and looked, at the interval it names.
    Poll { interval_ms: f64 },
}

impl fmt::Display for Wakeup {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Wakeup::Event => f.write_str("event"),
            Wakeup::Poll { interval_ms } => write!(f, "poll {interval_ms:.3} ms requested"),
        }
    }
}

/// What the run amounted to, once counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// No packet ever arrived.
    Nothing,
    /// Packets arrived and every one of them was flagged silent.
    OnlySilence,
    /// Packets arrived carrying something.
    Captured,
}

#[derive(Clone, Debug)]
pub struct Report {
    pub endpoint: String,
    pub format: MixFormat,
    pub default_period_ms: f64,
    pub minimum_period_ms: f64,
    /// Frames in the endpoint buffer the engine allocated, as `GetBufferSize`
    /// reported it.
    pub buffer_frames: u32,
    pub wakeup: Wakeup,
    /// Whether an event-driven initialise was refused and the run fell back.
    pub event_refused: Option<String>,
    pub requested_seconds: f64,
    pub totals: Totals,
    pub packet_frames: Option<Percentiles>,
    /// Intervals between successive wakeups of the capture loop, in
    /// microseconds.
    pub wakeup_intervals_us: Option<Percentiles>,
    pub wakeup_timeouts: u64,
    pub tone: ToneReport,
    /// Packets `GetBuffer` refused, and what it said the first time.
    pub buffer_errors: u64,
    pub first_buffer_error: Option<String>,
    /// Measurements the fixed-size sample stores could not hold.
    pub samples_dropped: u64,
    /// Bytes of audio the dump buffer had no room for.
    pub pcm_dropped: u64,
}

impl Report {
    pub fn verdict(&self) -> Verdict {
        if self.totals.packets == 0 || self.totals.frames == 0 {
            Verdict::Nothing
        } else if self.totals.silent_packets == self.totals.packets {
            Verdict::OnlySilence
        } else {
            Verdict::Captured
        }
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let packet_frames = self.packet_frames.unwrap_or(Percentiles {
            count: 0,
            min: 0,
            p50: 0,
            p95: 0,
            p99: 0,
            max: 0,
        });

        writeln!(f, "endpoint {}", self.endpoint)?;
        writeln!(f, "mix format {}", self.format)?;
        writeln!(
            f,
            "buffer period default {:.3} ms minimum {:.3} ms",
            self.default_period_ms, self.minimum_period_ms
        )?;
        writeln!(f, "packets {}", self.totals.packets)?;
        writeln!(f, "frames captured {}", self.totals.frames)?;
        writeln!(f, "discontinuities {}", self.totals.discontinuities)?;
        writeln!(f, "silent packets {}", self.totals.silent_packets)?;
        writeln!(
            f,
            "position gaps {} totalling {} frames",
            self.totals.gaps, self.totals.gap_frames
        )?;
        writeln!(
            f,
            "device position first {} last {}",
            self.totals.first_position, self.totals.last_position
        )?;
        writeln!(f, "qpc span {:.6}", self.totals.qpc_span_seconds())?;
        writeln!(
            f,
            "packet frames p50 {} p95 {} p99 {} max {}",
            packet_frames.p50, packet_frames.p95, packet_frames.p99, packet_frames.max
        )?;
        writeln!(
            f,
            "tone left {:.1} right {:.1}",
            hertz(self.tone.left),
            hertz(self.tone.right)
        )?;
        writeln!(f, "tone channels distinct {}", yes_no(self.tone.distinct()))?;

        writeln!(f, "requested seconds {:.3}", self.requested_seconds)?;
        writeln!(f, "wakeup mode {}", self.wakeup)?;
        if let Some(reason) = &self.event_refused {
            writeln!(
                f,
                "event-driven loopback was refused by this endpoint: {reason}"
            )?;
        }
        match self.wakeup_intervals_us {
            Some(intervals) => writeln!(
                f,
                "wakeup interval us p50 {} p95 {} p99 {} max {} over {} wakeups",
                intervals.p50, intervals.p95, intervals.p99, intervals.max, intervals.count
            )?,
            None => writeln!(f, "wakeup interval us none measured")?,
        }
        writeln!(f, "wakeup timeouts {}", self.wakeup_timeouts)?;
        writeln!(f, "endpoint buffer {} frames", self.buffer_frames)?;
        writeln!(
            f,
            "packet frames min {} count {}",
            packet_frames.min, packet_frames.count
        )?;
        writeln!(
            f,
            "position rewinds {} totalling {} frames",
            self.totals.rewinds, self.totals.rewind_frames
        )?;
        writeln!(
            f,
            "discontinuities in flight {} first packet discontinuous {}",
            self.totals.discontinuities_in_flight(),
            yes_no(self.totals.first_packet_discontinuous)
        )?;
        writeln!(f, "timestamp errors {}", self.totals.timestamp_errors)?;
        writeln!(f, "getbuffer errors {}", self.buffer_errors)?;
        if let Some(error) = &self.first_buffer_error {
            writeln!(f, "first getbuffer error {error}")?;
        }
        if self.format.valid_bits != self.format.bits_per_sample {
            writeln!(
                f,
                "valid bits {} of {}",
                self.format.valid_bits, self.format.bits_per_sample
            )?;
        }
        writeln!(f, "channel mask {:#x}", self.format.channel_mask)?;
        writeln!(
            f,
            "analysis window {} frames at {:.3} Hz resolution",
            self.tone.analysed_frames, self.tone.resolution_hz
        )?;
        writeln!(
            f,
            "tone level left {:.2} dBFS right {:.2} dBFS",
            self.tone.left.map_or(f64::NEG_INFINITY, |t| t.level_dbfs),
            self.tone.right.map_or(f64::NEG_INFINITY, |t| t.level_dbfs)
        )?;
        if self.samples_dropped > 0 {
            writeln!(
                f,
                "{} measurements did not fit their sample store, so the distributions above \
                 describe only the part of the run that did",
                self.samples_dropped
            )?;
        }
        if self.pcm_dropped > 0 {
            writeln!(
                f,
                "{} bytes of audio did not fit the dump buffer and are missing from the wav",
                self.pcm_dropped
            )?;
        }
        if !self.format.matches_contract() {
            writeln!(
                f,
                "this endpoint is not the 48000 Hz 2 ch mix the rest of the project assumed, \
                 which is a finding and not a fault"
            )?;
        }

        match self.verdict() {
            Verdict::Nothing => writeln!(
                f,
                "captured nothing: the endpoint delivered no frames at all, so nothing in this \
                 report is evidence that loopback works"
            ),
            Verdict::OnlySilence => writeln!(
                f,
                "captured only silence: every packet carried the silent flag, so the frame \
                 counts above describe the shape of the stream and not any audio in it"
            ),
            Verdict::Captured => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::accounting::{Accounting, Packet};
    use crate::format::{RawExtensible, RawWaveFormat, SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_EXTENSIBLE};
    use crate::goertzel::Tone;

    fn format() -> MixFormat {
        MixFormat::from_raw(&RawWaveFormat {
            format_tag: WAVE_FORMAT_EXTENSIBLE,
            channels: 2,
            samples_per_sec: 48_000,
            avg_bytes_per_sec: 48_000 * 8,
            block_align: 8,
            bits_per_sample: 32,
            extensible: Some(RawExtensible {
                valid_bits: 32,
                channel_mask: 3,
                subformat: SUBTYPE_IEEE_FLOAT,
            }),
        })
        .expect("a describable format")
    }

    fn report(totals: Totals, tone: ToneReport) -> Report {
        Report {
            endpoint: "Speakers (Realtek(R) Audio)".into(),
            format: format(),
            default_period_ms: 10.0,
            minimum_period_ms: 2.667,
            buffer_frames: 480,
            wakeup: Wakeup::Event,
            event_refused: None,
            requested_seconds: 5.0,
            totals,
            packet_frames: Some(Percentiles {
                count: 500,
                min: 440,
                p50: 480,
                p95: 480,
                p99: 960,
                max: 1_440,
            }),
            wakeup_intervals_us: Some(Percentiles {
                count: 500,
                min: 9_000,
                p50: 10_000,
                p95: 10_400,
                p99: 12_000,
                max: 30_000,
            }),
            wakeup_timeouts: 0,
            tone,
            buffer_errors: 0,
            first_buffer_error: None,
            samples_dropped: 0,
            pcm_dropped: 0,
        }
    }

    fn tones() -> ToneReport {
        ToneReport {
            left: Some(Tone {
                frequency: 997.03,
                level_dbfs: -6.02,
            }),
            right: Some(Tone {
                frequency: 1996.94,
                level_dbfs: -6.03,
            }),
            resolution_hz: 10.0,
            analysed_frames: 4_800,
        }
    }

    fn good_totals() -> Totals {
        let mut account = Accounting::new();
        let mut position = 4_800;
        let mut qpc = 100_000_000;
        for _ in 0..10 {
            account.record(&Packet {
                device_position: position,
                frames: 480,
                qpc_100ns: qpc,
                discontinuity: false,
                silent: false,
                timestamp_error: false,
            });
            position += 480;
            qpc += 100_000;
        }
        account.totals()
    }

    fn lines(report: &Report) -> Vec<String> {
        report.to_string().lines().map(str::to_owned).collect()
    }

    #[test]
    fn the_keyed_lines_come_first_and_in_order() {
        let printed = lines(&report(good_totals(), tones()));
        assert_eq!(
            printed[..13],
            [
                "endpoint Speakers (Realtek(R) Audio)",
                "mix format 48000 Hz 2 ch 32 bit float",
                "buffer period default 10.000 ms minimum 2.667 ms",
                "packets 10",
                "frames captured 4800",
                "discontinuities 0",
                "silent packets 0",
                "position gaps 0 totalling 0 frames",
                "device position first 4800 last 9120",
                "qpc span 0.090000",
                "packet frames p50 480 p95 480 p99 960 max 1440",
                "tone left 997.0 right 1996.9",
                "tone channels distinct yes",
            ]
        );
    }

    #[test]
    fn a_run_that_captured_something_is_a_pass() {
        let report = report(good_totals(), tones());
        assert_eq!(report.verdict(), Verdict::Captured);
        assert!(!report.to_string().contains("captured nothing"));
        assert!(!report.to_string().contains("captured only silence"));
    }

    #[test]
    fn a_run_with_no_packets_says_it_captured_nothing() {
        let report = report(Totals::default(), ToneReport::empty());
        assert_eq!(report.verdict(), Verdict::Nothing);
        let printed = lines(&report);
        assert!(printed.contains(&"packets 0".to_string()));
        assert!(printed.contains(&"frames captured 0".to_string()));
        assert!(printed.contains(&"tone left 0.0 right 0.0".to_string()));
        assert!(printed.contains(&"tone channels distinct no".to_string()));
        assert!(
            report.to_string().contains("captured nothing"),
            "a run that captured nothing must say so"
        );
    }

    /// The keyed lines must be present even when there is nothing to say on
    /// them, because a gate that cannot find a line cannot tell a silent run
    /// from a crashed one.
    #[test]
    fn an_empty_run_still_prints_every_keyed_line() {
        let mut empty = report(Totals::default(), ToneReport::empty());
        empty.packet_frames = None;
        empty.wakeup_intervals_us = None;
        let printed = lines(&empty);
        for key in [
            "endpoint ",
            "mix format ",
            "buffer period default ",
            "packets ",
            "frames captured ",
            "discontinuities ",
            "silent packets ",
            "position gaps ",
            "device position first ",
            "qpc span ",
            "packet frames p50 ",
            "tone left ",
            "tone channels distinct ",
        ] {
            assert!(
                printed.iter().any(|line| line.starts_with(key)),
                "no line starting {key:?}"
            );
        }
        assert!(printed.contains(&"packet frames p50 0 p95 0 p99 0 max 0".to_string()));
    }

    #[test]
    fn a_run_of_nothing_but_silent_packets_says_so() {
        let mut account = Accounting::new();
        for index in 0..4u64 {
            account.record(&Packet {
                device_position: index * 480,
                frames: 480,
                qpc_100ns: index * 100_000,
                discontinuity: false,
                silent: true,
                timestamp_error: false,
            });
        }
        let report = report(account.totals(), ToneReport::empty());
        assert_eq!(report.verdict(), Verdict::OnlySilence);
        assert!(report.to_string().contains("captured only silence"));
        assert!(!report.to_string().contains("captured nothing"));
    }

    #[test]
    fn gaps_are_printed_as_a_count_and_a_total() {
        let mut account = Accounting::new();
        account.record(&Packet {
            device_position: 0,
            frames: 480,
            qpc_100ns: 0,
            discontinuity: false,
            silent: false,
            timestamp_error: false,
        });
        account.record(&Packet {
            device_position: 1_920,
            frames: 480,
            qpc_100ns: 400_000,
            discontinuity: true,
            silent: false,
            timestamp_error: false,
        });
        let printed = lines(&report(account.totals(), tones()));
        assert!(printed.contains(&"position gaps 1 totalling 1440 frames".to_string()));
        assert!(printed.contains(&"discontinuities 1".to_string()));
        assert!(
            printed
                .contains(&"discontinuities in flight 1 first packet discontinuous no".to_string())
        );
    }

    #[test]
    fn an_unexpected_mix_format_is_called_out() {
        let mut odd = report(good_totals(), tones());
        odd.format = MixFormat::from_raw(&RawWaveFormat {
            format_tag: WAVE_FORMAT_EXTENSIBLE,
            channels: 6,
            samples_per_sec: 44_100,
            avg_bytes_per_sec: 44_100 * 24,
            block_align: 24,
            bits_per_sample: 32,
            extensible: Some(RawExtensible {
                valid_bits: 24,
                channel_mask: 0x3F,
                subformat: SUBTYPE_IEEE_FLOAT,
            }),
        })
        .expect("a describable format");
        let printed = odd.to_string();
        assert!(printed.contains("mix format 44100 Hz 6 ch 32 bit float"));
        assert!(printed.contains("valid bits 24 of 32"));
        assert!(printed.contains("not the 48000 Hz 2 ch mix"));
    }

    #[test]
    fn a_fallback_to_polling_is_stated() {
        let mut polled = report(good_totals(), tones());
        polled.wakeup = Wakeup::Poll { interval_ms: 5.0 };
        polled.event_refused = Some("AUDCLNT_E_UNSUPPORTED_FORMAT".into());
        let printed = polled.to_string();
        assert!(printed.contains("wakeup mode poll 5.000 ms requested"));
        assert!(printed.contains("event-driven loopback was refused"));
    }

    #[test]
    fn two_channels_with_one_tone_are_not_distinct() {
        let same = ToneReport {
            right: Some(Tone {
                frequency: 997.04,
                level_dbfs: -6.02,
            }),
            ..tones()
        };
        let printed = lines(&report(good_totals(), same));
        assert!(printed.contains(&"tone left 997.0 right 997.0".to_string()));
        assert!(printed.contains(&"tone channels distinct no".to_string()));
    }
}
