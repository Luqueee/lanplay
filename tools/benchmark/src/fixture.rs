//! Builds (or reuses) an encoded fixture and reports what came out of it.
//!
//! Nothing here is timed. The point is to confirm, before the decoder exists,
//! that the fixture on disk really is what phase 2 assumes: the right number
//! of access units, one IDR per second, and no reordering.

use std::path::PathBuf;

use clap::{Args, ValueEnum};
use lanplay_video_core::{
    FixtureError, FixturePattern, FixtureSource, FixtureSpec, ensure_fixture,
    verify_no_frame_reordering,
};

#[derive(Args, Debug, Clone)]
pub struct FixtureArgs {
    /// Content type: a readable frame counter, or high-detail motion.
    #[arg(long, value_enum, default_value_t = Pattern::Motion)]
    pub pattern: Pattern,
    #[arg(long, default_value_t = 1920)]
    pub width: u32,
    #[arg(long, default_value_t = 1080)]
    pub height: u32,
    #[arg(long, default_value_t = 120)]
    pub fps: u32,
    #[arg(long, default_value_t = 10)]
    pub seconds: u32,
    #[arg(long, default_value_t = 50)]
    pub bitrate_mbps: u32,
    /// Keyframe interval. Defaults to one IDR per second.
    #[arg(long)]
    pub gop: Option<u32>,
    /// Where fixtures are cached.
    #[arg(long, default_value = "fixtures")]
    pub dir: PathBuf,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, ValueEnum)]
pub enum Pattern {
    Motion,
    Detail,
}

impl FixtureArgs {
    fn spec(&self) -> FixtureSpec {
        FixtureSpec {
            pattern: match self.pattern {
                Pattern::Motion => FixturePattern::Motion,
                Pattern::Detail => FixturePattern::Detail,
            },
            width: self.width,
            height: self.height,
            fps: self.fps,
            seconds: self.seconds,
            bitrate_mbps: self.bitrate_mbps,
            gop: self.gop.unwrap_or(self.fps),
        }
    }
}

pub fn run(args: &FixtureArgs) -> Result<(), FixtureError> {
    let spec = args.spec();
    let path = ensure_fixture(&spec, &args.dir)?;
    let bytes = std::fs::metadata(&path)?.len();
    let report = verify_no_frame_reordering(&path)?;
    let source = FixtureSource::load(&path, spec.fps)?;

    let units = source.access_unit_count();
    let mean = source.total_bytes().checked_div(units).unwrap_or(0);

    println!("{}", path.display());
    println!(
        "  {} {}x{} @{} fps, {} s, {} Mbps, gop {}",
        spec.pattern, spec.width, spec.height, spec.fps, spec.seconds, spec.bitrate_mbps, spec.gop
    );
    println!(
        "  file            {} bytes ({:.2} MiB, {:.1} Mbps actual)",
        bytes,
        bytes as f64 / (1024.0 * 1024.0),
        (bytes as f64 * 8.0) / (f64::from(spec.seconds) * 1e6),
    );
    println!(
        "  access units    {units} (expected {})",
        spec.expected_frames()
    );
    println!("  idr             {}", source.idr_count());
    println!("  mean unit       {mean} bytes");
    println!("  largest unit    {} bytes", source.largest_access_unit());
    println!(
        "  pict types      {} frames: {} I, {} P, {} B",
        report.frames, report.i_frames, report.p_frames, report.b_frames
    );
    Ok(())
}
