//! Byte-for-byte proof that what came out of the depacketiser is what went
//! into the packetiser.
//!
//! "The decoder did not complain" is not verification: H.264 decoders are
//! built to conceal damage, so a transport that silently truncated every
//! hundredth slice would still produce a picture. A SHA-256 per access unit
//! either matches or it does not.

use std::ffi::OsString;
use std::fmt;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use lanplay_protocol::FrameId;
use lanplay_video_core::{AccessUnitSource, FixtureError, FixtureSource};
use sha2::{Digest, Sha256};

pub type Sha256Digest = [u8; 32];

#[derive(Debug)]
pub enum DigestError {
    Io(io::Error),
    Fixture(FixtureError),
    /// The sidecar exists but is not one hex digest per line.
    Malformed {
        path: PathBuf,
        line: usize,
    },
    EmptyFixture(PathBuf),
}

impl fmt::Display for DigestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DigestError::Io(err) => write!(f, "{err}"),
            DigestError::Fixture(err) => write!(f, "{err}"),
            DigestError::Malformed { path, line } => {
                write!(
                    f,
                    "{} line {line} is not a SHA-256 hex digest",
                    path.display()
                )
            }
            DigestError::EmptyFixture(path) => {
                write!(f, "{} holds no access units", path.display())
            }
        }
    }
}

impl core::error::Error for DigestError {}

impl From<io::Error> for DigestError {
    fn from(err: io::Error) -> Self {
        DigestError::Io(err)
    }
}

impl From<FixtureError> for DigestError {
    fn from(err: FixtureError) -> Self {
        DigestError::Fixture(err)
    }
}

/// One digest per access unit of a fixture, in file order.
pub struct Digests {
    entries: Vec<Sha256Digest>,
    path: PathBuf,
    generated: bool,
}

impl Digests {
    /// Loads `<fixture>.sha256`, computing and writing it if it is absent or
    /// does not describe this fixture.
    pub fn ensure(fixture: &Path, fps: u32) -> Result<Digests, DigestError> {
        let path = sidecar(fixture);
        let expected = FixtureSource::load(fixture, fps)?;
        let count = expected.access_unit_count();
        if count == 0 {
            return Err(DigestError::EmptyFixture(fixture.to_path_buf()));
        }

        if let Some(entries) = read(&path)?
            && entries.len() == count
        {
            return Ok(Digests {
                entries,
                path,
                generated: false,
            });
        }

        let entries = compute(expected);
        write(&path, &entries)?;
        Ok(Digests {
            entries,
            path,
            generated: true,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether this run had to build the sidecar rather than reuse it.
    pub fn generated(&self) -> bool {
        self.generated
    }

    /// The digest a frame id should hash to.
    ///
    /// Frame ids climb forever while the fixture loops, so the id maps back
    /// onto a file position; ids start at one.
    pub fn for_frame(&self, frame: FrameId) -> Option<&Sha256Digest> {
        let ordinal = frame.get().checked_sub(1)?;
        self.entries
            .get((ordinal % self.entries.len() as u64) as usize)
    }

    pub fn matches(&self, frame: FrameId, data: &[u8]) -> Option<bool> {
        let expected = self.for_frame(frame)?;
        Some(Sha256::digest(data).as_slice() == expected.as_slice())
    }
}

fn sidecar(fixture: &Path) -> PathBuf {
    let mut name = OsString::from(fixture.as_os_str());
    name.push(".sha256");
    PathBuf::from(name)
}

fn compute(mut source: FixtureSource) -> Vec<Sha256Digest> {
    let mut entries = Vec::with_capacity(source.access_unit_count());
    while let Some(unit) = source.next_access_unit() {
        entries.push(Sha256::digest(&unit.data).into());
    }
    entries
}

fn read(path: &Path) -> Result<Option<Vec<Sha256Digest>>, DigestError> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err.into()),
    };

    let mut entries = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        entries.push(parse_hex(line).ok_or_else(|| DigestError::Malformed {
            path: path.to_path_buf(),
            line: index + 1,
        })?);
    }
    Ok(Some(entries))
}

fn parse_hex(line: &str) -> Option<Sha256Digest> {
    if line.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (byte, pair) in out.iter_mut().zip(line.as_bytes().chunks_exact(2)) {
        let text = core::str::from_utf8(pair).ok()?;
        *byte = u8::from_str_radix(text, 16).ok()?;
    }
    Some(out)
}

fn write(path: &Path, entries: &[Sha256Digest]) -> Result<(), DigestError> {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut text = Vec::with_capacity(entries.len() * (Sha256::output_size() * 2 + 1));
    for entry in entries {
        for byte in entry {
            text.push(DIGITS[usize::from(byte >> 4)]);
            text.push(DIGITS[usize::from(byte & 0x0F)]);
        }
        text.push(b'\n');
    }
    fs::write(path, text)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn written_digests_parse_back_identically() {
        let entries: Vec<Sha256Digest> = (0..4u8)
            .map(|seed| {
                core::array::from_fn(|index| seed.wrapping_mul(31).wrapping_add(index as u8))
            })
            .collect();
        let dir = std::env::temp_dir().join(format!("net-bench-digests-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join("digests.sha256");
        write(&path, &entries).expect("write");
        let round_tripped = read(&path).expect("read").expect("present");
        fs::remove_dir_all(&dir).expect("cleanup");
        assert_eq!(round_tripped, entries);
    }

    #[test]
    fn short_or_non_hex_lines_are_rejected() {
        assert_eq!(parse_hex(""), None);
        assert_eq!(parse_hex(&"0".repeat(63)), None);
        assert_eq!(parse_hex(&"z".repeat(64)), None);
    }

    #[test]
    fn frame_ids_wrap_onto_the_fixture_as_it_loops() {
        let digests = Digests {
            entries: vec![[1; 32], [2; 32], [3; 32]],
            path: PathBuf::from("unused"),
            generated: false,
        };
        assert_eq!(digests.for_frame(FrameId::new(1)), Some(&[1u8; 32]));
        assert_eq!(digests.for_frame(FrameId::new(3)), Some(&[3u8; 32]));
        assert_eq!(digests.for_frame(FrameId::new(4)), Some(&[1u8; 32]));
        assert_eq!(digests.for_frame(FrameId::new(3001)), Some(&[1u8; 32]));
        assert_eq!(digests.for_frame(FrameId::NONE), None);
    }
}
