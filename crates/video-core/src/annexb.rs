//! Just enough H.264 bitstream handling to feed a decoder.
//!
//! This is **not** a general H.264 parser and must not grow into one. It does
//! three things: split Annex-B into NAL units, group those into access units,
//! and hand back the parameter sets a `CMVideoFormatDescription` needs. No SPS
//! is parsed beyond copying it, no slice header is decoded.
//!
//! Everything that leaves here is AVCC (length-prefixed), because that is what
//! VideoToolbox accepts in a sample buffer.

use core::fmt;

use crate::access_unit::ParameterSets;

/// The NAL unit types this project cares about. Everything else is `Other`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum NalUnitType {
    /// Coded slice of a non-IDR picture.
    Slice,
    /// Coded slice of an IDR picture.
    IdrSlice,
    Sei,
    Sps,
    Pps,
    /// Access unit delimiter.
    Aud,
    Other(u8),
}

impl NalUnitType {
    /// Reads the type out of a NAL unit header byte.
    pub const fn from_header(header: u8) -> Self {
        match header & 0x1F {
            1 => NalUnitType::Slice,
            5 => NalUnitType::IdrSlice,
            6 => NalUnitType::Sei,
            7 => NalUnitType::Sps,
            8 => NalUnitType::Pps,
            9 => NalUnitType::Aud,
            other => NalUnitType::Other(other),
        }
    }

    /// Video Coding Layer: a NAL that actually carries picture data. The
    /// boundary between access units is defined in terms of these.
    pub const fn is_vcl(self) -> bool {
        matches!(self, NalUnitType::Slice | NalUnitType::IdrSlice)
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum AnnexBError {
    /// No start code at all: the input is not an Annex-B stream.
    NotAnnexB,
    /// The stream carries no SPS or no PPS, so no format description can be
    /// built and no decoder can be created.
    MissingParameterSets { sps: usize, pps: usize },
    /// The stream contains no coded pictures.
    NoAccessUnits,
}

impl fmt::Display for AnnexBError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AnnexBError::NotAnnexB => f.write_str("no Annex-B start code found"),
            AnnexBError::MissingParameterSets { sps, pps } => {
                write!(
                    f,
                    "stream has {sps} SPS and {pps} PPS, needs at least one of each"
                )
            }
            AnnexBError::NoAccessUnits => f.write_str("stream contains no coded pictures"),
        }
    }
}

impl core::error::Error for AnnexBError {}

/// Splits an Annex-B stream into NAL unit payloads, start codes removed.
///
/// Handles both three- and four-byte start codes, and trailing zero bytes
/// before the next start code (encoders pad).
pub fn split_annex_b(stream: &[u8]) -> impl Iterator<Item = &[u8]> {
    NalIter { stream, cursor: 0 }
}

struct NalIter<'a> {
    stream: &'a [u8],
    cursor: usize,
}

impl<'a> Iterator for NalIter<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        loop {
            let start_code = next_start_code(self.stream, self.cursor)?;
            let start = start_code + start_code_len(self.stream, start_code);
            let end = next_start_code(self.stream, start).unwrap_or(self.stream.len());
            self.cursor = end;
            let nal = trim_trailing_zeros(&self.stream[start..end]);
            if !nal.is_empty() {
                return Some(nal);
            }
        }
    }
}

/// Position of the next start code at or after `from`, or `None`.
fn next_start_code(stream: &[u8], from: usize) -> Option<usize> {
    let mut index = from;
    while index + 3 <= stream.len() {
        if stream[index] == 0 && stream[index + 1] == 0 {
            if stream[index + 2] == 1 {
                return Some(index);
            }
            if index + 4 <= stream.len() && stream[index + 2] == 0 && stream[index + 3] == 1 {
                return Some(index);
            }
        }
        index += 1;
    }
    None
}

fn start_code_len(stream: &[u8], at: usize) -> usize {
    if stream.get(at + 2) == Some(&1) { 3 } else { 4 }
}

/// Drops `trailing_zero_8bits` padding an encoder may insert before the next
/// start code.
///
/// Safe because `rbsp_trailing_bits` ends every real NAL with a stop bit, so
/// the last byte of a payload is never zero.
fn trim_trailing_zeros(nal: &[u8]) -> &[u8] {
    let mut end = nal.len();
    while end > 0 && nal[end - 1] == 0 {
        end -= 1;
    }
    &nal[..end]
}

/// One access unit: the NAL units that make up exactly one coded picture.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct RawAccessUnit {
    /// AVCC bytes, four-byte big-endian lengths.
    pub data: Vec<u8>,
    pub is_idr: bool,
}

/// Splits a whole Annex-B stream into parameter sets plus access units.
///
/// Access unit boundaries are detected the way a fixture needs and no
/// further: a new unit starts at an access unit delimiter, at a parameter set
/// or SEI that follows picture data, or at a second VCL NAL. That is exact for
/// single-slice-per-picture streams, which is what this project encodes and
/// what NVENC will send.
///
/// Parameter sets are collected but deliberately left out of the access units:
/// VideoToolbox takes them through the format description, not the sample.
pub fn parse_stream(stream: &[u8]) -> Result<(ParameterSets, Vec<RawAccessUnit>), AnnexBError> {
    if next_start_code(stream, 0).is_none() {
        return Err(AnnexBError::NotAnnexB);
    }

    let mut sets = ParameterSets {
        sps: Vec::new(),
        pps: Vec::new(),
        nal_length_size: 4,
    };
    let mut units: Vec<RawAccessUnit> = Vec::new();
    let mut current: Vec<&[u8]> = Vec::new();
    let mut current_has_vcl = false;
    let mut current_is_idr = false;

    let mut flush = |nals: &mut Vec<&[u8]>, has_vcl: &mut bool, is_idr: &mut bool| {
        if *has_vcl {
            units.push(RawAccessUnit {
                data: to_avcc(nals.iter().copied(), sets.nal_length_size),
                is_idr: *is_idr,
            });
        }
        nals.clear();
        *has_vcl = false;
        *is_idr = false;
    };

    for nal in split_annex_b(stream) {
        let kind = NalUnitType::from_header(nal[0]);
        let starts_new_unit = match kind {
            NalUnitType::Aud => true,
            _ if kind.is_vcl() => current_has_vcl,
            NalUnitType::Sps | NalUnitType::Pps | NalUnitType::Sei => current_has_vcl,
            _ => false,
        };
        if starts_new_unit {
            flush(&mut current, &mut current_has_vcl, &mut current_is_idr);
        }

        match kind {
            NalUnitType::Sps => sets.sps.push(nal.to_vec()),
            NalUnitType::Pps => sets.pps.push(nal.to_vec()),
            // Delimiters carry no information the decoder needs once the
            // access unit is framed.
            NalUnitType::Aud => {}
            _ => {
                if kind.is_vcl() {
                    current_has_vcl = true;
                    current_is_idr |= kind == NalUnitType::IdrSlice;
                }
                current.push(nal);
            }
        }
    }
    flush(&mut current, &mut current_has_vcl, &mut current_is_idr);

    if sets.sps.is_empty() || sets.pps.is_empty() {
        return Err(AnnexBError::MissingParameterSets {
            sps: sets.sps.len(),
            pps: sets.pps.len(),
        });
    }
    if units.is_empty() {
        return Err(AnnexBError::NoAccessUnits);
    }
    Ok((sets, units))
}

/// Concatenates NAL units with big-endian length prefixes.
pub fn to_avcc<'a>(nals: impl Iterator<Item = &'a [u8]>, length_size: u8) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in nals {
        let length = nal.len() as u32;
        match length_size {
            1 => out.push(length as u8),
            2 => out.extend_from_slice(&(length as u16).to_be_bytes()),
            4 => out.extend_from_slice(&length.to_be_bytes()),
            other => panic!("unsupported NAL length size {other}"),
        }
        out.extend_from_slice(nal);
    }
    out
}

/// Iterates the NAL units inside AVCC data, length prefixes removed.
///
/// Stops at the first truncated prefix rather than guessing: a malformed
/// sample is a bug upstream, and inventing a NAL boundary would hide it.
pub fn avcc_nal_units(data: &[u8], length_size: u8) -> AvccNalUnits<'_> {
    AvccNalUnits {
        data,
        cursor: 0,
        length_size: usize::from(length_size),
    }
}

pub struct AvccNalUnits<'a> {
    data: &'a [u8],
    cursor: usize,
    length_size: usize,
}

impl<'a> Iterator for AvccNalUnits<'a> {
    type Item = &'a [u8];

    fn next(&mut self) -> Option<&'a [u8]> {
        let header_end = self.cursor.checked_add(self.length_size)?;
        if header_end > self.data.len() {
            return None;
        }
        let mut length = 0usize;
        for byte in &self.data[self.cursor..header_end] {
            length = (length << 8) | usize::from(*byte);
        }
        let end = header_end.checked_add(length)?;
        if length == 0 || end > self.data.len() {
            return None;
        }
        self.cursor = end;
        Some(&self.data[header_end..end])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds an Annex-B stream from (start code length, payload) pairs.
    fn stream(nals: &[(usize, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (start_code, payload) in nals {
            out.extend_from_slice(if *start_code == 3 {
                &[0, 0, 1][..]
            } else {
                &[0, 0, 0, 1][..]
            });
            out.extend_from_slice(payload);
        }
        out
    }

    const SPS: &[u8] = &[0x67, 0x42, 0x00, 0x1E];
    const PPS: &[u8] = &[0x68, 0xCE, 0x38, 0x80];
    // Real NAL payloads never end in 0x00: rbsp_trailing_bits puts a stop bit
    // in the last byte. Test data has to respect that or it is testing a
    // stream no encoder can emit.
    const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x21];
    const SLICE: &[u8] = &[0x41, 0x9A, 0x00, 0x11];
    const SEI: &[u8] = &[0x06, 0x05, 0x01, 0x80];
    const AUD: &[u8] = &[0x09, 0x10];

    #[test]
    fn zero_padding_after_a_nal_is_dropped() {
        let mut bytes = stream(&[(4, IDR)]);
        bytes.extend_from_slice(&[0, 0, 0]);
        let nals: Vec<&[u8]> = split_annex_b(&bytes).collect();
        assert_eq!(nals, vec![IDR]);
    }

    #[test]
    fn both_start_code_lengths_split_the_same_way() {
        let bytes = stream(&[(4, SPS), (3, PPS), (4, IDR)]);
        let nals: Vec<&[u8]> = split_annex_b(&bytes).collect();
        assert_eq!(nals, vec![SPS, PPS, IDR]);
    }

    #[test]
    fn trailing_zero_padding_is_not_part_of_a_nal() {
        let mut bytes = stream(&[(4, SPS)]);
        bytes.extend_from_slice(&[0, 0]);
        bytes.extend_from_slice(&[0, 0, 0, 1]);
        bytes.extend_from_slice(PPS);
        let nals: Vec<&[u8]> = split_annex_b(&bytes).collect();
        assert_eq!(nals, vec![SPS, PPS]);
    }

    #[test]
    fn nal_types_come_from_the_header_byte() {
        assert_eq!(NalUnitType::from_header(SPS[0]), NalUnitType::Sps);
        assert_eq!(NalUnitType::from_header(PPS[0]), NalUnitType::Pps);
        assert_eq!(NalUnitType::from_header(IDR[0]), NalUnitType::IdrSlice);
        assert_eq!(NalUnitType::from_header(SLICE[0]), NalUnitType::Slice);
        assert!(NalUnitType::from_header(IDR[0]).is_vcl());
        assert!(!NalUnitType::from_header(SEI[0]).is_vcl());
    }

    #[test]
    fn each_coded_picture_becomes_one_access_unit() {
        let bytes = stream(&[
            (4, SPS),
            (4, PPS),
            (4, SEI),
            (4, IDR),
            (4, SLICE),
            (4, SLICE),
        ]);
        let (sets, units) = parse_stream(&bytes).expect("parses");

        assert_eq!(sets.sps, vec![SPS.to_vec()]);
        assert_eq!(sets.pps, vec![PPS.to_vec()]);
        assert_eq!(units.len(), 3);
        assert!(units[0].is_idr);
        assert!(!units[1].is_idr);
    }

    #[test]
    fn access_unit_delimiters_frame_pictures_too() {
        let bytes = stream(&[(4, SPS), (4, PPS), (4, AUD), (4, IDR), (4, AUD), (4, SLICE)]);
        let (_, units) = parse_stream(&bytes).expect("parses");
        assert_eq!(units.len(), 2);
        // The delimiter itself is dropped: only the slice is in the sample.
        assert_eq!(units[0].data.len(), 4 + IDR.len());
    }

    #[test]
    fn samples_are_length_prefixed_not_start_code_prefixed() {
        let bytes = stream(&[(4, SPS), (4, PPS), (4, IDR)]);
        let (_, units) = parse_stream(&bytes).expect("parses");
        assert_eq!(&units[0].data[..4], &(IDR.len() as u32).to_be_bytes());
        assert_eq!(&units[0].data[4..], IDR);
    }

    #[test]
    fn parameter_sets_stay_out_of_the_samples() {
        let bytes = stream(&[(4, SPS), (4, PPS), (4, IDR)]);
        let (_, units) = parse_stream(&bytes).expect("parses");
        assert_eq!(units[0].data.len(), 4 + IDR.len());
    }

    #[test]
    fn a_stream_without_parameter_sets_is_rejected() {
        let bytes = stream(&[(4, IDR), (4, SLICE)]);
        assert_eq!(
            parse_stream(&bytes),
            Err(AnnexBError::MissingParameterSets { sps: 0, pps: 0 })
        );
    }

    #[test]
    fn a_stream_without_start_codes_is_rejected() {
        assert_eq!(parse_stream(&[1, 2, 3, 4]), Err(AnnexBError::NotAnnexB));
    }

    #[test]
    fn parameter_sets_alone_are_not_a_stream() {
        let bytes = stream(&[(4, SPS), (4, PPS)]);
        assert_eq!(parse_stream(&bytes), Err(AnnexBError::NoAccessUnits));
    }
}
