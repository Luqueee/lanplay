//! Writing the captured bytes to a file for a human to listen to.
//!
//! An extensible header is written whatever the endpoint's format tag was, so
//! that the channel mask and the valid-bit count survive into the file. The
//! samples themselves go out exactly as WASAPI delivered them: this is the one
//! place where a wrong answer would be inaudible, since a file rewritten into
//! some other format would still play, and would still play whatever the
//! rewriting code believed rather than what the endpoint sent.
//!
//! Nothing is written while the stream is running. The bytes are collected in
//! memory and the file is written once the client has stopped, because a write
//! syscall inside the capture loop would be measured as capture jitter and this
//! phase exists to measure capture jitter.

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use crate::format::MixFormat;

/// Size of the fmt chunk this writes: eighteen bytes of `WAVEFORMATEX` plus
/// the twenty-two byte extensible tail.
const FMT_CHUNK: u32 = 40;

/// Everything before the samples.
const HEADER_BYTES: usize = 68;

/// The RIFF header for a given format and payload length.
pub fn header(format: &MixFormat, data_bytes: u32) -> [u8; HEADER_BYTES] {
    let mut out = [0u8; HEADER_BYTES];
    let mut at = 0usize;
    let mut put = |bytes: &[u8]| {
        out[at..at + bytes.len()].copy_from_slice(bytes);
        at += bytes.len();
    };

    put(b"RIFF");
    // Everything after this field: the WAVE tag, the fmt chunk with its
    // header, and the data chunk with its header.
    put(&(4 + 8 + FMT_CHUNK + 8 + data_bytes).to_le_bytes());
    put(b"WAVE");

    put(b"fmt ");
    put(&FMT_CHUNK.to_le_bytes());
    put(&crate::format::WAVE_FORMAT_EXTENSIBLE.to_le_bytes());
    put(&format.channels.to_le_bytes());
    put(&format.sample_rate.to_le_bytes());
    put(&(format.sample_rate * u32::from(format.block_align)).to_le_bytes());
    put(&format.block_align.to_le_bytes());
    put(&format.bits_per_sample.to_le_bytes());
    put(&22u16.to_le_bytes());
    put(&format.valid_bits.to_le_bytes());
    put(&format.channel_mask.to_le_bytes());
    put(&guid_bytes(format.subformat));

    put(b"data");
    put(&data_bytes.to_le_bytes());
    debug_assert_eq!(at, HEADER_BYTES);
    out
}

/// A GUID in the layout a RIFF file wants: the first three fields
/// little-endian, the last eight bytes in order.
fn guid_bytes(guid: u128) -> [u8; 16] {
    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&((guid >> 96) as u32).to_le_bytes());
    out[4..6].copy_from_slice(&((guid >> 80) as u16).to_le_bytes());
    out[6..8].copy_from_slice(&((guid >> 64) as u16).to_le_bytes());
    out[8..16].copy_from_slice(&(guid as u64).to_be_bytes());
    out
}

pub fn write(path: &Path, format: &MixFormat, pcm: &[u8]) -> io::Result<()> {
    let data_bytes = u32::try_from(pcm.len()).map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "a RIFF file cannot hold more than four gigabytes of samples",
        )
    })?;
    let mut file = BufWriter::new(File::create(path)?);
    file.write_all(&header(format, data_bytes))?;
    file.write_all(pcm)?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::{RawExtensible, RawWaveFormat, SUBTYPE_IEEE_FLOAT, WAVE_FORMAT_EXTENSIBLE};

    fn stereo_float() -> MixFormat {
        MixFormat::from_raw(&RawWaveFormat {
            format_tag: WAVE_FORMAT_EXTENSIBLE,
            channels: 2,
            samples_per_sec: 48_000,
            avg_bytes_per_sec: 48_000 * 8,
            block_align: 8,
            bits_per_sample: 32,
            extensible: Some(RawExtensible {
                valid_bits: 32,
                channel_mask: 3,
                subformat: SUBTYPE_IEEE_FLOAT,
            }),
        })
        .expect("a describable format")
    }

    #[test]
    fn the_header_describes_the_endpoint_format() {
        let bytes = header(&stereo_float(), 800);
        assert_eq!(&bytes[0..4], b"RIFF");
        assert_eq!(
            u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
            60 + 800
        );
        assert_eq!(&bytes[8..12], b"WAVE");
        assert_eq!(&bytes[12..16], b"fmt ");
        assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 40);
        assert_eq!(
            u16::from_le_bytes(bytes[20..22].try_into().unwrap()),
            0xFFFE
        );
        assert_eq!(u16::from_le_bytes(bytes[22..24].try_into().unwrap()), 2);
        assert_eq!(
            u32::from_le_bytes(bytes[24..28].try_into().unwrap()),
            48_000
        );
        assert_eq!(
            u32::from_le_bytes(bytes[28..32].try_into().unwrap()),
            384_000
        );
        assert_eq!(u16::from_le_bytes(bytes[32..34].try_into().unwrap()), 8);
        assert_eq!(u16::from_le_bytes(bytes[34..36].try_into().unwrap()), 32);
        assert_eq!(u16::from_le_bytes(bytes[36..38].try_into().unwrap()), 22);
        assert_eq!(u16::from_le_bytes(bytes[38..40].try_into().unwrap()), 32);
        assert_eq!(u32::from_le_bytes(bytes[40..44].try_into().unwrap()), 3);
        assert_eq!(&bytes[60..64], b"data");
        assert_eq!(u32::from_le_bytes(bytes[64..68].try_into().unwrap()), 800);
    }

    #[test]
    fn the_subformat_guid_is_laid_out_the_way_riff_wants_it() {
        let bytes = guid_bytes(SUBTYPE_IEEE_FLOAT);
        assert_eq!(
            bytes,
            [
                0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x10, 0x00, 0x80, 0x00, 0x00, 0xaa, 0x00, 0x38,
                0x9b, 0x71
            ]
        );
    }
}
