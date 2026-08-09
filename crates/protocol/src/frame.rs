use core::fmt;
use core::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// Identifies one video frame from capture through present.
///
/// The id is minted once on the host and travels with the frame: it is carried
/// in the RTP header extension, echoed by client telemetry, and used to join
/// host-side and client-side timings for a single frame.
///
/// Zero is reserved as "no frame", so ids start at [`FrameId::FIRST`].
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(transparent)]
pub struct FrameId(u64);

impl FrameId {
    /// Reserved value meaning "no frame".
    pub const NONE: FrameId = FrameId(0);
    /// First id handed out by a fresh [`FrameIdSource`].
    pub const FIRST: FrameId = FrameId(1);

    #[inline]
    pub const fn new(raw: u64) -> Self {
        FrameId(raw)
    }

    #[inline]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[inline]
    pub const fn is_none(self) -> bool {
        self.0 == 0
    }

    /// The next id in sequence. At 120 fps a `u64` lasts longer than the sun.
    #[inline]
    pub const fn next(self) -> Self {
        FrameId(self.0 + 1)
    }
}

impl fmt::Display for FrameId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

/// Hands out monotonically increasing [`FrameId`]s.
///
/// Shared by the capture backend and anything that needs to mint ids for
/// synthetic frames; cloning is not needed because it is used behind an `Arc`
/// or as a `static`.
#[derive(Debug)]
pub struct FrameIdSource(AtomicU64);

impl FrameIdSource {
    pub const fn new() -> Self {
        FrameIdSource(AtomicU64::new(FrameId::FIRST.0))
    }

    /// Mints the next id. Safe to call from any thread; ordering between
    /// threads does not matter, only uniqueness and monotonicity per caller.
    #[inline]
    pub fn next(&self) -> FrameId {
        FrameId(self.0.fetch_add(1, Ordering::Relaxed))
    }
}

impl Default for FrameIdSource {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_starts_at_first_and_increments() {
        let source = FrameIdSource::new();
        assert_eq!(source.next(), FrameId::FIRST);
        assert_eq!(source.next(), FrameId::FIRST.next());
        assert!(!FrameId::FIRST.is_none());
        assert!(FrameId::NONE.is_none());
    }
}
