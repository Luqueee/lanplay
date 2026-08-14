//! The tone, generated one frame at a time from a phase accumulator.
//!
//! Two frequencies, one per channel, because the capture side has to be able to
//! prove things a frame count cannot: which channel is which, and that both
//! carry distinct real content rather than one channel duplicated or a buffer
//! left over from a previous packet. 997 Hz on the left and 1997 Hz on the
//! right are coprime with 48000, so neither produces a short repeating pattern
//! that a buggy accumulator on either side could fake.
//!
//! That coprimality is also why the samples come from an accumulator rather
//! than a lookup table: a table exact for either tone would have to be a whole
//! second of samples, 48000 entries, and would have to be rebuilt for any other
//! sample rate. The other obvious shape, `sin(2 * PI * f * n / rate)` for a
//! growing sample index `n`, is worse in a different way — a ten minute run
//! reaches n = 28.8 million, and the product loses low bits exactly where the
//! argument reduction inside `sin` needs them.
//!
//! So the phase is kept in turns rather than radians and wrapped by subtracting
//! one whole turn. Wrapping is what keeps the precision: the accumulator stays
//! in [0, 1) forever, so its ulp stays around 1e-16 instead of growing with the
//! run, and the increment `f / rate` is added in double precision, which puts
//! the accumulated phase error over ten minutes some nine orders of magnitude
//! below anything a Goertzel filter could resolve.

use core::f64::consts::TAU;

/// What the batch contract says the tone is, and the only tone this program
/// plays. Deliberately not settable from the command line: the capture side
/// asserts these numbers, so a flag that changed them would turn a real
/// disagreement between the two halves into a silent pass.
pub const CONTRACT: ToneSpec = ToneSpec {
    sample_rate: 48_000,
    channels: 2,
    left_hz: 997.0,
    right_hz: 1997.0,
    // Chosen for whoever is sitting next to the endpoint, not for the detector.
    // The only active render endpoint on the lab host is a monitor's audio, so
    // every run is an hour of two-tone test signal in somebody's room, and
    // amplitude buys the measurement nothing: loopback capture takes the
    // digital mix before any converter, so a -20 dBFS sine arrives exact in
    // float and still thousands of counts wide in sixteen bits. Being able to
    // choose the level for the operator is a property of the capture path being
    // digital, and would not be safe if anything analogue were in it.
    level_dbfs: -20.0,
};

/// The tone as numbers, separated from the generator so the format check and
/// the report can talk about it without owning a generator.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ToneSpec {
    pub sample_rate: u32,
    pub channels: u16,
    pub left_hz: f64,
    pub right_hz: f64,
    pub level_dbfs: f64,
}

impl ToneSpec {
    /// Peak sample value, as a fraction of full scale.
    pub fn amplitude(&self) -> f64 {
        10f64.powf(self.level_dbfs / 20.0)
    }
}

/// One channel's phase, advanced by a fixed increment per sample.
#[derive(Clone, Copy, Debug)]
struct Oscillator {
    /// Position in the cycle, in turns, always in [0, 1).
    turns: f64,
    /// Turns per sample: the frequency divided by the sample rate.
    per_sample: f64,
}

impl Oscillator {
    fn new(hz: f64, sample_rate: u32) -> Oscillator {
        Oscillator {
            turns: 0.0,
            per_sample: hz / f64::from(sample_rate),
        }
    }

    /// The sample at the current phase, then advance.
    ///
    /// The wrap is a subtraction rather than `%`: the increment is well under
    /// one turn for any audible frequency, so a single subtraction always
    /// suffices and a branch is cheaper than a division.
    #[inline]
    fn next(&mut self) -> f64 {
        let sample = (TAU * self.turns).sin();
        self.turns += self.per_sample;
        if self.turns >= 1.0 {
            self.turns -= 1.0;
        }
        sample
    }
}

/// The generator. Holds two oscillators and the level they are scaled to.
#[derive(Clone, Copy, Debug)]
pub struct Tone {
    left: Oscillator,
    right: Oscillator,
    amplitude: f64,
}

impl Tone {
    pub fn new(spec: ToneSpec) -> Tone {
        Tone {
            left: Oscillator::new(spec.left_hz, spec.sample_rate),
            right: Oscillator::new(spec.right_hz, spec.sample_rate),
            amplitude: spec.amplitude(),
        }
    }

    /// One stereo frame, left then right, in the order WASAPI interleaves them.
    #[inline]
    pub fn next_frame(&mut self) -> [f32; 2] {
        [
            (self.amplitude * self.left.next()) as f32,
            (self.amplitude * self.right.next()) as f32,
        ]
    }

    /// Fills an interleaved stereo buffer and answers how many frames it wrote.
    ///
    /// Nothing is allocated and nothing is logged: this runs between a
    /// `GetBuffer` and a `ReleaseBuffer`, where the device is waiting.
    #[inline]
    pub fn fill_stereo(&mut self, interleaved: &mut [f32]) -> u32 {
        let mut frames = 0u32;
        for frame in interleaved.chunks_exact_mut(2) {
            let [left, right] = self.next_frame();
            frame[0] = left;
            frame[1] = right;
            frames += 1;
        }
        frames
    }

    /// Both accumulators' current phase, in turns. Exists so a test can assert
    /// the invariant the wrap exists to maintain, which is not otherwise
    /// observable from outside.
    pub fn phase_turns(&self) -> [f64; 2] {
        [self.left.turns, self.right.turns]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ten minutes at 48 kHz.
    const TEN_MINUTES: u64 = 48_000 * 600;

    /// The wrap is the whole reason this is an accumulator rather than a
    /// growing product, so the run it has to survive is asserted at its real
    /// length: 28,800,000 frames, ten minutes at 48 kHz, every frame checked.
    #[test]
    fn phase_stays_in_range_over_a_ten_minute_run() {
        assert_eq!(TEN_MINUTES, 28_800_000);

        let mut tone = Tone::new(CONTRACT);
        let amplitude = CONTRACT.amplitude() as f32;

        for _ in 0..TEN_MINUTES {
            let [left, right] = tone.next_frame();
            let [left_turns, right_turns] = tone.phase_turns();
            assert!(
                (0.0..1.0).contains(&left_turns) && (0.0..1.0).contains(&right_turns),
                "phase left over the wrap: {left_turns} {right_turns}"
            );
            assert!(
                left.abs() <= amplitude && right.abs() <= amplitude,
                "sample outside the requested level: {left} {right}"
            );
        }
    }

    /// Counts sign changes over exactly one second, which for a sine at `f` Hz
    /// is `2f` crossings less the one at the origin: the first sample is
    /// exactly zero and has no earlier sample to change sign against.
    ///
    /// This is what proves the two channels carry different content and which
    /// one is which. A test that only compared the channels for inequality
    /// would pass on two tones at the wrong frequencies, or on the right two
    /// swapped.
    #[test]
    fn the_two_channels_carry_their_own_frequencies() {
        let mut tone = Tone::new(CONTRACT);
        let mut left_sign = 0i32;
        let mut right_sign = 0i32;
        let mut left_changes = 0u32;
        let mut right_changes = 0u32;

        for _ in 0..CONTRACT.sample_rate {
            let [left, right] = tone.next_frame();
            for (sample, sign, changes) in [
                (left, &mut left_sign, &mut left_changes),
                (right, &mut right_sign, &mut right_changes),
            ] {
                let now = if sample > 0.0 {
                    1
                } else if sample < 0.0 {
                    -1
                } else {
                    0
                };
                if now != 0 {
                    if *sign != 0 && now != *sign {
                        *changes += 1;
                    }
                    *sign = now;
                }
            }
        }

        assert_eq!(left_changes, 2 * 997 - 1);
        assert_eq!(right_changes, 2 * 1997 - 1);
        assert_ne!(left_changes, right_changes);
    }

    /// -20 dBFS is 0.1 of full scale exactly, and a sine sampled at 48 kHz gets
    /// within `cos(pi * f / rate)` of its own peak, which at 997 Hz is 0.998 of
    /// it. So the peak over a second must sit just under the requested level,
    /// never above it and never far below.
    #[test]
    fn the_peak_is_the_level_that_was_asked_for() {
        assert!((CONTRACT.amplitude() - 0.1).abs() < 1e-15);
        assert!(
            (ToneSpec {
                level_dbfs: 0.0,
                ..CONTRACT
            }
            .amplitude()
                - 1.0)
                .abs()
                < 1e-15
        );

        let mut tone = Tone::new(CONTRACT);
        let amplitude = CONTRACT.amplitude() as f32;
        let mut peak = 0.0f32;
        for _ in 0..CONTRACT.sample_rate {
            let [left, right] = tone.next_frame();
            peak = peak.max(left.abs()).max(right.abs());
        }

        assert!(peak <= amplitude, "peak {peak} above full requested level");
        assert!(peak > amplitude * 0.997, "peak {peak} far below the level");
    }
}
