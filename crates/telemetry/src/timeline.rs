use core::fmt;

use lanplay_protocol::FrameId;

use crate::clock::{ClockDomain, Nanos, Timestamp};
use crate::stage::{STAGE_COUNT, Stage};

/// What a segment of a frame's life is.
///
/// The distinction matters because the three are attacked differently: work
/// gets faster hardware or better parameters, waits get scheduling and
/// admission changes, and the wire gets network engineering.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SegmentKind {
    /// Something was actively being done to the frame.
    Work,
    /// The frame existed, finished, and sat there.
    Wait,
    /// The frame was in flight between machines.
    Wire,
}

impl SegmentKind {
    pub const fn label(self) -> &'static str {
        match self {
            SegmentKind::Work => "work",
            SegmentKind::Wait => "wait",
            SegmentKind::Wire => "wire",
        }
    }
}

/// A named piece of a frame's life.
///
/// Every one of these is measured between two real marks. Nothing here is a
/// residue: what cannot be attributed to a named segment is reported
/// separately as [`FrameTimeline::unattributed_gap`], and shrinking that
/// number means adding instrumentation, not arithmetic.
#[repr(u8)]
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub enum Segment {
    Capture = 0,
    PreprocessWait,
    GpuPreprocess,
    /// Frame ready, waiting for an encoder surface.
    AdmissionWait,
    Encode,
    /// Bitstream ready, waiting for the packetiser.
    PacketizeWait,
    Packetization,
    Send,
    Network,
    Receive,
    Reassembly,
    /// Access unit complete, waiting for the decoder.
    DecoderWait,
    Decode,
    /// Frame decoded, waiting for the renderer to pick it up.
    PresentationWait,
    Render,
}

pub const SEGMENT_COUNT: usize = 15;

impl Segment {
    pub const ALL: [Segment; SEGMENT_COUNT] = [
        Segment::Capture,
        Segment::PreprocessWait,
        Segment::GpuPreprocess,
        Segment::AdmissionWait,
        Segment::Encode,
        Segment::PacketizeWait,
        Segment::Packetization,
        Segment::Send,
        Segment::Network,
        Segment::Receive,
        Segment::Reassembly,
        Segment::DecoderWait,
        Segment::Decode,
        Segment::PresentationWait,
        Segment::Render,
    ];

    #[inline]
    pub const fn index(self) -> usize {
        self as usize
    }

    pub const fn label(self) -> &'static str {
        match self {
            Segment::Capture => "capture",
            Segment::PreprocessWait => "preprocess wait",
            Segment::GpuPreprocess => "gpu preprocess",
            Segment::AdmissionWait => "admission wait",
            Segment::Encode => "encode",
            Segment::PacketizeWait => "packetize wait",
            Segment::Packetization => "packetization",
            Segment::Send => "send",
            Segment::Network => "network",
            Segment::Receive => "receive",
            Segment::Reassembly => "reassembly",
            Segment::DecoderWait => "decoder wait",
            Segment::Decode => "decode",
            Segment::PresentationWait => "presentation wait",
            Segment::Render => "render",
        }
    }

    pub const fn kind(self) -> SegmentKind {
        match self {
            Segment::PreprocessWait
            | Segment::AdmissionWait
            | Segment::PacketizeWait
            | Segment::DecoderWait
            | Segment::PresentationWait => SegmentKind::Wait,
            Segment::Network => SegmentKind::Wire,
            _ => SegmentKind::Work,
        }
    }
}

/// Stages that tile a frame's life, in order.
///
/// `CaptureAvailable` is not an anchor: it is detail inside `capture`, and
/// putting it here would split that segment for no gain.
const CHAIN: [Stage; 16] = [
    Stage::FrameCreated,
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

/// Which segment a pair of consecutive marks represents.
///
/// `AdmissionWait` appears twice on purpose: a pipeline with no GPU
/// preprocess step still has an admission wait, and it would be wrong to let
/// that time fall into the unattributed bucket just because an optional stage
/// is absent.
const EDGES: [(Stage, Stage, Segment); 16] = [
    (
        Stage::FrameCreated,
        Stage::CaptureAcquired,
        Segment::Capture,
    ),
    (
        Stage::CaptureAcquired,
        Stage::GpuPreprocessStart,
        Segment::PreprocessWait,
    ),
    (
        Stage::CaptureAcquired,
        Stage::EncodeSubmit,
        Segment::AdmissionWait,
    ),
    (
        Stage::GpuPreprocessStart,
        Stage::GpuPreprocessEnd,
        Segment::GpuPreprocess,
    ),
    (
        Stage::GpuPreprocessEnd,
        Stage::EncodeSubmit,
        Segment::AdmissionWait,
    ),
    (Stage::EncodeSubmit, Stage::EncodeComplete, Segment::Encode),
    (
        Stage::EncodeComplete,
        Stage::PacketizationStart,
        Segment::PacketizeWait,
    ),
    (
        Stage::PacketizationStart,
        Stage::NetworkSendFirst,
        Segment::Packetization,
    ),
    (
        Stage::NetworkSendFirst,
        Stage::NetworkSendLast,
        Segment::Send,
    ),
    (
        Stage::NetworkSendLast,
        Stage::NetworkReceiveFirst,
        Segment::Network,
    ),
    (
        Stage::NetworkReceiveFirst,
        Stage::NetworkReceiveLast,
        Segment::Receive,
    ),
    (
        Stage::NetworkReceiveLast,
        Stage::FrameReassembled,
        Segment::Reassembly,
    ),
    (
        Stage::FrameReassembled,
        Stage::DecodeSubmit,
        Segment::DecoderWait,
    ),
    (Stage::DecodeSubmit, Stage::DecodeComplete, Segment::Decode),
    (
        Stage::DecodeComplete,
        Stage::RenderSubmit,
        Segment::PresentationWait,
    ),
    (Stage::RenderSubmit, Stage::PresentSubmit, Segment::Render),
];

const fn lookup(from: Stage, to: Stage) -> Option<Segment> {
    let mut index = 0;
    while index < EDGES.len() {
        let (edge_from, edge_to, segment) = EDGES[index];
        if edge_from as u8 == from as u8 && edge_to as u8 == to as u8 {
            return Some(segment);
        }
        index += 1;
    }
    None
}

/// How stale a frame is when it reaches the compositor. The number the
/// architectural gates are judged on.
pub const FRAME_AGE: (Stage, Stage) = (Stage::FrameCreated, Stage::PresentSubmit);

/// One stage timestamp, tagged with the clock it came from.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Mark {
    pub at: Timestamp,
    pub domain: ClockDomain,
}

/// One measured segment of one frame.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct SegmentSample {
    pub segment: Segment,
    pub duration: Nanos,
    /// True when the two marks came from different clocks, so the number is
    /// only as good as the offset estimate between them.
    pub cross_domain: bool,
}

/// Every timestamp collected for one frame.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrameTimeline {
    frame: FrameId,
    marks: [Option<Mark>; STAGE_COUNT],
}

impl FrameTimeline {
    pub(crate) fn new(frame: FrameId) -> Self {
        FrameTimeline {
            frame,
            marks: [None; STAGE_COUNT],
        }
    }

    /// Records a mark. Returns false if the stage was already set, which the
    /// collector counts as a duplicate rather than overwriting: the first
    /// mark is the one the code intended.
    pub(crate) fn set(&mut self, stage: Stage, mark: Mark) -> bool {
        let slot = &mut self.marks[stage.index()];
        if slot.is_some() {
            return false;
        }
        *slot = Some(mark);
        true
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.marks.iter().all(Option::is_none)
    }

    pub fn frame(&self) -> FrameId {
        self.frame
    }

    pub fn mark(&self, stage: Stage) -> Option<Mark> {
        self.marks[stage.index()]
    }

    pub fn at(&self, stage: Stage) -> Option<Timestamp> {
        self.mark(stage).map(|mark| mark.at)
    }

    /// Walks the frame's life, yielding each named segment between
    /// consecutive marks. Segments never overlap, so their durations sum to
    /// no more than the frame age.
    pub fn segments(&self) -> Segments<'_> {
        Segments {
            timeline: self,
            next_anchor: 0,
            previous: None,
        }
    }

    /// Duration of one named segment, if this frame has both its marks
    /// adjacent in the chain.
    pub fn segment(&self, segment: Segment) -> Option<Nanos> {
        self.segments()
            .find(|sample| sample.segment == segment)
            .map(|sample| sample.duration)
    }

    pub fn frame_age(&self) -> Option<Nanos> {
        let created = self.at(FRAME_AGE.0)?;
        self.at(FRAME_AGE.1)?.since(created)
    }

    /// Sum of every segment of `kind`.
    pub fn total(&self, kind: SegmentKind) -> Nanos {
        self.segments()
            .filter(|sample| sample.segment.kind() == kind)
            .fold(Nanos::ZERO, |acc, sample| acc + sample.duration)
    }

    /// Sum of every named segment, whatever its kind.
    pub fn attributed(&self) -> Nanos {
        self.segments()
            .fold(Nanos::ZERO, |acc, sample| acc + sample.duration)
    }

    /// Frame age that no named segment accounts for: missing instrumentation,
    /// scheduler delay between two unmarked points, or a stage this build
    /// never marks. It is a debt to be paid with more marks, not a metric to
    /// be interpreted.
    pub fn unattributed_gap(&self) -> Option<Nanos> {
        let age = self.frame_age()?;
        age.get().checked_sub(self.attributed().get()).map(Nanos)
    }

    /// A frame is complete when it was both born and presented.
    pub fn is_complete(&self) -> bool {
        self.at(Stage::FrameCreated).is_some() && self.at(Stage::PresentSubmit).is_some()
    }

    /// Raw stage marks, for dumping an unaggregated trace.
    pub fn stages(&self) -> impl Iterator<Item = (Stage, Mark)> + '_ {
        Stage::ALL
            .into_iter()
            .filter_map(|stage| self.mark(stage).map(|mark| (stage, mark)))
    }
}

/// Iterator over a frame's named segments.
pub struct Segments<'a> {
    timeline: &'a FrameTimeline,
    next_anchor: usize,
    previous: Option<(Stage, Mark)>,
}

impl Iterator for Segments<'_> {
    type Item = SegmentSample;

    fn next(&mut self) -> Option<SegmentSample> {
        while self.next_anchor < CHAIN.len() {
            let stage = CHAIN[self.next_anchor];
            self.next_anchor += 1;
            let Some(mark) = self.timeline.mark(stage) else {
                continue;
            };
            let Some((from_stage, from_mark)) = self.previous.replace((stage, mark)) else {
                continue;
            };
            // An unnamed pair or a mark out of order is not silently absorbed:
            // it stays outside the attributed total and shows up in the gap.
            let (Some(segment), Some(duration)) =
                (lookup(from_stage, stage), mark.at.since(from_mark.at))
            else {
                continue;
            };
            return Some(SegmentSample {
                segment,
                duration,
                cross_domain: from_mark.domain != mark.domain,
            });
        }
        None
    }
}

impl fmt::Display for FrameTimeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Frame {}", self.frame)?;
        writeln!(f)?;
        for sample in self.segments() {
            writeln!(
                f,
                "{:<18} {:>10}  {}{}",
                sample.segment.label(),
                sample.duration.to_string(),
                sample.segment.kind().label(),
                if sample.cross_domain { " *" } else { "" },
            )?;
        }
        writeln!(f)?;
        write_row(f, "measured work", Some(self.total(SegmentKind::Work)))?;
        write_row(f, "waits", Some(self.total(SegmentKind::Wait)))?;
        write_row(f, "wire", Some(self.total(SegmentKind::Wire)))?;
        write_row(f, "unattributed gap", self.unattributed_gap())?;
        write_row(f, "frame age", self.frame_age())
    }
}

fn write_row(f: &mut fmt::Formatter<'_>, name: &str, value: Option<Nanos>) -> fmt::Result {
    match value {
        Some(value) => writeln!(f, "{name:<18} {:>10}", value.to_string()),
        None => writeln!(f, "{name:<18} {:>10}", "-"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mark(ms: f64) -> Mark {
        Mark {
            at: Timestamp::from_nanos((ms * 1_000_000.0) as u64),
            domain: ClockDomain::local(),
        }
    }

    fn timeline(marks: &[(Stage, f64)]) -> FrameTimeline {
        let mut timeline = FrameTimeline::new(FrameId::new(7));
        for &(stage, ms) in marks {
            timeline.set(stage, mark(ms));
        }
        timeline
    }

    #[test]
    fn the_chain_tiles_the_frame_with_nothing_left_over() {
        let timeline = timeline(&[
            (Stage::FrameCreated, 0.0),
            (Stage::CaptureAcquired, 1.0),
            (Stage::EncodeSubmit, 1.5),
            (Stage::EncodeComplete, 3.5),
            (Stage::PacketizationStart, 3.6),
            (Stage::NetworkSendFirst, 3.7),
            (Stage::NetworkSendLast, 3.8),
            (Stage::NetworkReceiveFirst, 4.2),
            (Stage::NetworkReceiveLast, 4.3),
            (Stage::FrameReassembled, 4.4),
            (Stage::DecodeSubmit, 4.5),
            (Stage::DecodeComplete, 6.1),
            (Stage::RenderSubmit, 6.2),
            (Stage::PresentSubmit, 6.5),
        ]);

        assert_eq!(timeline.frame_age(), Some(Nanos::from_millis_f64(6.5)));
        assert_eq!(timeline.attributed(), Nanos::from_millis_f64(6.5));
        assert_eq!(timeline.unattributed_gap(), Some(Nanos::ZERO));
    }

    #[test]
    fn admission_wait_is_named_with_and_without_a_preprocess_step() {
        let without = timeline(&[
            (Stage::FrameCreated, 0.0),
            (Stage::CaptureAcquired, 1.0),
            (Stage::EncodeSubmit, 1.5),
            (Stage::EncodeComplete, 3.5),
        ]);
        assert_eq!(
            without.segment(Segment::AdmissionWait),
            Some(Nanos::from_millis_f64(0.5))
        );

        let with = timeline(&[
            (Stage::FrameCreated, 0.0),
            (Stage::CaptureAcquired, 1.0),
            (Stage::GpuPreprocessStart, 1.2),
            (Stage::GpuPreprocessEnd, 1.4),
            (Stage::EncodeSubmit, 1.5),
        ]);
        assert_eq!(
            with.segment(Segment::PreprocessWait),
            Some(Nanos::from_millis_f64(0.2))
        );
        assert_eq!(
            with.segment(Segment::AdmissionWait),
            Some(Nanos::from_millis_f64(0.1))
        );
    }

    #[test]
    fn missing_instrumentation_lands_in_the_gap_not_in_a_segment() {
        // Decoder-only pipeline: nothing is known between birth and decode.
        let timeline = timeline(&[
            (Stage::FrameCreated, 0.0),
            (Stage::DecodeSubmit, 4.0),
            (Stage::DecodeComplete, 5.6),
            (Stage::RenderSubmit, 5.7),
            (Stage::PresentSubmit, 6.0),
        ]);

        assert_eq!(
            timeline.segment(Segment::Decode),
            Some(Nanos::from_millis_f64(1.6))
        );
        assert_eq!(timeline.segment(Segment::Capture), None);
        assert_eq!(timeline.attributed(), Nanos::from_millis_f64(2.0));
        assert_eq!(
            timeline.unattributed_gap(),
            Some(Nanos::from_millis_f64(4.0))
        );
    }

    #[test]
    fn waits_and_work_are_totalled_separately() {
        let timeline = timeline(&[
            (Stage::DecodeSubmit, 0.0),
            (Stage::DecodeComplete, 1.6),
            (Stage::RenderSubmit, 3.0),
            (Stage::PresentSubmit, 3.3),
        ]);

        assert_eq!(
            timeline.total(SegmentKind::Work),
            Nanos::from_millis_f64(1.9)
        );
        assert_eq!(
            timeline.total(SegmentKind::Wait),
            Nanos::from_millis_f64(1.4)
        );
        assert_eq!(timeline.total(SegmentKind::Wire), Nanos::ZERO);
    }

    #[test]
    fn marks_from_different_clocks_are_flagged() {
        let mut timeline = FrameTimeline::new(FrameId::new(1));
        timeline.set(
            Stage::NetworkSendLast,
            Mark {
                at: Timestamp::from_nanos(1_000_000),
                domain: ClockDomain::LocalWindows,
            },
        );
        timeline.set(
            Stage::NetworkReceiveFirst,
            Mark {
                at: Timestamp::from_nanos(1_400_000),
                domain: ClockDomain::LocalMac,
            },
        );

        let sample = timeline.segments().next().expect("network segment");
        assert_eq!(sample.segment, Segment::Network);
        assert!(sample.cross_domain);
    }

    #[test]
    fn a_mark_out_of_order_does_not_fabricate_a_duration() {
        let timeline = timeline(&[
            (Stage::DecodeSubmit, 5.0),
            (Stage::DecodeComplete, 4.0),
            (Stage::PresentSubmit, 6.0),
        ]);
        assert_eq!(timeline.segment(Segment::Decode), None);
    }

    #[test]
    fn duplicate_marks_keep_the_first_timestamp() {
        let mut timeline = FrameTimeline::new(FrameId::new(1));
        assert!(timeline.set(Stage::EncodeSubmit, mark(1.0)));
        assert!(!timeline.set(Stage::EncodeSubmit, mark(5.0)));
        assert_eq!(timeline.at(Stage::EncodeSubmit), Some(mark(1.0).at));
    }

    #[test]
    fn every_segment_is_reachable_from_the_chain() {
        for segment in Segment::ALL {
            assert!(
                EDGES.iter().any(|(_, _, named)| *named == segment),
                "{segment:?} has no edge in the chain"
            );
        }
        for (from, to, _) in EDGES {
            let from_position = CHAIN.iter().position(|stage| *stage == from);
            let to_position = CHAIN.iter().position(|stage| *stage == to);
            assert!(
                from_position < to_position,
                "{from:?} -> {to:?} runs backwards"
            );
        }
    }
}
