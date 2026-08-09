//! Deciding when each backend runs, so drift cannot be mistaken for a result.
//!
//! Running WGC for a minute and then Desktop Duplication for a minute measures
//! the difference between the two APIs plus the difference between the first
//! minute and the second: the GPU is hotter, the driver is warmer, whatever
//! else the machine is doing has moved on. Alternating short blocks puts both
//! backends through the same drift, so what survives the interleaving is the
//! part that is about the APIs.
//!
//! The starting backend is randomised because the first block is the one that
//! is different, whatever else the schedule does about it. Randomised from a
//! seed rather than from the clock so a suspicious result can be re-run
//! exactly.

use serde::Serialize;

/// Shortest block worth running. Below this the per-block start-up dominates
/// and the block reports the cost of starting a capture rather than the cost
/// of running one.
pub const MIN_BLOCK_SECONDS: f64 = 1.0;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BackendKind {
    Wgc,
    Dda,
}

impl BackendKind {
    pub const BOTH: [BackendKind; 2] = [BackendKind::Wgc, BackendKind::Dda];

    pub fn as_str(self) -> &'static str {
        match self {
            BackendKind::Wgc => "wgc",
            BackendKind::Dda => "dda",
        }
    }

    pub fn other(self) -> BackendKind {
        match self {
            BackendKind::Wgc => BackendKind::Dda,
            BackendKind::Dda => BackendKind::Wgc,
        }
    }
}

impl core::fmt::Display for BackendKind {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub struct Block {
    pub index: usize,
    pub backend: BackendKind,
    pub seconds: f64,
}

/// The block order for a compare run.
///
/// Always an even number of blocks so each backend gets the same count, and
/// the requested total is spent in full rather than truncated: the block
/// length is the total divided by the block count, not the requested block
/// length. A run asked for 11 seconds in 5-second blocks gets two blocks of
/// 5.5 rather than two of 5 and a second of nothing.
pub fn alternating(total_seconds: f64, block_seconds: f64, seed: u64) -> Vec<Block> {
    let requested = block_seconds.max(MIN_BLOCK_SECONDS);
    let total = total_seconds.max(2.0 * requested);

    let mut count = (total / requested).floor() as usize;
    count = count.max(2);
    count -= count % 2;

    let seconds = total / count as f64;
    let first = first_backend(seed);

    (0..count)
        .map(|index| Block {
            index,
            backend: if index % 2 == 0 { first } else { first.other() },
            seconds,
        })
        .collect()
}

/// Which backend goes first, from the run's seed.
pub fn first_backend(seed: u64) -> BackendKind {
    if splitmix64(seed) & 1 == 0 {
        BackendKind::Wgc
    } else {
        BackendKind::Dda
    }
}

/// SplitMix64: one multiply-xor finaliser, enough to decorrelate adjacent
/// seeds. A whole PRNG for one bit would be ceremony.
fn splitmix64(seed: u64) -> u64 {
    let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(blocks: &[Block]) -> Vec<BackendKind> {
        blocks.iter().map(|block| block.backend).collect()
    }

    #[test]
    fn blocks_alternate_and_are_indexed_in_order() {
        let blocks = alternating(20.0, 5.0, 1);
        assert_eq!(blocks.len(), 4);
        for pair in blocks.windows(2) {
            assert_eq!(
                pair[1].backend,
                pair[0].backend.other(),
                "two blocks of the same backend in a row reintroduce the drift"
            );
            assert_eq!(pair[1].index, pair[0].index + 1);
        }
    }

    #[test]
    fn each_backend_gets_the_same_number_of_blocks() {
        // Nine 2-second blocks would give one backend an extra one, which is
        // exactly the asymmetry the alternation exists to remove.
        let blocks = alternating(19.0, 2.0, 7);
        assert_eq!(blocks.len() % 2, 0);
        let wgc = kinds(&blocks)
            .iter()
            .filter(|kind| **kind == BackendKind::Wgc)
            .count();
        assert_eq!(wgc * 2, blocks.len());
    }

    #[test]
    fn the_whole_requested_time_is_scheduled() {
        let total = 11.0;
        let blocks = alternating(total, 5.0, 3);
        let scheduled: f64 = blocks.iter().map(|block| block.seconds).sum();
        assert!(
            (scheduled - total).abs() < 1e-9,
            "scheduled {scheduled} of {total}"
        );
    }

    #[test]
    fn every_block_is_the_same_length() {
        let blocks = alternating(37.0, 4.0, 11);
        let first = blocks[0].seconds;
        assert!(blocks.iter().all(|block| block.seconds == first));
    }

    #[test]
    fn a_total_shorter_than_two_blocks_still_gives_both_backends_a_turn() {
        let blocks = alternating(1.0, 5.0, 0);
        assert_eq!(blocks.len(), 2);
        assert_ne!(blocks[0].backend, blocks[1].backend);
        assert!(blocks[0].seconds >= MIN_BLOCK_SECONDS);
    }

    #[test]
    fn a_block_shorter_than_the_floor_is_lifted_to_it() {
        let blocks = alternating(100.0, 0.01, 0);
        assert!(blocks.iter().all(|block| block.seconds >= MIN_BLOCK_SECONDS));
    }

    #[test]
    fn the_seed_decides_who_goes_first_and_decides_it_the_same_way_twice() {
        assert_eq!(first_backend(42), first_backend(42));
        assert_eq!(alternating(20.0, 5.0, 42), alternating(20.0, 5.0, 42));
    }

    #[test]
    fn both_backends_can_be_drawn_first() {
        // A "randomised" start that always picks the same backend would leave
        // the first-block effect permanently attached to one API.
        let drawn: Vec<BackendKind> = (0..64).map(first_backend).collect();
        assert!(drawn.contains(&BackendKind::Wgc));
        assert!(drawn.contains(&BackendKind::Dda));
    }

    #[test]
    fn the_schedule_follows_the_drawn_starter() {
        for seed in 0..8 {
            assert_eq!(alternating(10.0, 5.0, seed)[0].backend, first_backend(seed));
        }
    }
}
