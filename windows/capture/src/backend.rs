//! What a desktop capture API is, reduced to what a streamer needs from it.
//!
//! The shape here is dictated by one fact about both APIs: neither of them
//! gives you a texture you may keep. Windows.Graphics.Capture lends you a
//! surface belonging to a frame pool it will recycle, and Desktop Duplication
//! lends you the desktop image until you call `ReleaseFrame`. A type that
//! handed back an owned `ID3D11Texture2D` would be describing an API that
//! does not exist, so [`CapturedFrame`] borrows the backend and is only valid
//! until the next call.
//!
//! Where the release happens follows from that borrow. Desktop Duplication
//! asks callers to keep the gap between `ReleaseFrame` and the next
//! `AcquireNextFrame` as short as possible, because ownership of the surface
//! changes what the duplication does internally. Releasing in a destructor
//! would widen exactly that gap: the caller drops the frame when it has
//! finished reading it, which can be long before it asks for another. So a
//! backend releases at the head of its own `acquire`, immediately before
//! asking for the next frame, and the `&mut self` borrow guarantees the
//! previous frame is already dead by then. The recommended call pattern is
//! the only one the type system permits.

use core::fmt;

use lanplay_telemetry::Timestamp;

/// What the capture API was asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CaptureConfig {
    /// Which output to capture, as an index into the adapter's outputs.
    pub output: u32,
    /// How many surfaces the API should keep in flight. Meaningful for the
    /// Windows.Graphics.Capture frame pool; Desktop Duplication has no such
    /// knob and ignores it.
    pub buffers: u32,
    /// How long an acquire waits for a new frame before reporting
    /// [`Acquired::Timeout`].
    pub acquire_timeout_ms: u32,
    /// Whether to ask the API to draw the mouse cursor into the frame. Off for
    /// the comparison: a cursor is content, and the two APIs composite it at
    /// different points.
    pub cursor: bool,
}

impl Default for CaptureConfig {
    fn default() -> Self {
        CaptureConfig {
            output: 0,
            buffers: 2,
            acquire_timeout_ms: 100,
            cursor: false,
        }
    }
}

/// Which event the source clock marked.
///
/// The two APIs both hand back a QPC-derived instant and they do not mean the
/// same thing by it: one is when the compositor rendered the frame, the other
/// is when the desktop image last changed. Keeping the distinction in the type
/// stops a report from subtracting them and calling the difference a latency.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SourceMark {
    /// `Direct3D11CaptureFrame::SystemRelativeTime` — when the compositor
    /// rendered the frame.
    CompositorRendered(Timestamp),
    /// `DXGI_OUTDUPL_FRAME_INFO::LastPresentTime` — when the desktop image was
    /// last updated.
    DesktopPresented(Timestamp),
}

impl SourceMark {
    pub fn at(self) -> Timestamp {
        match self {
            SourceMark::CompositorRendered(at) | SourceMark::DesktopPresented(at) => at,
        }
    }

    /// A short name for the report column, so a reader can see which event the
    /// delay was measured from without consulting the backend name.
    pub fn describes(self) -> &'static str {
        match self {
            SourceMark::CompositorRendered(_) => "compositor rendered",
            SourceMark::DesktopPresented(_) => "desktop presented",
        }
    }
}

/// Why the capture API returned a frame-sized surface.
///
/// Desktop Duplication reports cursor activity through the same acquire call
/// as desktop presents. Keeping the distinction attached to the frame prevents
/// notification cadence from being mistaken for image cadence.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FrameUpdate {
    /// The desktop image changed.
    #[default]
    Desktop,
    /// Only the hardware pointer changed.
    PointerOnly,
    /// The API returned a surface without either a desktop or pointer mark.
    Other,
}

/// What the API said about the frame beyond the pixels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FrameMetadata {
    /// Desktop Duplication's `AccumulatedFrames`. Greater than one means the
    /// desktop updated more than once while we were busy with the previous
    /// frame, which is the API telling us we are behind. `None` where the API
    /// does not report it.
    pub accumulated_frames: Option<u32>,
    /// Frames the API had ready at the moment of the acquire, where it can be
    /// known. For the WGC frame pool this is pool pressure.
    pub pending: Option<u32>,
    /// Whether this acquisition contains a desktop image, only a pointer
    /// update, or neither.
    pub update: FrameUpdate,
}

/// A frame, on loan.
///
/// The texture belongs to the capture API. It is valid until this value drops
/// and no longer.
pub struct CapturedFrame<'a> {
    pub width: u32,
    pub height: u32,
    /// When the OS says the content came to be.
    pub source: SourceMark,
    /// When our acquire returned, on the same clock.
    pub acquired: Timestamp,
    pub metadata: FrameMetadata,
    /// The API's own surface. Borrowed for exactly as long as this frame.
    pub texture: &'a Texture,
}

impl CapturedFrame<'_> {
    /// How long the frame took to reach us from the event the source marked.
    ///
    /// `None` when the source mark is not usable — Desktop Duplication reports
    /// a zero `LastPresentTime` for a frame carrying only a cursor update, and
    /// a clock that has not been set is not a delay of thirty years.
    pub fn delivery_delay(&self) -> Option<lanplay_telemetry::Nanos> {
        let source = self.source.at();
        if source.as_nanos() == 0 {
            return None;
        }
        self.acquired.since(source)
    }
}

/// The result of asking for the newest frame.
pub enum Acquired<'a> {
    Frame(CapturedFrame<'a>),
    /// Nothing new arrived inside the configured timeout. Normal on a static
    /// desktop, and not an error.
    Timeout,
    /// The capture became invalid and must be rebuilt: a mode change, a
    /// desktop switch, or a fullscreen transition. Expected, not exceptional.
    Lost,
}

/// A capture API, from the streamer's side.
pub trait CaptureBackend {
    /// A name for reports. Stable, because results are compared across runs.
    fn name(&self) -> &'static str;

    fn start(&mut self, config: CaptureConfig) -> Result<(), CaptureError>;

    /// Hands back the newest frame the API has, releasing the previous one.
    ///
    /// Borrowing `self` mutably is the whole design: the previous
    /// [`Acquisition`] must be dropped first, and that drop is the release.
    fn acquire(&mut self) -> Result<Acquired<'_>, CaptureError>;

    /// Tears the capture down. Safe to call when not started.
    fn stop(&mut self);

    /// Rebuilds after [`Acquired::Lost`], keeping the configuration.
    fn restart(&mut self) -> Result<(), CaptureError>;
}

#[derive(Debug)]
pub enum CaptureError {
    /// The machine cannot run this backend at all: no such output, no support
    /// for the API, no device.
    Unsupported(String),
    /// A call the API is not documented to fail made returned a failure.
    Api {
        call: &'static str,
        hresult: i32,
    },
    NotStarted,
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::Unsupported(why) => write!(f, "unsupported: {why}"),
            CaptureError::Api { call, hresult } => {
                write!(f, "{call} failed with 0x{:08X}", *hresult as u32)
            }
            CaptureError::NotStarted => f.write_str("capture has not been started"),
        }
    }
}

impl core::error::Error for CaptureError {}

#[cfg(windows)]
pub type Texture = ::windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

/// Off Windows the crate exists only so the workspace builds; nothing can be
/// captured and no backend is constructible.
#[cfg(not(windows))]
pub type Texture = core::convert::Infallible;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_source_mark_keeps_its_meaning() {
        let compositor = SourceMark::CompositorRendered(Timestamp::from_nanos(10));
        let desktop = SourceMark::DesktopPresented(Timestamp::from_nanos(10));
        assert_eq!(compositor.at(), desktop.at());
        assert_ne!(
            compositor.describes(),
            desktop.describes(),
            "the same instant from two APIs is not the same event"
        );
    }

    #[test]
    fn the_default_pool_is_two_because_that_is_what_is_documented() {
        // One and three are things to measure, not to assume. The baseline is
        // the value Microsoft's own samples use.
        assert_eq!(CaptureConfig::default().buffers, 2);
    }
}
