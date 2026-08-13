//! A deliberately unreliable relay, for proving that input survives a bad
//! link rather than asserting that it would.
//!
//! Everything measured so far crossed a LAN that lost nothing, so the
//! retransmission and snapshot machinery has never actually been needed. Code
//! that has never run is not known to work, and the failure it guards against
//! is the worst one available: a key release lost once leaves a player walking
//! into a wall until they notice and press the key again.
//!
//! Sits between the two halves and does to their datagrams what a congested
//! access point would:
//!
//! ```text
//! client ──► 127.0.0.1:5106 ──► host:5006
//!        ◄──               ◄──
//! ```
//!
//! Faults apply in both directions, because losing an acknowledgement is a
//! different failure from losing the event it acknowledges, and only one of
//! them can be repaired by retransmitting.
//!
//! The generator is seeded and its own, not the system's, so a run that
//! exposes a bug can be repeated exactly. A fault injector whose faults cannot
//! be reproduced turns a bug into a rumour.
//!
//! usage:
//!   udp-fault --forward <addr:port> [--listen 127.0.0.1:5106]
//!             [--loss 1.0] [--duplicate 0.5] [--reorder 2.0]
//!             [--stall-ms 50] [--stall-every-ms 5000] [--seed 1]

use std::collections::VecDeque;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// What was done to the traffic, so a run's outcome can be attributed.
#[derive(Default)]
struct Counts {
    seen: u64,
    dropped: u64,
    duplicated: u64,
    reordered: u64,
    stalled: u64,
}

impl Counts {
    fn report(&self, elapsed: Duration, holding: usize) {
        let share = |count: u64| {
            if self.seen == 0 {
                0.0
            } else {
                count as f64 * 100.0 / self.seen as f64
            }
        };
        println!(
            "{:>5.0}s  seen {:>7}  dropped {:>6} ({:.2}%)  duplicated {:>6} ({:.2}%)  \
             reordered {:>6} ({:.2}%)  stalls {:>4}  holding {holding}",
            elapsed.as_secs_f64(),
            self.seen,
            self.dropped,
            share(self.dropped),
            self.duplicated,
            share(self.duplicated),
            self.reordered,
            share(self.reordered),
            self.stalled
        );
    }
}

/// Everything held back, with the instant it is due.
struct Delayed {
    due: Instant,
    to_host: bool,
    bytes: Vec<u8>,
}

/// Percentages, taken as they are given rather than clamped silently: asking
/// for 200% loss is a mistake worth reporting.
struct Faults {
    loss: f64,
    duplicate: f64,
    reorder: f64,
    reorder_hold: Duration,
    stall: Duration,
    stall_every: Duration,
}

/// xorshift64star. A dependency would be heavier than the algorithm, and the
/// only property needed is that the same seed gives the same sequence.
struct Rng(u64);

impl Rng {
    fn next_percent(&mut self) -> f64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        let value = self.0.wrapping_mul(0x2545_F491_4F6C_DD1D);
        // The top 53 bits, which is all a double can hold exactly.
        ((value >> 11) as f64 / (1u64 << 53) as f64) * 100.0
    }
}

fn main() {
    let mut listen: SocketAddr = "127.0.0.1:5106".parse().expect("valid default");
    let mut forward: Option<SocketAddr> = None;
    let mut faults = Faults {
        loss: 0.0,
        duplicate: 0.0,
        reorder: 0.0,
        reorder_hold: Duration::from_millis(8),
        stall: Duration::ZERO,
        stall_every: Duration::from_secs(5),
    };
    let mut seed = 1u64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut number = || {
            args.next()
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or_default()
        };
        match arg.as_str() {
            "--listen" => listen = parse_addr(&mut args, "--listen"),
            "--forward" => forward = Some(parse_addr(&mut args, "--forward")),
            "--loss" => faults.loss = number(),
            "--duplicate" => faults.duplicate = number(),
            "--reorder" => faults.reorder = number(),
            "--reorder-hold-ms" => faults.reorder_hold = Duration::from_millis(number() as u64),
            "--stall-ms" => faults.stall = Duration::from_millis(number() as u64),
            "--stall-every-ms" => faults.stall_every = Duration::from_millis(number() as u64),
            "--seed" => seed = number() as u64,
            other => {
                eprintln!("unknown argument {other}");
                std::process::exit(2);
            }
        }
    }
    let Some(forward) = forward else {
        eprintln!("--forward <addr:port> is required");
        std::process::exit(2);
    };
    for (name, value) in [
        ("loss", faults.loss),
        ("duplicate", faults.duplicate),
        ("reorder", faults.reorder),
    ] {
        if !(0.0..=100.0).contains(&value) {
            eprintln!("--{name} {value} is not a percentage");
            std::process::exit(2);
        }
    }

    let socket = match UdpSocket::bind(listen) {
        Ok(socket) => socket,
        Err(error) => {
            eprintln!("{listen}: {error}");
            std::process::exit(1);
        }
    };
    // Short enough that a held datagram is released close to when it is due,
    // long enough that the loop is not a spin. Nothing here is on the input
    // path: this process exists to be in the way.
    socket
        .set_read_timeout(Some(Duration::from_millis(2)))
        .expect("timeout on a fresh socket");

    println!(
        "udp-fault: {listen} -> {forward}, loss {:.1}%, duplicate {:.1}%, \
         reorder {:.1}% held {} ms, stall {} ms every {} ms, seed {seed}",
        faults.loss,
        faults.duplicate,
        faults.reorder,
        faults.reorder_hold.as_millis(),
        faults.stall.as_millis(),
        faults.stall_every.as_millis()
    );

    let mut rng = Rng(seed | 1);
    let mut client: Option<SocketAddr> = None;
    let mut held: VecDeque<Delayed> = VecDeque::new();
    let mut buffer = [0u8; 2048];
    let mut counts = Counts::default();
    let start = Instant::now();
    let mut stall_until = start;
    let mut next_stall = start + faults.stall_every;
    // Reported as it goes rather than at exit, because this process is killed
    // by whatever harness started it and a summary printed on the way out
    // would never be seen. Without it there is no way to tell a run that lost
    // one percent from one that lost nothing.
    let mut next_report = start + Duration::from_secs(5);

    loop {
        let now = Instant::now();
        if now >= next_report {
            counts.report(start.elapsed(), held.len());
            next_report = now + Duration::from_secs(5);
        }

        // A stall is a window in which nothing moves at all, which is what an
        // access point going off channel does. Applied before anything else so
        // that held datagrams stay held through it too.
        if !faults.stall.is_zero() && now >= next_stall {
            stall_until = now + faults.stall;
            next_stall = now + faults.stall_every;
            counts.stalled += 1;
        }
        let stalled = now < stall_until;

        if !stalled {
            while let Some(front) = held.front() {
                if front.due > now {
                    break;
                }
                let item = held.pop_front().expect("front exists");
                deliver(&socket, &item.bytes, item.to_host, forward, client);
            }
        }

        let (len, from) = match socket.recv_from(&mut buffer) {
            Ok(result) => result,
            // A timeout is how the loop gets to release held datagrams.
            Err(_) => continue,
        };
        counts.seen += 1;
        let to_host = from != forward;
        if to_host {
            client = Some(from);
        }
        let bytes = buffer[..len].to_vec();

        if stalled {
            // Not dropped: held until the stall ends, which is what a radio
            // does. Dropping here would test loss instead of bunching.
            held.push_back(Delayed {
                due: stall_until,
                to_host,
                bytes,
            });
            continue;
        }
        if rng.next_percent() < faults.loss {
            counts.dropped += 1;
            continue;
        }
        if rng.next_percent() < faults.reorder {
            counts.reordered += 1;
            held.push_back(Delayed {
                due: now + faults.reorder_hold,
                to_host,
                bytes,
            });
            continue;
        }
        deliver(&socket, &bytes, to_host, forward, client);
        if rng.next_percent() < faults.duplicate {
            counts.duplicated += 1;
            deliver(&socket, &bytes, to_host, forward, client);
        }
    }
}

fn deliver(
    socket: &UdpSocket,
    bytes: &[u8],
    to_host: bool,
    forward: SocketAddr,
    client: Option<SocketAddr>,
) {
    let target = if to_host { Some(forward) } else { client };
    if let Some(target) = target {
        // A failed send is the network's business, not this program's: it is
        // pretending to be a bad network and a refused datagram is one more
        // way of being one.
        let _ = socket.send_to(bytes, target);
    }
}

fn parse_addr(args: &mut impl Iterator<Item = String>, flag: &str) -> SocketAddr {
    match args.next().and_then(|value| value.parse().ok()) {
        Some(addr) => addr,
        None => {
            eprintln!("{flag} needs an address like 127.0.0.1:5006");
            std::process::exit(2);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_same_seed_gives_the_same_sequence() {
        // The property the whole tool rests on. A run that exposes a bug has to
        // be repeatable, or the bug is a rumour.
        let first: Vec<f64> = (0..16).map(|_| Rng(7).next_percent()).collect();
        let mut a = Rng(7);
        let mut b = Rng(7);
        for _ in 0..16 {
            assert_eq!(a.next_percent(), b.next_percent());
        }
        assert_eq!(first.len(), 16);
    }

    #[test]
    fn percentages_stay_inside_their_range() {
        let mut rng = Rng(12345);
        for _ in 0..10_000 {
            let value = rng.next_percent();
            assert!((0.0..100.0).contains(&value), "{value}");
        }
    }

    #[test]
    fn a_rate_of_zero_never_fires_and_a_hundred_always_does() {
        // The two settings every sweep starts and ends with, and the only two
        // where an off-by-one in the comparison would be invisible in a rate.
        let mut rng = Rng(99);
        let mut fired_at_zero = 0;
        let mut fired_at_hundred = 0;
        for _ in 0..1000 {
            if rng.next_percent() < 0.0 {
                fired_at_zero += 1;
            }
            if rng.next_percent() < 100.0 {
                fired_at_hundred += 1;
            }
        }
        assert_eq!(fired_at_zero, 0);
        assert_eq!(fired_at_hundred, 1000);
    }
}
