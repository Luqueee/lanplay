/// A point in a frame's life that is worth timestamping.
///
/// The list is deliberately fixed: every stage is one slot in a per-frame
/// array, so a timeline costs a small constant and nothing allocates while a
/// frame is in flight. Stages that a given pipeline never reaches (a host with
/// no GPU preprocess step, a client-only run) simply stay unset.
///
/// Host and client each mark on their own machine's clock; the pairs that
/// cross the wire (`NetworkSendLast` -> `NetworkReceiveFirst`) are only
/// meaningful once clock offset estimation lands.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Stage {
    /// Frame exists as content: the game presented, or the synthetic source ticked.
    FrameCreated = 0,
    /// Capture backend signalled a new frame is ready.
    CaptureAvailable,
    /// We own the GPU texture.
    CaptureAcquired,
    GpuPreprocessStart,
    GpuPreprocessEnd,
    /// Frame handed to the encoder.
    EncodeSubmit,
    /// Encoder produced the bitstream.
    EncodeComplete,
    PacketizationStart,
    /// First packet of the frame handed to the socket.
    NetworkSendFirst,
    /// Last packet of the frame handed to the socket.
    NetworkSendLast,
    /// First packet of the frame arrived (client clock).
    NetworkReceiveFirst,
    NetworkReceiveLast,
    /// Complete access unit assembled.
    FrameReassembled,
    DecodeSubmit,
    DecodeComplete,
    RenderSubmit,
    /// Handed to the compositor. End of the measurable software pipeline.
    PresentSubmit,
}

/// Number of variants in [`Stage`]; the width of a timeline.
pub const STAGE_COUNT: usize = 17;

impl Stage {
    pub const ALL: [Stage; STAGE_COUNT] = [
        Stage::FrameCreated,
        Stage::CaptureAvailable,
        Stage::CaptureAcquired,
        Stage::GpuPreprocessStart,
        Stage::GpuPreprocessEnd,
        Stage::EncodeSubmit,
        Stage::EncodeComplete,
        Stage::PacketizationStart,
        Stage::NetworkSendFirst,
        Stage::NetworkSendLast,
        Stage::NetworkReceiveFirst,
        Stage::NetworkReceiveLast,
        Stage::FrameReassembled,
        Stage::DecodeSubmit,
        Stage::DecodeComplete,
        Stage::RenderSubmit,
        Stage::PresentSubmit,
    ];

    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn name(self) -> &'static str {
        match self {
            Stage::FrameCreated => "frame_created",
            Stage::CaptureAvailable => "capture_available",
            Stage::CaptureAcquired => "capture_acquired",
            Stage::GpuPreprocessStart => "gpu_preprocess_start",
            Stage::GpuPreprocessEnd => "gpu_preprocess_end",
            Stage::EncodeSubmit => "encode_submit",
            Stage::EncodeComplete => "encode_complete",
            Stage::PacketizationStart => "packetization_start",
            Stage::NetworkSendFirst => "network_send_first",
            Stage::NetworkSendLast => "network_send_last",
            Stage::NetworkReceiveFirst => "network_receive_first",
            Stage::NetworkReceiveLast => "network_receive_last",
            Stage::FrameReassembled => "frame_reassembled",
            Stage::DecodeSubmit => "decode_submit",
            Stage::DecodeComplete => "decode_complete",
            Stage::RenderSubmit => "render_submit",
            Stage::PresentSubmit => "present_submit",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_is_dense_and_ordered() {
        for (i, stage) in Stage::ALL.iter().enumerate() {
            assert_eq!(stage.index(), i, "{stage:?} out of order");
        }
    }
}
