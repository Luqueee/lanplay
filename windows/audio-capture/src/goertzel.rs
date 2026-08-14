//! Measuring what the captured samples actually contain.
//!
//! A frame count cannot tell captured audio from captured silence. Both
//! produce packets, both advance the device position, and both fill a report
//! with numbers that look like success. This project has already shipped gates
//! that read an absence of evidence as evidence, so this phase measures the
//! content: if the report says a tone was there, a filter tuned to that tone
//! found energy at it, and the frequency printed came out of the samples rather
//! than out of the command line.
//!
//! Goertzel rather than an FFT because the question is small. Evaluating one
//! DFT bin costs two multiplies and two adds per sample and needs no
//! dependency, no power-of-two length and no transform buffer, and a bank of
//! them across the audible band answers "which frequency is loudest" as well as
//! a transform would at this resolution. The refinement below is what buys back
//! the accuracy a coarse bin spacing costs.
//!
//! The analysis runs after the stream has stopped, on a window copied out of
//! the capture loop. Running a filter bank inside that loop would put tens of
//! millions of multiplies a second on the path whose timing is the measurement.

use core::f64::consts::PI;

/// A frequency found in a signal, and how loud it was.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Tone {
    pub frequency: f64,
    /// Amplitude of the sinusoid at that frequency, in dB relative to full
    /// scale. A sine of amplitude 0.5 reads as -6.02.
    pub level_dbfs: f64,
}

/// Magnitude of the discrete Fourier transform of `samples` at `frequency`,
/// with no window applied.
///
/// The unwindowed form is what a test wants when it asks whether a particular
/// frequency is present at all, because a window would spread a neighbouring
/// tone into the bin being asked about and blur the answer.
pub fn magnitude(samples: &[f32], sample_rate: u32, frequency: f64) -> f64 {
    if samples.is_empty() || sample_rate == 0 {
        return 0.0;
    }
    let omega = 2.0 * PI * frequency / f64::from(sample_rate);
    let coefficient = 2.0 * omega.cos();
    let (mut previous, mut older) = (0.0f64, 0.0f64);
    for &sample in samples {
        let current = f64::from(sample) + coefficient * previous - older;
        older = previous;
        previous = current;
    }
    let real = previous - older * omega.cos();
    let imaginary = older * omega.sin();
    real.hypot(imaginary)
}

/// Amplitude of a sinusoid whose transform has the given magnitude, for a
/// window of `length` samples whose coherent gain is `gain`.
///
/// Half the energy of a real sinusoid sits in the negative frequency, which is
/// where the factor of two comes from.
fn amplitude(magnitude: f64, length: usize, gain: f64) -> f64 {
    if length == 0 || gain == 0.0 {
        return 0.0;
    }
    2.0 * magnitude / (length as f64 * gain)
}

fn dbfs(amplitude: f64) -> f64 {
    if amplitude <= 0.0 {
        f64::NEG_INFINITY
    } else {
        20.0 * amplitude.log10()
    }
}

/// The loudest frequency in `samples` between `low` and `high` hertz, or
/// nothing when the strongest thing in the band is quieter than `floor_dbfs`.
///
/// The band is scanned at the natural bin spacing of the window and the winner
/// is then refined by fitting a parabola to its own magnitude and its two
/// neighbours' in decibels. A periodic Hann window is applied first: without
/// one, a tone that does not land on a bin centre leaks across the whole band
/// and the parabola is fitted to the leakage rather than to the peak.
///
/// A floor rather than a peak-to-average test, because the case that matters is
/// digital silence, where every bin is zero and any relative test would still
/// crown a winner.
pub fn dominant(
    samples: &[f32],
    sample_rate: u32,
    low: f64,
    high: f64,
    floor_dbfs: f64,
) -> Option<Tone> {
    let length = samples.len();
    if length < 16 || sample_rate == 0 {
        return None;
    }
    let rate = f64::from(sample_rate);
    let resolution = rate / length as f64;

    let windowed: Vec<f64> = samples
        .iter()
        .enumerate()
        .map(|(index, &sample)| {
            let phase = 2.0 * PI * index as f64 / length as f64;
            f64::from(sample) * 0.5 * (1.0 - phase.cos())
        })
        .collect();

    let first_bin = (low / resolution).ceil().max(1.0) as usize;
    let last_bin = ((high / resolution).floor() as usize).min(length / 2 - 1);
    if first_bin >= last_bin {
        return None;
    }

    let mut best_bin = first_bin;
    let mut best = 0.0f64;
    for bin in first_bin..=last_bin {
        let value = bin_magnitude(&windowed, bin, length);
        if value > best {
            best = value;
            best_bin = bin;
        }
    }

    let lower = bin_magnitude(&windowed, best_bin - 1, length);
    let upper = bin_magnitude(&windowed, best_bin + 1, length);
    let (a, b, c) = (db(lower), db(best), db(upper));
    let denominator = a - 2.0 * b + c;
    let offset = if denominator.abs() > f64::EPSILON {
        (0.5 * (a - c) / denominator).clamp(-0.5, 0.5)
    } else {
        0.0
    };
    let peak_db = b - 0.25 * (a - c) * offset;

    // The Hann window's coherent gain is exactly one half, so a tone measured
    // through it reads six decibels low until this puts it back.
    let level = dbfs(amplitude(10f64.powf(peak_db / 20.0), length, 0.5));
    if level < floor_dbfs {
        return None;
    }
    Some(Tone {
        frequency: (best_bin as f64 + offset) * resolution,
        level_dbfs: level,
    })
}

fn db(magnitude: f64) -> f64 {
    if magnitude <= 0.0 {
        -300.0
    } else {
        20.0 * magnitude.log10()
    }
}

fn bin_magnitude(windowed: &[f64], bin: usize, length: usize) -> f64 {
    let omega = 2.0 * PI * bin as f64 / length as f64;
    let coefficient = 2.0 * omega.cos();
    let (mut previous, mut older) = (0.0f64, 0.0f64);
    for &sample in windowed {
        let current = sample + coefficient * previous - older;
        older = previous;
        previous = current;
    }
    let real = previous - older * omega.cos();
    let imaginary = older * omega.sin();
    real.hypot(imaginary)
}

#[cfg(test)]
mod tests {
    use super::*;

    const RATE: u32 = 48_000;
    /// A tenth of a second, which is a ten hertz bin spacing: coarse enough
    /// that the refinement is doing real work, long enough that 997 and 1997
    /// are a hundred bins apart.
    const WINDOW: usize = 4_800;

    fn sine(frequency: f64, amplitude: f64, length: usize) -> Vec<f32> {
        (0..length)
            .map(|n| (amplitude * (2.0 * PI * frequency * n as f64 / f64::from(RATE)).sin()) as f32)
            .collect()
    }

    #[test]
    fn the_left_contract_tone_is_found() {
        // -6 dBFS is an amplitude of one half.
        let tone = dominant(&sine(997.0, 0.5, WINDOW), RATE, 20.0, 8_000.0, -80.0)
            .expect("a tone at -6 dBFS is far above the floor");
        assert!(
            (tone.frequency - 997.0).abs() < 1.0,
            "found {} Hz",
            tone.frequency
        );
        assert!(
            (tone.level_dbfs - -6.02).abs() < 0.5,
            "read {} dBFS",
            tone.level_dbfs
        );
    }

    #[test]
    fn the_right_contract_tone_is_found() {
        let tone = dominant(&sine(1997.0, 0.5, WINDOW), RATE, 20.0, 8_000.0, -80.0)
            .expect("a tone at -6 dBFS is far above the floor");
        assert!(
            (tone.frequency - 1997.0).abs() < 1.0,
            "found {} Hz",
            tone.frequency
        );
    }

    /// The negative case the acceptance asks for: a signal that is emphatically
    /// not the tone being looked for must not read as it.
    #[test]
    fn a_different_frequency_does_not_answer_to_the_contract_ones() {
        let samples = sine(1_500.0, 0.5, WINDOW);
        let at_1500 = magnitude(&samples, RATE, 1_500.0);
        let at_997 = magnitude(&samples, RATE, 997.0);
        let at_1997 = magnitude(&samples, RATE, 1_997.0);
        assert!(
            at_997 < at_1500 / 100.0,
            "997 Hz read {at_997} against {at_1500} at the real tone"
        );
        assert!(
            at_1997 < at_1500 / 100.0,
            "1997 Hz read {at_1997} against {at_1500} at the real tone"
        );

        let tone = dominant(&samples, RATE, 20.0, 8_000.0, -80.0).expect("a tone is present");
        assert!(
            (tone.frequency - 1_500.0).abs() < 1.0,
            "found {} Hz",
            tone.frequency
        );
    }

    #[test]
    fn silence_is_not_a_tone() {
        let silence = vec![0.0f32; WINDOW];
        assert_eq!(dominant(&silence, RATE, 20.0, 8_000.0, -80.0), None);
        assert_eq!(magnitude(&silence, RATE, 997.0), 0.0);
    }

    /// Dither-level noise is the case that separates a floor from a
    /// peak-to-average test: something is technically loudest, and reporting it
    /// as a tone would turn an idle endpoint into a captured signal.
    #[test]
    fn a_signal_below_the_floor_is_not_a_tone() {
        let whisper = sine(997.0, 0.000_01, WINDOW);
        assert_eq!(dominant(&whisper, RATE, 20.0, 8_000.0, -80.0), None);
    }

    #[test]
    fn the_louder_of_two_tones_wins() {
        let quiet = sine(997.0, 0.05, WINDOW);
        let loud = sine(1_997.0, 0.5, WINDOW);
        let mixed: Vec<f32> = quiet.iter().zip(&loud).map(|(a, b)| a + b).collect();
        let tone = dominant(&mixed, RATE, 20.0, 8_000.0, -80.0).expect("two tones are present");
        assert!(
            (tone.frequency - 1_997.0).abs() < 1.0,
            "found {} Hz",
            tone.frequency
        );
    }

    #[test]
    fn a_tone_between_two_bins_is_still_placed_accurately() {
        // 1005 Hz sits half a bin from either neighbour, which is where an
        // unrefined bin scan is at its worst.
        let tone = dominant(&sine(1_005.0, 0.5, WINDOW), RATE, 20.0, 8_000.0, -80.0)
            .expect("a tone at -6 dBFS is far above the floor");
        assert!(
            (tone.frequency - 1_005.0).abs() < 1.0,
            "found {} Hz",
            tone.frequency
        );
    }

    #[test]
    fn too_few_samples_is_not_an_answer() {
        assert_eq!(
            dominant(&sine(997.0, 0.5, 8), RATE, 20.0, 8_000.0, -80.0),
            None
        );
    }
}
