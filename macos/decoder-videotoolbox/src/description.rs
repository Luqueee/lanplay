use core::fmt::{self, Write as _};

use objc2_core_video::{
    CVPixelBuffer, CVPixelBufferGetBytesPerRowOfPlane, CVPixelBufferGetHeight,
    CVPixelBufferGetIOSurface, CVPixelBufferGetPixelFormatType, CVPixelBufferGetPlaneCount,
    CVPixelBufferGetWidth,
};

/// What the decoder actually handed back, sampled once.
///
/// This exists to prove the zero-copy path: if `iosurface_backed` is false or
/// the four-character code is not the requested bi-planar NV12, the renderer
/// would have to convert on the CPU and the whole pipeline is invalid. It is
/// captured from the first frame only, because these properties are fixed for
/// the life of a session and querying them per frame would put six CoreVideo
/// calls on the hot path for no information.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct PixelBufferDescription {
    pub four_cc: String,
    pub width: usize,
    pub height: usize,
    pub plane_count: usize,
    /// Row stride of each plane, in bytes. Longer than `width` whenever the
    /// decoder aligns rows, which it always does.
    pub bytes_per_row: Vec<usize>,
    pub iosurface_backed: bool,
    /// Read back from the pixel buffer pool VideoToolbox built for this
    /// session, not from the attributes we asked for.
    pub metal_compatible: bool,
}

impl PixelBufferDescription {
    pub(crate) fn capture(buffer: &CVPixelBuffer, metal_compatible: bool) -> Self {
        let plane_count = CVPixelBufferGetPlaneCount(buffer);
        PixelBufferDescription {
            four_cc: four_cc_string(CVPixelBufferGetPixelFormatType(buffer)),
            width: CVPixelBufferGetWidth(buffer),
            height: CVPixelBufferGetHeight(buffer),
            plane_count,
            bytes_per_row: (0..plane_count)
                .map(|plane| CVPixelBufferGetBytesPerRowOfPlane(buffer, plane))
                .collect(),
            iosurface_backed: CVPixelBufferGetIOSurface(Some(buffer)).is_some(),
            metal_compatible,
        }
    }
}

impl fmt::Display for PixelBufferDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} {}x{}, {} plane(s), stride",
            self.four_cc, self.width, self.height, self.plane_count
        )?;
        for (plane, stride) in self.bytes_per_row.iter().enumerate() {
            write!(f, "{}{stride}", if plane == 0 { " " } else { "/" })?;
        }
        write!(
            f,
            ", iosurface {}, metal {}",
            yes_no(self.iosurface_backed),
            yes_no(self.metal_compatible)
        )
    }
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

/// Renders a CoreVideo four-character code the way Apple's headers write it.
///
/// Codes are conventionally readable ASCII ('420v'), but nothing enforces
/// that, and a decoder that silently produced some other format would be
/// worth seeing rather than hiding behind a lossy conversion. Bytes outside
/// printable ASCII are escaped.
pub(crate) fn four_cc_string(code: u32) -> String {
    let mut text = String::with_capacity(4);
    for byte in code.to_be_bytes() {
        if byte.is_ascii_graphic() || byte == b' ' {
            text.push(byte as char);
        } else {
            let _ = write!(text, "\\x{byte:02x}");
        }
    }
    text
}

#[cfg(test)]
mod tests {
    use super::{PixelBufferDescription, four_cc_string};
    use lanplay_video_core::PixelFormat;

    #[test]
    fn nv12_codes_render_as_their_apple_spelling() {
        assert_eq!(
            four_cc_string(PixelFormat::Nv12VideoRange.four_cc()),
            "420v"
        );
        assert_eq!(four_cc_string(PixelFormat::Nv12FullRange.four_cc()), "420f");
    }

    #[test]
    fn byte_order_is_big_endian_not_native() {
        // '420v' is 0x34_32_30_76; a little-endian read would spell "v024".
        assert_eq!(four_cc_string(0x3432_3076), "420v");
    }

    #[test]
    fn non_printable_codes_are_escaped_rather_than_mangled() {
        assert_eq!(four_cc_string(0), "\\x00\\x00\\x00\\x00");
        assert_eq!(four_cc_string(0x0000_0020), "\\x00\\x00\\x00 ");
    }

    #[test]
    fn display_shows_every_plane_stride_and_both_compatibility_flags() {
        let description = PixelBufferDescription {
            four_cc: "420v".into(),
            width: 1920,
            height: 1080,
            plane_count: 2,
            bytes_per_row: vec![1920, 1920],
            iosurface_backed: true,
            metal_compatible: false,
        };
        assert_eq!(
            description.to_string(),
            "420v 1920x1080, 2 plane(s), stride 1920/1920, iosurface yes, metal no"
        );
    }
}
