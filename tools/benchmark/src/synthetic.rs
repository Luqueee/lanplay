//! A synthetic pipeline with the same thread topology as the real one.
//!
//! Two threads, one per machine, joined by a channel that models wire delay.
//! Every stage burns a configurable amount of time, so the harness answers
//! "what does this latency budget look like end to end, and does the
//! instrumentation survive it" long before any GPU is involved.

use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;
use std::time::Duration;

use clap::Args;
use lanplay_protocol::{FrameIdSource, VideoMode};
use lanplay_telemetry::{
    FrameTimeline, Nanos, Recorder, Snapshot, Stage, Telemetry, TelemetryConfig, Timestamp,
};

#[derive(Args, Debug, Clone)]
pub struct SyntheticArgs {
    /// Source cadence in frames per second.
    #[arg(long, default_value_t = 120.0)]
    pub fps: f64,
    /// How long to run.
    #[arg(long, default_value_t = 5.0)]
    pub seconds: f64,
    /// Frame width, reported only in the header.
    #[arg(long, default_value_t = 1920)]
    pub width: u32,
    /// Frame height, reported only in the header.
    #[arg(long, default_value_t = 1080)]
    pub height: u32,

    /// frame_created -> capture_acquired.
    #[arg(long, default_value_t = 1.10)]
    pub capture_ms: f64,
    /// GPU preprocess. Zero disables the stage instead of recording a zero.
    #[arg(long, default_value_t = 0.0)]
    pub preprocess_ms: f64,
    /// encode_submit -> encode_complete.
    #[arg(long, default_value_t = 1.80)]
    pub encode_ms: f64,
    /// packetization_start -> network_send_first.
    #[arg(long, default_value_t = 0.07)]
    pub packetize_ms: f64,
    /// network_send_first -> network_send_last.
    #[arg(long, default_value_t = 0.05)]
    pub send_ms: f64,
    /// Wire delay: network_send_last -> network_receive_first.
    #[arg(long, default_value_t = 0.42)]
    pub network_ms: f64,
    /// network_receive_first -> network_receive_last.
    #[arg(long, default_value_t = 0.04)]
    pub receive_ms: f64,
    /// network_receive_last -> frame_reassembled.
    #[arg(long, default_value_t = 0.09)]
    pub reassembly_ms: f64,
    /// decode_submit -> decode_complete.
    #[arg(long, default_value_t = 1.60)]
    pub decode_ms: f64,
    /// render_submit -> present_submit.
    #[arg(long, default_value_t = 0.31)]
    pub render_ms: f64,
    /// Uniform jitter added to every stage, in milliseconds.
    #[arg(long, default_value_t = 0.05)]
    pub jitter_ms: f64,
    /// Seed for the jitter generator; runs are reproducible.
    #[arg(long, default_value_t = 0x5EED)]
    pub seed: u64,

    /// Emit an aggregate line every N milliseconds while running.
    #[arg(long)]
    pub live_ms: Option<u64>,
    /// Dump this frame's timeline instead of the last one.
    #[arg(long)]
    pub frame: Option<u64>,
}

impl SyntheticArgs {
    fn mode(&self) -> VideoMode {
        VideoMode::from_hz(self.width, self.height, self.fps)
    }

    fn frame_count(&self) -> u64 {
        (self.fps * self.seconds).round().max(1.0) as u64
    }

    fn period(&self) -> Nanos {
        Nanos((1_000_000_000.0 / self.fps) as u64)
    }
}

pub struct Run {
    pub snapshot: Snapshot,
    pub sample: Option<FrameTimeline>,
    pub mode: VideoMode,
    pub expected_frames: u64,
}

pub fn run(args: &SyntheticArgs) -> Run {
    let mut config = TelemetryConfig::default();
    if let Some(interval) = args.live_ms {
        config.report_interval = Some(Duration::from_millis(interval));
        config.reporter = Some(Box::new(|snapshot: &Snapshot| {
            eprintln!(
                "[live] presented {:>6}  {:>6.1}/s  age p99 {:>6.2} ms  dropped {}",
                snapshot.counters.frames_presented,
                snapshot.presented_per_second(),
                snapshot.frame_age.p99.as_millis_f64(),
                snapshot.counters.events_dropped,
            );
        }));
    }
    let telemetry = Telemetry::start(config);

    let (sender, receiver) = mpsc::channel::<Wire>();
    let host = spawn(
        "host",
        host_loop(telemetry.recorder(), args.clone(), sender),
    );
    let client = spawn(
        "client",
        client_loop(telemetry.recorder(), args.clone(), receiver),
    );
    host.join().expect("host thread");
    client.join().expect("client thread");

    assert!(
        telemetry.flush(Duration::from_secs(5)),
        "collector did not catch up"
    );
    let sample = match args.frame {
        Some(id) => telemetry.frame(lanplay_protocol::FrameId::new(id)),
        None => telemetry.last_frame(),
    };

    Run {
        snapshot: telemetry.shutdown(),
        sample,
        mode: args.mode(),
        expected_frames: args.frame_count(),
    }
}

fn spawn<F>(name: &str, body: F) -> thread::JoinHandle<()>
where
    F: FnOnce() + Send + 'static,
{
    thread::Builder::new()
        .name(name.to_owned())
        .spawn(body)
        .expect("spawn pipeline thread")
}

/// What crosses the wire: a frame id and the instant it should land.
struct Wire {
    frame: lanplay_protocol::FrameId,
    arrival: Timestamp,
}

fn host_loop(
    recorder: Recorder,
    args: SyntheticArgs,
    sender: Sender<Wire>,
) -> impl FnOnce() + Send + 'static {
    move || {
        let frames = FrameIdSource::new();
        let mut jitter = Jitter::new(args.seed, args.jitter_ms);
        let period = args.period();
        let start = Timestamp::now();

        for tick in 0..args.frame_count() {
            // Absolute pacing: a slow frame does not push every later frame.
            let due = start.add(Nanos(period.get() * tick));
            wait_until(due);

            let frame = frames.next();
            recorder.mark(frame, Stage::FrameCreated);

            // Capture: the backend signals availability part-way through.
            let capture = jitter.of(args.capture_ms);
            burn(Nanos(capture.get() * 2 / 3));
            recorder.mark(frame, Stage::CaptureAvailable);
            burn(Nanos(capture.get() / 3));
            recorder.mark(frame, Stage::CaptureAcquired);

            if args.preprocess_ms > 0.0 {
                recorder.mark(frame, Stage::GpuPreprocessStart);
                burn(jitter.of(args.preprocess_ms));
                recorder.mark(frame, Stage::GpuPreprocessEnd);
            }

            recorder.mark(frame, Stage::EncodeSubmit);
            burn(jitter.of(args.encode_ms));
            recorder.mark(frame, Stage::EncodeComplete);

            recorder.mark(frame, Stage::PacketizationStart);
            burn(jitter.of(args.packetize_ms));
            recorder.mark(frame, Stage::NetworkSendFirst);
            burn(jitter.of(args.send_ms));
            recorder.mark(frame, Stage::NetworkSendLast);

            let arrival = Timestamp::now().add(jitter.of(args.network_ms));
            if sender.send(Wire { frame, arrival }).is_err() {
                return;
            }
        }
    }
}

fn client_loop(
    recorder: Recorder,
    args: SyntheticArgs,
    receiver: Receiver<Wire>,
) -> impl FnOnce() + Send + 'static {
    move || {
        let mut jitter = Jitter::new(args.seed ^ 0xA5A5_A5A5, args.jitter_ms);

        while let Ok(packet) = receiver.recv() {
            wait_until(packet.arrival);
            recorder.mark(packet.frame, Stage::NetworkReceiveFirst);
            burn(jitter.of(args.receive_ms));
            recorder.mark(packet.frame, Stage::NetworkReceiveLast);

            burn(jitter.of(args.reassembly_ms));
            recorder.mark(packet.frame, Stage::FrameReassembled);

            recorder.mark(packet.frame, Stage::DecodeSubmit);
            burn(jitter.of(args.decode_ms));
            recorder.mark(packet.frame, Stage::DecodeComplete);

            recorder.mark(packet.frame, Stage::RenderSubmit);
            burn(jitter.of(args.render_ms));
            recorder.mark(packet.frame, Stage::PresentSubmit);
        }
    }
}

/// Occupies the thread for `duration`, the way real work would.
fn burn(duration: Nanos) {
    wait_until(Timestamp::now().add(duration));
}

/// Sleeps while there is more than a millisecond to go, then spins. Plain
/// sleeping cannot resolve the 70 microsecond stages this harness models.
fn wait_until(target: Timestamp) {
    loop {
        let now = Timestamp::now();
        if now >= target {
            return;
        }
        let remaining = target.saturating_since(now).get();
        if remaining > 1_500_000 {
            thread::sleep(Duration::from_nanos(remaining - 1_000_000));
        } else {
            std::hint::spin_loop();
        }
    }
}

/// xorshift64*, so a seed reproduces a run exactly.
struct Jitter {
    state: u64,
    spread_ms: f64,
}

impl Jitter {
    fn new(seed: u64, spread_ms: f64) -> Self {
        Jitter {
            state: seed | 1,
            spread_ms: spread_ms.max(0.0),
        }
    }

    fn of(&mut self, base_ms: f64) -> Nanos {
        if base_ms <= 0.0 {
            return Nanos::ZERO;
        }
        Nanos::from_millis_f64(base_ms + self.unit() * self.spread_ms)
    }

    fn unit(&mut self) -> f64 {
        self.state ^= self.state >> 12;
        self.state ^= self.state << 25;
        self.state ^= self.state >> 27;
        let value = self.state.wrapping_mul(0x2545_F491_4F6C_DD1D);
        (value >> 11) as f64 / (1u64 << 53) as f64
    }
}
