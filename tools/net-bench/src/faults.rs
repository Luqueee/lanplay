//! Deliberate network misbehaviour, applied after packetisation so the
//! receiver sees exactly the datagram stream a lossy link would deliver.
//!
//! Rates are parts per million because the interesting failures are rare: a
//! soak needs to exercise one dropped packet in ten thousand, and a percentage
//! knob cannot express that. Everything is driven from one seed, so a failure
//! found at 03:00 is reproducible at 09:00.

use core::fmt;

use lanplay_transport::HEADER_OVERHEAD;

/// Datagrams a held packet waits before release. Three is enough to land it
/// behind the next access unit's first packets on a fast link, which is the
/// reordering a real switch produces.
const REORDER_DELAY: u32 = 3;

/// Packets the delay line can hold at once. Fixed, and the buffers are
/// allocated up front: an injector that grew when reordering got common would
/// be measuring its own allocator.
const REORDER_SLOTS: usize = 4;

const PPM: u32 = 1_000_000;

/// xorshift64*. Not cryptographic, and does not need to be: it decides whether
/// packet 4_712_003 is unlucky, and it must decide the same way every run.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        // A zero state is xorshift's fixed point and would emit nothing but
        // zeroes, turning every fault rate into 100%.
        Rng(if seed == 0 {
            0x9E37_79B9_7F4A_7C15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// True with probability `threshold / 1_000_000`.
    fn hits(&mut self, threshold: u32) -> bool {
        if threshold == 0 {
            return false;
        }
        ((self.next_u64() >> 32) as u32) % PPM < threshold
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FaultConfig {
    pub drop_ppm: u32,
    pub duplicate_ppm: u32,
    pub reorder_ppm: u32,
    pub corrupt_ppm: u32,
    pub seed: u64,
}

impl FaultConfig {
    pub fn is_enabled(&self) -> bool {
        self.drop_ppm | self.duplicate_ppm | self.reorder_ppm | self.corrupt_ppm != 0
    }
}

impl fmt::Display for FaultConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "drop {} ppm, duplicate {} ppm, reorder {} ppm, corrupt {} ppm, seed {:#x}",
            self.drop_ppm, self.duplicate_ppm, self.reorder_ppm, self.corrupt_ppm, self.seed
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct FaultStats {
    pub dropped: u64,
    pub duplicated: u64,
    pub reordered: u64,
    /// Reorders declined because the delay line was full. Counted rather than
    /// grown into, so the injector's memory is a constant.
    pub reorder_declined: u64,
    pub corrupted_header: u64,
    pub corrupted_payload: u64,
}

impl fmt::Display for FaultStats {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "dropped {}, duplicated {}, reordered {} (declined {}), corrupted {} header + {} payload",
            self.dropped,
            self.duplicated,
            self.reordered,
            self.reorder_declined,
            self.corrupted_header,
            self.corrupted_payload
        )
    }
}

struct Held {
    bytes: Vec<u8>,
    len: usize,
    release_in: u32,
}

pub struct FaultInjector {
    config: FaultConfig,
    rng: Rng,
    stats: FaultStats,
    /// Corruption needs a mutable copy; the packetiser's slice is not ours.
    scratch: Vec<u8>,
    held: [Held; REORDER_SLOTS],
}

impl FaultInjector {
    pub fn new(config: FaultConfig, mtu: usize) -> Self {
        FaultInjector {
            config,
            rng: Rng::new(config.seed),
            stats: FaultStats::default(),
            scratch: vec![0; mtu],
            held: core::array::from_fn(|_| Held {
                bytes: vec![0; mtu],
                len: 0,
                release_in: 0,
            }),
        }
    }

    pub fn stats(&self) -> &FaultStats {
        &self.stats
    }

    /// Offers one packetised datagram and hands whatever should go on the wire
    /// to `send`, in transmission order.
    pub fn offer(&mut self, packet: &[u8], mut send: impl FnMut(&[u8])) {
        self.release_due(&mut send);

        if self.rng.hits(self.config.drop_ppm) {
            self.stats.dropped += 1;
            return;
        }

        if self.rng.hits(self.config.reorder_ppm) {
            if self.hold(packet) {
                return;
            }
            self.stats.reorder_declined += 1;
        }

        self.emit(packet, &mut send);
    }

    /// Releases everything still held. End of stream is not a reason to lose a
    /// packet the injector only meant to delay.
    pub fn flush(&mut self, mut send: impl FnMut(&[u8])) {
        for slot in &mut self.held {
            if slot.release_in > 0 {
                slot.release_in = 0;
                send(&slot.bytes[..slot.len]);
            }
        }
    }

    fn release_due(&mut self, send: &mut impl FnMut(&[u8])) {
        for slot in &mut self.held {
            if slot.release_in == 0 {
                continue;
            }
            slot.release_in -= 1;
            if slot.release_in == 0 {
                send(&slot.bytes[..slot.len]);
            }
        }
    }

    fn hold(&mut self, packet: &[u8]) -> bool {
        let Some(slot) = self
            .held
            .iter_mut()
            .find(|slot| slot.release_in == 0 && slot.bytes.len() >= packet.len())
        else {
            return false;
        };
        slot.bytes[..packet.len()].copy_from_slice(packet);
        slot.len = packet.len();
        slot.release_in = REORDER_DELAY;
        self.stats.reordered += 1;
        true
    }

    fn emit(&mut self, packet: &[u8], send: &mut impl FnMut(&[u8])) {
        // Both draws happen before the corrupted copy borrows `self`, which
        // also keeps the RNG sequence independent of which faults fired.
        let corrupt = self.rng.hits(self.config.corrupt_ppm) && packet.len() > HEADER_OVERHEAD;
        let duplicate = self.rng.hits(self.config.duplicate_ppm);
        if duplicate {
            self.stats.duplicated += 1;
        }
        let bytes = if corrupt {
            self.corrupt(packet)
        } else {
            packet
        };
        send(bytes);
        if duplicate {
            send(bytes);
        }
    }

    /// Flips one byte, half the time in the RTP header and half in the
    /// payload.
    ///
    /// A uniform choice across the datagram would put 98% of the damage in the
    /// payload, where RTP has nothing to say about it: the header is 28 bytes
    /// of 1200, and header corruption is the case that exercises the parser.
    fn corrupt(&mut self, packet: &[u8]) -> &[u8] {
        self.scratch[..packet.len()].copy_from_slice(packet);
        let in_header = self.rng.next_u64() & 1 == 0;
        let index = if in_header {
            self.stats.corrupted_header += 1;
            self.rng.below(HEADER_OVERHEAD)
        } else {
            self.stats.corrupted_payload += 1;
            HEADER_OVERHEAD + self.rng.below(packet.len() - HEADER_OVERHEAD)
        };
        // XOR with a non-zero byte: a bit flip leaves the two-bit RTP version
        // field intact seven times in eight, so the parser would almost never
        // see the malformed packet this is meant to produce.
        let mask = 1 + (self.rng.next_u64() % 255) as u8;
        self.scratch[index] ^= mask;
        &self.scratch[..packet.len()]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn packet(fill: u8) -> Vec<u8> {
        vec![fill; 200]
    }

    #[test]
    fn zero_rates_pass_every_packet_through_untouched() {
        let mut injector = FaultInjector::new(FaultConfig::default(), 1200);
        let source = packet(7);
        let mut seen = Vec::new();
        for _ in 0..1000 {
            injector.offer(&source, |bytes| seen.push(bytes.to_vec()));
        }
        assert_eq!(seen.len(), 1000);
        assert!(seen.iter().all(|bytes| *bytes == source));
        assert_eq!(injector.stats().dropped, 0);
    }

    #[test]
    fn drop_rate_lands_near_the_requested_parts_per_million() {
        let config = FaultConfig {
            drop_ppm: 100_000,
            seed: 1,
            ..FaultConfig::default()
        };
        let mut injector = FaultInjector::new(config, 1200);
        let source = packet(1);
        let mut sent = 0u64;
        for _ in 0..100_000 {
            injector.offer(&source, |_| sent += 1);
        }
        let dropped = injector.stats().dropped;
        assert_eq!(sent + dropped, 100_000);
        assert!(
            (9_000..=11_000).contains(&dropped),
            "10% of 100k should drop ~10000, got {dropped}"
        );
    }

    #[test]
    fn the_same_seed_produces_the_same_stream() {
        let config = FaultConfig {
            drop_ppm: 50_000,
            duplicate_ppm: 50_000,
            reorder_ppm: 50_000,
            corrupt_ppm: 50_000,
            seed: 0xABCD,
        };
        let run = || {
            let mut injector = FaultInjector::new(config, 1200);
            let mut seen = Vec::new();
            for index in 0..5_000u32 {
                let mut source = packet(0);
                source[HEADER_OVERHEAD..HEADER_OVERHEAD + 4].copy_from_slice(&index.to_be_bytes());
                injector.offer(&source, |bytes| seen.push(bytes.to_vec()));
            }
            injector.flush(|bytes| seen.push(bytes.to_vec()));
            seen
        };
        assert_eq!(run(), run());
    }

    #[test]
    fn reordering_delays_a_packet_without_losing_it() {
        let config = FaultConfig {
            reorder_ppm: 200_000,
            seed: 99,
            ..FaultConfig::default()
        };
        let mut injector = FaultInjector::new(config, 1200);
        let mut seen = Vec::new();
        for index in 0..2_000u32 {
            let mut source = packet(0);
            source[0..4].copy_from_slice(&index.to_be_bytes());
            injector.offer(&source, |bytes| {
                seen.push(u32::from_be_bytes(bytes[0..4].try_into().expect("4 bytes")))
            });
        }
        injector
            .flush(|bytes| seen.push(u32::from_be_bytes(bytes[0..4].try_into().expect("4 bytes"))));

        assert_eq!(seen.len(), 2_000, "reordering must not lose packets");
        let mut sorted = seen.clone();
        sorted.sort_unstable();
        assert!(sorted.windows(2).all(|pair| pair[0] + 1 == pair[1]));
        assert!(
            seen.windows(2).any(|pair| pair[0] > pair[1]),
            "some packet must actually arrive out of order"
        );
    }

    #[test]
    fn corruption_changes_exactly_one_byte() {
        let config = FaultConfig {
            corrupt_ppm: 1_000_000,
            seed: 5,
            ..FaultConfig::default()
        };
        let mut injector = FaultInjector::new(config, 1200);
        let source = packet(0x5A);
        for _ in 0..500 {
            injector.offer(&source, |bytes| {
                assert_eq!(bytes.len(), source.len());
                let differing = bytes
                    .iter()
                    .zip(&source)
                    .filter(|(sent, original)| sent != original)
                    .count();
                assert_eq!(differing, 1);
            });
        }
        let stats = injector.stats();
        assert_eq!(stats.corrupted_header + stats.corrupted_payload, 500);
        assert!(stats.corrupted_header > 0 && stats.corrupted_payload > 0);
    }
}
