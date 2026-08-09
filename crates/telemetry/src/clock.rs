//! Monotonic clock shared by every stage on one machine.
//!
//! Requirements that rule out `std::time::Instant`: timestamps must be a plain
//! `u64` that can be stored in a lock-free queue, compared across threads and
//! later put on the wire for host/client clock-offset estimation.
//!
//! Both platform clocks keep counting while the machine sleeps, so a suspended
//! session shows up as one enormous frame age instead of a silent gap.

use core::fmt;

/// Which clock a [`Timestamp`] was read from.
///
/// Two machines' monotonic clocks share no epoch, so subtracting across them
/// is meaningless until phase 8 estimates the offset. The domain travels with
/// each mark instead of each timestamp: the hot path stays a bare `u64`, and
/// the session that records a mark already knows which clock it read.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ClockDomain {
    /// `mach_continuous_time` on the client Mac.
    LocalMac,
    /// `QueryPerformanceCounter` on the host PC.
    LocalWindows,
    /// A portable build; neither target platform.
    Other,
    /// A remote timestamp already corrected onto the local clock.
    Synchronized,
}

impl ClockDomain {
    /// The domain of everything [`Timestamp::now`] returns in this process.
    pub const fn local() -> Self {
        #[cfg(target_os = "macos")]
        {
            ClockDomain::LocalMac
        }
        #[cfg(windows)]
        {
            ClockDomain::LocalWindows
        }
        #[cfg(not(any(target_os = "macos", windows)))]
        {
            ClockDomain::Other
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            ClockDomain::LocalMac => "local-mac",
            ClockDomain::LocalWindows => "local-windows",
            ClockDomain::Other => "local-other",
            ClockDomain::Synchronized => "synchronized",
        }
    }
}

/// A point on the local monotonic clock, in nanoseconds since an arbitrary,
/// process-stable epoch. Only differences are meaningful.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub struct Timestamp(u64);

impl Timestamp {
    /// Reads the platform monotonic clock.
    #[inline]
    pub fn now() -> Self {
        Timestamp(platform::now_nanos())
    }

    #[inline]
    pub const fn from_nanos(nanos: u64) -> Self {
        Timestamp(nanos)
    }

    #[inline]
    pub const fn as_nanos(self) -> u64 {
        self.0
    }

    /// Time elapsed since `earlier`, or `None` if `earlier` is in the future.
    ///
    /// Out-of-order stage marks are a real condition (two threads, one clock,
    /// events queued independently), so the caller must decide what a negative
    /// interval means rather than silently seeing zero.
    #[inline]
    pub const fn since(self, earlier: Timestamp) -> Option<Nanos> {
        match self.0.checked_sub(earlier.0) {
            Some(delta) => Some(Nanos(delta)),
            None => None,
        }
    }

    #[inline]
    pub const fn saturating_since(self, earlier: Timestamp) -> Nanos {
        Nanos(self.0.saturating_sub(earlier.0))
    }

    #[inline]
    pub const fn add(self, delta: Nanos) -> Timestamp {
        Timestamp(self.0 + delta.0)
    }
}

/// Occupies the calling thread until `target`.
///
/// Sleeps most of the way, then spins. Plain sleeping cannot resolve a 120 Hz
/// frame period on macOS, and pure spinning heats the machine being measured.
///
/// The guard band is 3 ms because `thread::sleep` on macOS routinely overshoots
/// by around a millisecond under load: a narrower band leaves the overshoot
/// past the deadline, where it shows up as source jitter and gets blamed on
/// the pipeline.
pub fn wait_until(target: Timestamp) {
    const SPIN_GUARD_NANOS: u64 = 3_000_000;

    loop {
        let now = Timestamp::now();
        if now >= target {
            return;
        }
        let remaining = target.saturating_since(now).get();
        if remaining > SPIN_GUARD_NANOS {
            std::thread::sleep(core::time::Duration::from_nanos(
                remaining - SPIN_GUARD_NANOS,
            ));
        } else {
            core::hint::spin_loop();
        }
    }
}

/// A duration in nanoseconds. Displays as milliseconds, which is the unit the
/// whole project reasons in.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Debug)]
pub struct Nanos(pub u64);

impl Nanos {
    pub const ZERO: Nanos = Nanos(0);

    #[inline]
    pub const fn from_micros(micros: u64) -> Self {
        Nanos(micros * 1_000)
    }

    #[inline]
    pub const fn from_millis(millis: u64) -> Self {
        Nanos(millis * 1_000_000)
    }

    pub fn from_millis_f64(millis: f64) -> Self {
        Nanos((millis * 1_000_000.0).max(0.0) as u64)
    }

    #[inline]
    pub const fn get(self) -> u64 {
        self.0
    }

    pub fn as_millis_f64(self) -> f64 {
        self.0 as f64 / 1_000_000.0
    }

    pub fn as_secs_f64(self) -> f64 {
        self.0 as f64 / 1_000_000_000.0
    }

    pub const fn as_duration(self) -> core::time::Duration {
        core::time::Duration::from_nanos(self.0)
    }
}

impl core::ops::Add for Nanos {
    type Output = Nanos;

    #[inline]
    fn add(self, rhs: Nanos) -> Nanos {
        Nanos(self.0 + rhs.0)
    }
}

impl core::ops::AddAssign for Nanos {
    #[inline]
    fn add_assign(&mut self, rhs: Nanos) {
        self.0 += rhs.0;
    }
}

impl fmt::Display for Nanos {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:.2} ms", self.as_millis_f64())
    }
}

#[cfg(target_os = "macos")]
mod platform {
    use std::sync::LazyLock;

    #[repr(C)]
    #[derive(Clone, Copy, Default)]
    struct MachTimebaseInfo {
        numer: u32,
        denom: u32,
    }

    unsafe extern "C" {
        fn mach_continuous_time() -> u64;
        fn mach_timebase_info(info: *mut MachTimebaseInfo) -> i32;
    }

    static TIMEBASE: LazyLock<(u64, u64)> = LazyLock::new(|| {
        let mut info = MachTimebaseInfo::default();
        // SAFETY: `info` is a valid, correctly sized out-parameter.
        let status = unsafe { mach_timebase_info(&mut info) };
        assert_eq!(status, 0, "mach_timebase_info failed");
        assert!(info.denom != 0, "mach_timebase_info returned denom = 0");
        (u64::from(info.numer), u64::from(info.denom))
    });

    #[inline]
    pub fn now_nanos() -> u64 {
        let (numer, denom) = *TIMEBASE;
        // SAFETY: no arguments, no failure mode.
        let ticks = unsafe { mach_continuous_time() };
        if numer == denom {
            ticks
        } else {
            // 128-bit intermediate: ticks * numer overflows u64 after a few
            // hours on Apple Silicon, where the timebase is not 1:1.
            ((u128::from(ticks) * u128::from(numer)) / u128::from(denom)) as u64
        }
    }
}

#[cfg(windows)]
mod platform {
    use std::sync::LazyLock;

    #[link(name = "kernel32")]
    unsafe extern "system" {
        fn QueryPerformanceCounter(count: *mut i64) -> i32;
        fn QueryPerformanceFrequency(frequency: *mut i64) -> i32;
    }

    static FREQUENCY: LazyLock<u64> = LazyLock::new(|| {
        let mut freq = 0i64;
        // SAFETY: valid out-parameter; the call cannot fail on Windows XP+.
        let ok = unsafe { QueryPerformanceFrequency(&mut freq) };
        assert!(ok != 0 && freq > 0, "QueryPerformanceFrequency failed");
        freq as u64
    });

    #[inline]
    pub fn now_nanos() -> u64 {
        let mut ticks = 0i64;
        // SAFETY: valid out-parameter.
        let ok = unsafe { QueryPerformanceCounter(&mut ticks) };
        debug_assert!(ok != 0);
        let _ = ok;
        ((u128::from(ticks as u64) * 1_000_000_000) / u128::from(*FREQUENCY)) as u64
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
mod platform {
    use std::sync::LazyLock;
    use std::time::Instant;

    static EPOCH: LazyLock<Instant> = LazyLock::new(Instant::now);

    /// Neither target platform, but keeping the crate buildable elsewhere is
    /// worth six lines.
    #[inline]
    pub fn now_nanos() -> u64 {
        EPOCH.elapsed().as_nanos() as u64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clock_is_monotonic_and_advances() {
        let start = Timestamp::now();
        let mut last = start;
        for _ in 0..10_000 {
            let now = Timestamp::now();
            assert!(now >= last, "clock went backwards: {now:?} < {last:?}");
            last = now;
        }
        assert!(
            last.saturating_since(start).get() > 0,
            "clock did not advance over 10k reads"
        );
    }

    #[test]
    fn since_reports_none_for_reversed_order() {
        let early = Timestamp::from_nanos(100);
        let late = Timestamp::from_nanos(250);
        assert_eq!(late.since(early), Some(Nanos(150)));
        assert_eq!(early.since(late), None);
        assert_eq!(early.saturating_since(late), Nanos::ZERO);
    }

    #[test]
    fn nanos_display_uses_milliseconds() {
        assert_eq!(Nanos(1_100_000).to_string(), "1.10 ms");
        assert_eq!(Nanos(70_000).to_string(), "0.07 ms");
    }
}
