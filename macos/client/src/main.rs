//! The phase 2 experiment.
//!
//! One process: an H.264 fixture is paced into a hardware VideoToolbox
//! decoder, whose IOSurface-backed frames go into a latest-frame-wins slot,
//! which a Metal presenter drains on the main thread. No capture, no network,
//! no encoder. If this cannot hold 1080p120 with a flat backlog, nothing built
//! on top of it can.

mod config;
mod dscp;
mod gate;
mod nap;
mod phase;
mod preflight;
mod report;
mod session;
mod transport;
mod windows;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, ValueEnum};
use lanplay_renderer_metal::DriveMode;
use lanplay_video_core::{FixturePattern, FixtureSpec};

#[derive(Parser)]
#[command(
    name = "lanplay-client",
    about = "phase 2: fixture -> VideoToolbox -> Metal"
)]
pub struct Cli {
    /// Fixture content. `motion` carries a readable frame counter.
    #[arg(long, value_enum, default_value_t = Pattern::Motion)]
    pub pattern: Pattern,
    #[arg(long, default_value_t = 1920)]
    pub width: u32,
    #[arg(long, default_value_t = 1080)]
    pub height: u32,
    /// Rate the fixture is encoded at, and the rate it is fed at unless
    /// `--feed-fps` overrides it.
    #[arg(long, default_value_t = 120)]
    pub fps: u32,
    /// Length of the encoded fixture. Runs longer than this loop it.
    #[arg(long, default_value_t = 10)]
    pub fixture_seconds: u32,
    #[arg(long, default_value = "fixtures")]
    pub fixture_dir: PathBuf,

    /// How long to run.
    #[arg(long, default_value_t = 60.0)]
    pub seconds: f64,
    /// Feed rate, when it must differ from the fixture rate. Feeding faster
    /// than the encoder rate is the decoder overload test.
    #[arg(long)]
    pub feed_fps: Option<f64>,
    /// How access units reach the decoder. `loopback` inserts the real RTP
    /// packetiser, a UDP socket and the depacketiser between fixture and
    /// decoder; the delta against `direct` is the cost of our transport.
    #[arg(long, value_enum, default_value_t = Transport::Direct)]
    pub transport: Transport,
    /// Bytes per datagram, RTP header included.
    #[arg(long, default_value_t = lanplay_transport::MAX_UDP_PAYLOAD)]
    pub mtu: usize,
    /// Address to receive RTP on, for `--transport lan`.
    #[arg(long, default_value = "0.0.0.0:5004")]
    pub bind: std::net::SocketAddr,
    /// Where the host serves its codec configuration. Required for
    /// `--transport lan`: the decoder is built from the host's parameter
    /// sets, never from the fixture's.
    #[arg(long)]
    pub control: Option<std::net::SocketAddr>,
    /// Where the decoder's parameter sets come from. `fixture` exists to
    /// reproduce the failure that motivated the control plane: decoding a
    /// live stream against another encoder's SPS/PPS. It is the negative
    /// control, and a run using it is expected to fail.
    #[arg(long, value_enum, default_value_t = ParameterSetSource::Host)]
    pub parameter_sets: ParameterSetSource,
    /// Measure the link and stop there: no renderer, no window, no display
    /// link.
    ///
    /// Delivery cadence, loss, reordering and decode are all measured before
    /// anything touches a screen, so a radio experiment has no business
    /// depending on what the Mac's display is doing. Runs that rank on
    /// presentation need the window; runs that rank on the link must not.
    #[arg(long)]
    pub link_only: bool,
    /// Compare every reconstructed access unit against the original by
    /// SHA-256, rather than trusting that the decoder did not complain.
    #[arg(long)]
    pub verify: bool,
    /// Refuse to run unless the display can present at the source rate and
    /// the window is unoccluded, on the active Space and not minimised.
    #[arg(long)]
    pub require_clean_display: bool,
    /// Write the machine-readable result here.
    #[arg(long)]
    pub report: Option<PathBuf>,
    /// Length of each rolling window in the report.
    #[arg(long, default_value_t = 10.0)]
    pub window_seconds: f64,
    #[arg(long, value_enum, default_value_t = Mode::DisplayLink)]
    pub mode: Mode,
    /// Ask the host to hold a capture tick back so each frame becomes ready
    /// just before this display wants one, and keep asking as the two clocks
    /// drift apart.
    ///
    /// `off` is the negative control for the mechanism. A threshold on
    /// presentation wait cannot tell alignment from luck, so the claim is only
    /// falsifiable if an unaligned arm of the same run can be measured beside
    /// the aligned one: the unaligned one has to sit at half a refresh period,
    /// which is where an arbitrary phase between two unsynchronised clocks puts
    /// it.
    ///
    /// `observe` measures and reports exactly as `on` does and sends nothing. It
    /// is the control for the phase rather than for the mechanism: a run's
    /// starting phase is an independent draw, so an arm that happens to begin
    /// where alignment aims proves a favourable draw, while an untouched arm
    /// shows the whole distribution the acting arm has to be read against. It
    /// also needs no host, which is what makes it the place to settle by
    /// experiment which way a held tick moves the phase measured here.
    #[arg(long, value_enum, default_value_t = PhaseAlign::Observe)]
    pub phase_align: PhaseAlign,

    /// Burn this long in the renderer before presenting. Proves
    /// latest-frame-wins bounds latency when the consumer is slow.
    #[arg(long)]
    pub render_delay_ms: Option<u64>,
    /// Sleep this long inside the decoder's output callback. Proves a stall
    /// there is attributed to the decoder and not to something else.
    #[arg(long)]
    pub decoder_callback_delay_ms: Option<u64>,
    /// Run without requiring a hardware decoder. Only for demonstrating that
    /// the requirement is real; a run with this set cannot pass the gate.
    #[arg(long)]
    pub allow_software_decoder: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Pattern {
    Motion,
    Detail,
}

/// Which encoder's parameter sets configure the decoder.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ParameterSetSource {
    /// The sequence header of the encoder actually producing the stream,
    /// carried by the control plane. The only correct answer.
    Host,
    /// The local fixture's. Wrong by construction for a live stream; kept so
    /// the failure it causes can be demonstrated rather than argued about.
    Fixture,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Render as soon as a frame arrives.
    Immediate,
    /// Render when the display asks, through `CAMetalDisplayLink`.
    DisplayLink,
}

/// Whether the receiver measures the capture phase and asks the host to move
/// it.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum PhaseAlign {
    /// Measure and ask.
    On,
    /// Measure and stay silent.
    Observe,
    /// Neither.
    Off,
}

/// How access units reach the decoder.
#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Transport {
    /// Straight from the fixture, as phase 2 ran it.
    Direct,
    /// Through RTP over a UDP socket on the loopback interface.
    Loopback,
    /// Receive RTP from another machine. The sender is elsewhere; the fixture
    /// is still read locally, for its parameter sets only.
    Lan,
}

impl Cli {
    fn fixture_spec(&self) -> FixtureSpec {
        FixtureSpec {
            pattern: match self.pattern {
                Pattern::Motion => FixturePattern::Motion,
                Pattern::Detail => FixturePattern::Detail,
            },
            width: self.width,
            height: self.height,
            fps: self.fps,
            seconds: self.fixture_seconds,
            bitrate_mbps: 50,
            gop: self.fps,
        }
    }

    fn drive_mode(&self) -> DriveMode {
        match self.mode {
            Mode::Immediate => DriveMode::Immediate,
            Mode::DisplayLink => DriveMode::DisplayLink,
        }
    }

    fn feed_fps(&self) -> f64 {
        self.feed_fps.unwrap_or(f64::from(self.fps))
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match session::run(&cli) {
        Ok(passed) if passed => ExitCode::SUCCESS,
        Ok(_) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("client: {error}");
            ExitCode::FAILURE
        }
    }
}
