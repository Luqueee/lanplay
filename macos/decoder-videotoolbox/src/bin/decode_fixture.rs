//! Drives the VideoToolbox decoder over a raw H.264 file and reports what
//! actually happened.
//!
//! Exits non-zero when the run does not prove what it set out to prove: a
//! hardware decoder, and every submitted access unit accounted for.

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::mpsc::{Receiver, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use clap::Parser;
use lanplay_decoder_videotoolbox::{DecodedFrame, DecoderConfig, VideoToolboxDecoder};
use lanplay_protocol::FrameId;
use lanplay_telemetry::{Segment, Telemetry, TelemetryConfig};
use lanplay_video_core::{
    EncodedAccessUnit, PixelFormat, VideoDecoder, VideoTimestamp, parse_stream,
};

#[derive(Parser)]
#[command(about = "Decode a raw H.264 fixture through VideoToolbox and report timings")]
struct Args {
    /// Raw Annex-B H.264 file.
    #[arg(long)]
    path: PathBuf,
    /// Frame rate the stream was encoded at; sets the presentation timescale.
    #[arg(long, default_value_t = 120)]
    fps: u32,
    /// Stop after this many access units.
    #[arg(long)]
    limit: Option<usize>,
    /// Submit at 1/fps instead of as fast as the decoder will take them.
    #[arg(long)]
    pace: bool,
    /// Sleep this long inside the output callback, modelling a slow consumer.
    #[arg(long)]
    callback_delay_ms: Option<u64>,
    /// Picture size the stream is expected to carry.
    #[arg(long, default_value_t = 1920)]
    width: u32,
    #[arg(long, default_value_t = 1080)]
    height: u32,
}

fn main() -> ExitCode {
    let args = Args::parse();
    match run(&args) {
        Ok(outcome) => outcome,
        Err(error) => {
            eprintln!("decode-fixture: {error}");
            ExitCode::FAILURE
        }
    }
}

fn run(args: &Args) -> Result<ExitCode, Box<dyn std::error::Error>> {
    let bytes = std::fs::read(&args.path)?;
    let (parameter_sets, raw_units) = parse_stream(&bytes)?;

    let units: Vec<EncodedAccessUnit> = raw_units
        .into_iter()
        .take(args.limit.unwrap_or(usize::MAX))
        .enumerate()
        .map(|(index, unit)| EncodedAccessUnit {
            id: FrameId::new(index as u64 + 1),
            pts: VideoTimestamp::from_frame_index(index as u64, args.fps, 1),
            is_idr: unit.is_idr,
            data: unit.data,
        })
        .collect();

    let idr_count = units.iter().filter(|unit| unit.is_idr).count();
    let largest = units.iter().map(EncodedAccessUnit::len).max().unwrap_or(0);
    println!(
        "fixture: {} ({} access units, {idr_count} IDR, largest {largest} B)",
        args.path.display(),
        units.len()
    );
    // Multi-slice encodes make parse_stream emit several "access units" per
    // picture, which turns every timing below into nonsense. A picture count
    // that is not a whole number of seconds at the declared rate is the cheap
    // tell.
    if args.limit.is_none() && units.len() % args.fps as usize != 0 {
        println!(
            "warning: {} access units is not a whole number of seconds at {} fps; \
             the encode may be multi-slice",
            units.len(),
            args.fps
        );
    }

    let telemetry = Telemetry::start(TelemetryConfig::default());
    // The sink runs on a VideoToolbox decode thread. Handing the buffer
    // straight to a bounded channel keeps the callback short, which is what a
    // real client does, and lets the pixel buffers be released elsewhere.
    let (sender, receiver) = sync_channel::<DecodedFrame>(256);
    let drain = thread::spawn(move || drain_frames(receiver));

    let mut decoder = VideoToolboxDecoder::new(
        DecoderConfig {
            parameter_sets,
            width: args.width,
            height: args.height,
            pixel_format: PixelFormat::Nv12VideoRange,
            require_hardware: true,
            realtime: true,
            callback_delay: args.callback_delay_ms.map(Duration::from_millis),
        },
        telemetry.recorder(),
        Box::new(move |frame| {
            // A full channel means the consumer is behind; blocking here is
            // the honest response, and it shows up as in_flight backlog.
            let _ = sender.send(frame);
        }),
    )?;

    let frame_interval = Duration::from_secs_f64(1.0 / f64::from(args.fps.max(1)));
    let started = Instant::now();
    let mut max_in_flight = 0usize;
    for (index, unit) in units.iter().enumerate() {
        if args.pace {
            let due = started + frame_interval * index as u32;
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                thread::sleep(wait);
            }
        }
        decoder.submit(unit)?;
        max_in_flight = max_in_flight.max(decoder.in_flight());
    }
    decoder.flush()?;
    let wall = started.elapsed();

    let hardware = decoder.uses_hardware_decoder();
    let submitted = decoder.submitted();
    let decoded = decoder.decoded();
    let dropped = decoder.dropped();
    let errors = decoder.errors();
    let description = decoder.pixel_buffer_description();
    // Drops the sink, which closes the channel and lets the drain thread end.
    drop(decoder);

    let released = drain.join().map_err(|_| "frame drain thread panicked")?;

    telemetry.flush(Duration::from_secs(2));
    let snapshot = telemetry.shutdown();

    println!();
    println!("hardware decoder: {}", if hardware { "yes" } else { "no" });
    println!("submitted:        {submitted}");
    println!("decoded:          {decoded}");
    println!("dropped:          {dropped}");
    println!("errors:           {errors}");
    println!("max in-flight:    {max_in_flight}");
    println!("buffers released: {released}");
    println!(
        "wall clock:       {:.3} s ({:.1} fps)",
        wall.as_secs_f64(),
        decoded as f64 / wall.as_secs_f64()
    );
    match &description {
        Some(description) => println!("pixel buffer:     {description}"),
        None => println!("pixel buffer:     none captured"),
    }
    let decode = snapshot.segment(Segment::Decode);
    println!(
        "decode:           p50 {} p95 {} p99 {} max {} over {} frames",
        decode.p50, decode.p95, decode.p99, decode.max, decode.count
    );
    println!();
    println!("{snapshot}");

    let accounted = decoded == submitted;
    if !hardware {
        eprintln!("FAIL: session is not using a hardware decoder");
    }
    if !accounted {
        eprintln!("FAIL: {submitted} submitted but {decoded} decoded");
    }
    Ok(if hardware && accounted {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

/// Releases decoded frames off the VideoToolbox callback thread, the way a
/// renderer would after it has a texture, and counts them so a buffer that
/// never came back is visible.
fn drain_frames(receiver: Receiver<DecodedFrame>) -> u64 {
    let mut released = 0;
    while receiver.recv().is_ok() {
        released += 1;
    }
    released
}
