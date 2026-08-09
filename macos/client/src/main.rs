//! The phase 2 experiment.
//!
//! One process: an H.264 fixture is paced into a hardware VideoToolbox
//! decoder, whose IOSurface-backed frames go into a latest-frame-wins slot,
//! which a Metal presenter drains on the main thread. No capture, no network,
//! no encoder. If this cannot hold 1080p120 with a flat backlog, nothing built
//! on top of it can.

mod gate;
mod nap;
mod session;
mod transport;

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
    /// Compare every reconstructed access unit against the original by
    /// SHA-256, rather than trusting that the decoder did not complain.
    #[arg(long)]
    pub verify: bool,
    #[arg(long, value_enum, default_value_t = Mode::DisplayLink)]
    pub mode: Mode,

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

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Mode {
    /// Render as soon as a frame arrives.
    Immediate,
    /// Render when the display asks, through `CAMetalDisplayLink`.
    DisplayLink,
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
