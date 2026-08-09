//! Encoded H.264 fixtures and the source that replays them.
//!
//! Phase 2 has to measure decode and present latency before any network
//! exists, which means the bytes have to come from somewhere trustworthy. A
//! fixture is generated once by ffmpeg, cached on disk, and then replayed
//! entirely from memory.
//!
//! Two properties are load bearing and are enforced rather than assumed:
//! no B-frames (a reordered stream makes every decode timing meaningless,
//! because output order stops matching submission order), and one IDR per
//! second (so looping the fixture never hands the decoder a gap).

use core::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use lanplay_protocol::FrameId;

use crate::access_unit::{AccessUnitSource, EncodedAccessUnit, ParameterSets, VideoTimestamp};
use crate::annexb::{AnnexBError, RawAccessUnit, parse_stream};

/// What the fixture shows.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum FixturePattern {
    /// `testsrc2` draws a frame counter, which a camera pointed at the screen
    /// can read. That is how input-to-photon gets measured later without
    /// trusting either machine's clock.
    #[default]
    Motion,
    /// `mandelbrot` never stops changing and never compresses well, so the
    /// encoder produces the large, irregular access units a game actually
    /// generates. `testsrc2` is far too kind to the bitrate.
    Detail,
}

impl FixturePattern {
    pub const fn lavfi(self) -> &'static str {
        match self {
            FixturePattern::Motion => "testsrc2",
            FixturePattern::Detail => "mandelbrot",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            FixturePattern::Motion => "motion",
            FixturePattern::Detail => "detail",
        }
    }
}

impl fmt::Display for FixturePattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Everything that changes the encoded bytes, and therefore everything that
/// belongs in the cache key.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub struct FixtureSpec {
    pub pattern: FixturePattern,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub seconds: u32,
    pub bitrate_mbps: u32,
    pub gop: u32,
}

impl Default for FixtureSpec {
    fn default() -> Self {
        FixtureSpec {
            pattern: FixturePattern::Motion,
            width: 1920,
            height: 1080,
            fps: 120,
            seconds: 10,
            bitrate_mbps: 50,
            // One IDR per second: short enough that a loop point is never far
            // away, long enough that keyframes do not dominate the bitrate.
            gop: 120,
        }
    }
}

impl FixtureSpec {
    /// The cache key, rendered as a file name. Every field that affects the
    /// bytes appears here, so two specs can never collide on disk.
    pub fn file_name(&self) -> String {
        format!(
            "{}-{}x{}@{}-{}s-{}M.h264",
            self.pattern.label(),
            self.width,
            self.height,
            self.fps,
            self.seconds,
            self.bitrate_mbps
        )
    }

    /// Frames the encoder is asked to produce.
    pub const fn expected_frames(&self) -> u64 {
        self.fps as u64 * self.seconds as u64
    }

    fn lavfi_input(&self) -> String {
        format!(
            "{}=size={}x{}:rate={}",
            self.pattern.lavfi(),
            self.width,
            self.height,
            self.fps
        )
    }
}

#[derive(Debug)]
pub enum FixtureError {
    FfmpegMissing,
    ToolFailed {
        tool: &'static str,
        status: String,
        stderr: String,
    },
    Io(io::Error),
    Parse(AnnexBError),
    ReorderDetected {
        b_frames: usize,
    },
}

impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FixtureError::FfmpegMissing => f.write_str(
                "ffmpeg and ffprobe are required to build fixtures (brew install ffmpeg)",
            ),
            FixtureError::ToolFailed {
                tool,
                status,
                stderr,
            } => {
                write!(f, "{tool} failed ({status})")?;
                if !stderr.is_empty() {
                    write!(f, ": {}", stderr.trim())?;
                }
                Ok(())
            }
            FixtureError::Io(err) => write!(f, "fixture I/O failed: {err}"),
            FixtureError::Parse(err) => write!(f, "fixture is not a usable stream: {err}"),
            FixtureError::ReorderDetected { b_frames } => write!(
                f,
                "encoder emitted {b_frames} B-frames; a reordered stream invalidates every \
                 decode latency measurement"
            ),
        }
    }
}

impl core::error::Error for FixtureError {
    fn source(&self) -> Option<&(dyn core::error::Error + 'static)> {
        match self {
            FixtureError::Io(err) => Some(err),
            FixtureError::Parse(err) => Some(err),
            _ => None,
        }
    }
}

impl From<io::Error> for FixtureError {
    fn from(err: io::Error) -> Self {
        FixtureError::Io(err)
    }
}

impl From<AnnexBError> for FixtureError {
    fn from(err: AnnexBError) -> Self {
        FixtureError::Parse(err)
    }
}

/// Picture types as ffprobe counted them.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct FixtureReport {
    pub frames: usize,
    pub i_frames: usize,
    pub p_frames: usize,
    pub b_frames: usize,
}

/// Returns the cached fixture, encoding it first if it is not there.
///
/// The encode lands on a temporary file and is renamed into place only after
/// it has been verified, so an interrupted or rejected run can never leave
/// something behind that the next run mistakes for a valid cache entry.
pub fn ensure_fixture(spec: &FixtureSpec, dir: &Path) -> Result<PathBuf, FixtureError> {
    let path = dir.join(spec.file_name());
    if fs::metadata(&path).is_ok_and(|meta| meta.is_file() && meta.len() > 0) {
        return Ok(path);
    }

    fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{}.partial", spec.file_name()));
    // A previous crash may have left one; ffmpeg would prompt otherwise.
    let _ = fs::remove_file(&temp);

    let gop = spec.gop.to_string();
    let bitrate = format!("{}M", spec.bitrate_mbps);
    let seconds = spec.seconds.to_string();
    let status = run_tool(
        "ffmpeg",
        &[
            "-hide_banner".as_ref(),
            "-nostdin".as_ref(),
            "-loglevel".as_ref(),
            "error".as_ref(),
            "-y".as_ref(),
            "-f".as_ref(),
            "lavfi".as_ref(),
            "-i".as_ref(),
            spec.lavfi_input().as_ref(),
            "-t".as_ref(),
            seconds.as_ref(),
            "-c:v".as_ref(),
            "libx264".as_ref(),
            "-bf".as_ref(),
            "0".as_ref(),
            "-g".as_ref(),
            gop.as_ref(),
            "-keyint_min".as_ref(),
            gop.as_ref(),
            "-pix_fmt".as_ref(),
            "yuv420p".as_ref(),
            "-b:v".as_ref(),
            bitrate.as_ref(),
            "-preset".as_ref(),
            "veryfast".as_ref(),
            "-tune".as_ref(),
            "zerolatency".as_ref(),
            // `zerolatency` turns on sliced threads, which cuts every picture
            // into one slice per core. NVENC will send one slice per picture
            // and the access unit splitter is built for exactly that, so a
            // multi-slice fixture would measure a stream we never receive.
            "-x264-params".as_ref(),
            "sliced-threads=0".as_ref(),
            "-f".as_ref(),
            "h264".as_ref(),
            temp.as_os_str(),
        ],
    );

    if let Err(err) = status {
        let _ = fs::remove_file(&temp);
        return Err(err);
    }

    match verify_no_frame_reordering(&temp) {
        Ok(report) if report.b_frames == 0 => {}
        Ok(report) => {
            let _ = fs::remove_file(&temp);
            return Err(FixtureError::ReorderDetected {
                b_frames: report.b_frames,
            });
        }
        Err(err) => {
            let _ = fs::remove_file(&temp);
            return Err(err);
        }
    }

    fs::rename(&temp, &path)?;
    Ok(path)
}

/// Counts picture types in an encoded stream.
///
/// A non-zero `b_frames` is reported, not rejected: the caller decides whether
/// reordering matters. [`ensure_fixture`] decides that it does.
pub fn verify_no_frame_reordering(path: &Path) -> Result<FixtureReport, FixtureError> {
    let stdout = run_tool(
        "ffprobe",
        &[
            "-v".as_ref(),
            "error".as_ref(),
            "-select_streams".as_ref(),
            "v:0".as_ref(),
            "-show_entries".as_ref(),
            "frame=pict_type".as_ref(),
            "-of".as_ref(),
            "csv".as_ref(),
            path.as_os_str(),
        ],
    )?;

    Ok(count_pict_types(&stdout))
}

/// Tallies ffprobe's `-of csv` rows for `frame=pict_type`.
///
/// Rows are `frame,I`, section name first, but ffprobe appends a trailing
/// empty field to some frames, so the picture type is the last field that has
/// anything in it. Taking the last field outright silently loses those frames,
/// which is exactly the sort of quiet undercount that would let a reordered
/// stream through the gate.
fn count_pict_types(csv: &str) -> FixtureReport {
    let mut report = FixtureReport::default();
    for line in csv.lines() {
        let Some(kind) = line.rsplit(',').map(str::trim).find(|f| !f.is_empty()) else {
            continue;
        };
        match kind {
            "I" => report.i_frames += 1,
            "P" => report.p_frames += 1,
            "B" => report.b_frames += 1,
            _ => continue,
        }
        report.frames += 1;
    }
    report
}

fn run_tool(tool: &'static str, args: &[&std::ffi::OsStr]) -> Result<String, FixtureError> {
    let output = Command::new(tool)
        .args(args)
        .output()
        .map_err(|_| FixtureError::FfmpegMissing)?;

    if !output.status.success() {
        return Err(FixtureError::ToolFailed {
            tool,
            status: output.status.to_string(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        });
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Replays a parsed fixture as an [`AccessUnitSource`].
///
/// The whole file is held in memory, already split into access units: a
/// latency measurement must not accidentally time a page fault.
pub struct FixtureSource {
    parameter_sets: ParameterSets,
    units: Vec<RawAccessUnit>,
    cursor: usize,
    /// Frames handed out so far. Drives both the id and the PTS, and never
    /// goes backwards, so telemetry keyed on the id stays unambiguous even
    /// when the same bytes are replayed.
    sequence: u64,
    fps: u32,
    looping: bool,
}

impl FixtureSource {
    pub fn load(path: &Path, fps: u32) -> Result<Self, FixtureError> {
        let bytes = fs::read(path)?;
        let (parameter_sets, units) = parse_stream(&bytes)?;
        Ok(FixtureSource {
            parameter_sets,
            units,
            cursor: 0,
            sequence: 0,
            fps,
            looping: false,
        })
    }

    pub fn access_unit_count(&self) -> usize {
        self.units.len()
    }

    pub fn idr_count(&self) -> usize {
        self.units.iter().filter(|unit| unit.is_idr).count()
    }

    pub fn total_bytes(&self) -> usize {
        self.units.iter().map(|unit| unit.data.len()).sum()
    }

    pub fn largest_access_unit(&self) -> usize {
        self.units
            .iter()
            .map(|unit| unit.data.len())
            .max()
            .unwrap_or(0)
    }

    /// Rewinds to the first access unit, which is always an IDR.
    ///
    /// Frame ids and timestamps deliberately keep climbing: they identify a
    /// submission, not a position in the file, and reusing an id would make
    /// two different submissions indistinguishable in the telemetry.
    pub fn reset(&mut self) {
        self.cursor = 0;
    }

    /// When looping, end of stream wraps back to the first unit instead of
    /// stopping. The wrap point is an IDR, so the decoder never sees a gap.
    pub fn set_looping(&mut self, looping: bool) {
        self.looping = looping;
    }

    pub fn is_looping(&self) -> bool {
        self.looping
    }
}

impl AccessUnitSource for FixtureSource {
    fn parameter_sets(&self) -> &ParameterSets {
        &self.parameter_sets
    }

    fn next_access_unit(&mut self) -> Option<EncodedAccessUnit> {
        if self.cursor >= self.units.len() {
            if !self.looping || self.units.is_empty() {
                return None;
            }
            self.cursor = 0;
        }

        let unit = &self.units[self.cursor];
        self.cursor += 1;

        let sequence = self.sequence;
        self.sequence += 1;

        Some(EncodedAccessUnit {
            id: FrameId::new(sequence + 1),
            pts: VideoTimestamp::from_frame_index(sequence, self.fps, 1),
            is_idr: unit.is_idr,
            data: unit.data.clone(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SPS: &[u8] = &[0x67, 0x42, 0x00, 0x1E];
    const PPS: &[u8] = &[0x68, 0xCE, 0x38, 0x80];
    // Payloads must not end in 0x00: an encoder's rbsp_trailing_bits stop bit
    // guarantees they never do, and the splitter relies on it.
    const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x21];
    const SLICE: &[u8] = &[0x41, 0x9A, 0x00, 0x11];

    fn stream(nals: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for payload in nals {
            out.extend_from_slice(&[0, 0, 0, 1]);
            out.extend_from_slice(payload);
        }
        out
    }

    /// Three access units: IDR, then two P slices.
    fn source(fps: u32) -> FixtureSource {
        let bytes = stream(&[SPS, PPS, IDR, SLICE, SLICE]);
        let (parameter_sets, units) = parse_stream(&bytes).expect("parses");
        assert_eq!(units.len(), 3);
        FixtureSource {
            parameter_sets,
            units,
            cursor: 0,
            sequence: 0,
            fps,
            looping: false,
        }
    }

    #[test]
    fn file_name_carries_every_field_that_changes_the_bytes() {
        assert_eq!(
            FixtureSpec::default().file_name(),
            "motion-1920x1080@120-10s-50M.h264"
        );
        let detail = FixtureSpec {
            pattern: FixturePattern::Detail,
            width: 1280,
            height: 720,
            fps: 60,
            seconds: 5,
            bitrate_mbps: 8,
            gop: 60,
        };
        assert_eq!(detail.file_name(), "detail-1280x720@60-5s-8M.h264");
    }

    #[test]
    fn ids_are_unique_and_timestamps_advance_by_one_frame_period() {
        let mut src = source(120);
        let units: Vec<_> = std::iter::from_fn(|| src.next_access_unit()).collect();

        assert_eq!(units.len(), 3);
        let ids: Vec<u64> = units.iter().map(|unit| unit.id.get()).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        for (index, unit) in units.iter().enumerate() {
            assert_eq!(unit.pts, VideoTimestamp::new(index as i64, 120));
        }
        assert!(units[0].is_idr);
        assert!(!units[1].is_idr);
    }

    #[test]
    fn end_of_stream_stops_unless_looping() {
        let mut src = source(60);
        for _ in 0..3 {
            assert!(src.next_access_unit().is_some());
        }
        assert!(src.next_access_unit().is_none());
    }

    #[test]
    fn looping_wraps_to_the_idr_while_ids_keep_climbing() {
        let mut src = source(120);
        src.set_looping(true);

        let first: Vec<_> = (0..3)
            .map(|_| src.next_access_unit().expect("unit"))
            .collect();
        let wrapped = src.next_access_unit().expect("loops");

        assert!(wrapped.is_idr, "the wrap point must be a keyframe");
        assert_eq!(wrapped.data, first[0].data);
        assert_eq!(wrapped.id.get(), 4);
        assert_eq!(wrapped.pts, VideoTimestamp::new(3, 120));
    }

    #[test]
    fn reset_replays_from_the_first_unit_without_reusing_ids() {
        let mut src = source(120);
        let first = src.next_access_unit().expect("unit");
        let second = src.next_access_unit().expect("unit");
        assert_ne!(first.data, second.data);

        src.reset();
        let replayed = src.next_access_unit().expect("unit");
        assert_eq!(replayed.data, first.data);
        assert!(replayed.is_idr);
        assert_eq!(replayed.id.get(), 3);
    }

    #[test]
    fn size_accessors_describe_the_parsed_stream() {
        let src = source(120);
        assert_eq!(src.access_unit_count(), 3);
        assert_eq!(src.idr_count(), 1);
        // Every unit is one four-byte NAL behind a four-byte length prefix.
        assert_eq!(src.total_bytes(), 3 * 8);
        assert_eq!(src.largest_access_unit(), 8);
        assert_eq!(src.parameter_sets().nal_length_size, 4);
    }

    /// Verbatim ffprobe output. The `frame,I,` row is not a typo: ffprobe
    /// really does emit a trailing empty field on the frame carrying side
    /// data, and reading the last field blindly drops it.
    #[test]
    fn every_ffprobe_row_is_counted_including_the_ragged_one() {
        let csv = "frame,I,\nframe,P\nframe,P\nframe,I\n";
        let report = count_pict_types(csv);
        assert_eq!(report.frames, 4);
        assert_eq!(report.i_frames, 2);
        assert_eq!(report.p_frames, 2);
        assert_eq!(report.b_frames, 0);
    }

    #[test]
    fn reordered_streams_are_visible_in_the_report() {
        let report = count_pict_types("frame,I,\nframe,P\nframe,B\nframe,B\n");
        assert_eq!(report.b_frames, 2);
        assert_eq!(report.frames, 4);
    }
}
