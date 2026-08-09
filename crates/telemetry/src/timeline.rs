use core::fmt;

use lanplay_protocol::FrameId;

use crate::clock::{Nanos, Timestamp};
use crate::stage::{STAGE_COUNT, Stage};

/// A named interval between two stages.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Span {
    pub name: &'static str,
    pub from: Stage,
    pub to: Stage,
}

impl Span {
    /// True when the two endpoints are timestamped on different machines, so
    /// the interval is only trustworthy after clock-offset correction.
    pub const fn crosses_wire(&self) -> bool {
        !matches!(
            (self.from.side(), self.to.side()),
            (crate::stage::Side::Host, crate::stage::Side::Host)
                | (crate::stage::Side::Client, crate::stage::Side::Client)
        )
    }
}

/// The chain of intervals that make up one frame's software pipeline.
///
/// They are deliberately non-overlapping and ordered, so their sum is a real
/// number: the time the pipeline spent *working*. Whatever is left between the
/// sum and [`FRAME_AGE`] is queueing and admission delay, which is exactly the
/// quantity later phases are built to squeeze.
pub const SPANS: &[Span] = &[
    Span {
        name: "capture",
        from: Stage::FrameCreated,
        to: Stage::CaptureAcquired,
    },
    Span {
        name: "gpu preprocess",
        from: Stage::GpuPreprocessStart,
        to: Stage::GpuPreprocessEnd,
    },
    Span {
        name: "encode",
        from: Stage::EncodeSubmit,
        to: Stage::EncodeComplete,
    },
    Span {
        name: "packetization",
        from: Stage::PacketizationStart,
        to: Stage::NetworkSendFirst,
    },
    Span {
        name: "send",
        from: Stage::NetworkSendFirst,
        to: Stage::NetworkSendLast,
    },
    Span {
        name: "network",
        from: Stage::NetworkSendLast,
        to: Stage::NetworkReceiveFirst,
    },
    Span {
        name: "receive",
        from: Stage::NetworkReceiveFirst,
        to: Stage::NetworkReceiveLast,
    },
    Span {
        name: "reassembly",
        from: Stage::NetworkReceiveLast,
        to: Stage::FrameReassembled,
    },
    Span {
        name: "decode",
        from: Stage::DecodeSubmit,
        to: Stage::DecodeComplete,
    },
    Span {
        name: "render",
        from: Stage::RenderSubmit,
        to: Stage::PresentSubmit,
    },
];

/// Content-to-photon-minus-panel: how stale a frame is when it reaches the
/// compositor. The metric the 1080p120 gate is judged on.
pub const FRAME_AGE: Span = Span {
    name: "frame age",
    from: Stage::FrameCreated,
    to: Stage::PresentSubmit,
};

/// Every timestamp collected for one frame.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct FrameTimeline {
    frame: FrameId,
    marks: [Option<Timestamp>; STAGE_COUNT],
}

impl FrameTimeline {
    pub(crate) fn new(frame: FrameId) -> Self {
        FrameTimeline {
            frame,
            marks: [None; STAGE_COUNT],
        }
    }

    /// Records `at` for `stage`. Returns false if the stage was already set,
    /// which the collector counts as a duplicate rather than overwriting: the
    /// first mark is the one the code intended.
    pub(crate) fn set(&mut self, stage: Stage, at: Timestamp) -> bool {
        let slot = &mut self.marks[stage.index()];
        if slot.is_some() {
            return false;
        }
        *slot = Some(at);
        true
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.marks.iter().all(Option::is_none)
    }

    pub fn frame(&self) -> FrameId {
        self.frame
    }

    pub fn mark(&self, stage: Stage) -> Option<Timestamp> {
        self.marks[stage.index()]
    }

    /// Duration of `span`, or `None` if either endpoint is missing or the
    /// interval came out negative.
    pub fn span(&self, span: &Span) -> Option<Nanos> {
        let from = self.mark(span.from)?;
        let to = self.mark(span.to)?;
        to.since(from)
    }

    pub fn frame_age(&self) -> Option<Nanos> {
        self.span(&FRAME_AGE)
    }

    /// Sum of every measured span: time the pipeline spent doing work.
    pub fn pipeline_total(&self) -> Nanos {
        SPANS
            .iter()
            .filter_map(|span| self.span(span))
            .fold(Nanos::ZERO, |acc, d| acc + d)
    }

    /// Frame age minus pipeline work: time the frame spent waiting in queues
    /// and admission gates.
    pub fn queueing(&self) -> Option<Nanos> {
        let age = self.frame_age()?;
        age.get()
            .checked_sub(self.pipeline_total().get())
            .map(Nanos)
    }

    /// A frame is complete when it was both born and presented.
    pub fn is_complete(&self) -> bool {
        self.mark(Stage::FrameCreated).is_some() && self.mark(Stage::PresentSubmit).is_some()
    }

    /// Raw stage marks, for dumping an unaggregated trace.
    pub fn stages(&self) -> impl Iterator<Item = (Stage, Timestamp)> + '_ {
        Stage::ALL
            .into_iter()
            .filter_map(|stage| self.mark(stage).map(|at| (stage, at)))
    }
}

/// The per-frame report from the Fase 0 gate.
impl fmt::Display for FrameTimeline {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(f, "Frame {}", self.frame)?;
        writeln!(f)?;
        for span in SPANS {
            write_row(f, span.name, self.span(span))?;
        }
        if let Some(queueing) = self.queueing() {
            write_row(f, "queueing", Some(queueing))?;
        }
        writeln!(f)?;
        write_row(f, "software pipeline", Some(self.pipeline_total()))?;
        write_row(f, FRAME_AGE.name, self.frame_age())
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

    fn at(ms: f64) -> Timestamp {
        Timestamp::from_nanos((ms * 1_000_000.0) as u64)
    }

    #[test]
    fn spans_are_ordered_and_non_overlapping() {
        let mut previous_end = Stage::FrameCreated.index();
        for span in SPANS {
            assert!(
                span.from.index() >= previous_end,
                "{} overlaps the previous span",
                span.name
            );
            assert!(span.to.index() > span.from.index(), "{}", span.name);
            previous_end = span.to.index();
        }
    }

    #[test]
    fn queueing_is_frame_age_minus_measured_work() {
        let mut timeline = FrameTimeline::new(FrameId::new(7));
        timeline.set(Stage::FrameCreated, at(0.0));
        timeline.set(Stage::CaptureAcquired, at(1.0));
        timeline.set(Stage::EncodeSubmit, at(2.0));
        timeline.set(Stage::EncodeComplete, at(4.0));
        timeline.set(Stage::PresentSubmit, at(10.0));

        assert_eq!(timeline.pipeline_total(), Nanos::from_millis(3));
        assert_eq!(timeline.frame_age(), Some(Nanos::from_millis(10)));
        assert_eq!(timeline.queueing(), Some(Nanos::from_millis(7)));
        assert!(timeline.is_complete());
    }

    #[test]
    fn missing_endpoints_do_not_fabricate_zero() {
        let mut timeline = FrameTimeline::new(FrameId::new(1));
        timeline.set(Stage::EncodeSubmit, at(1.0));
        assert_eq!(timeline.span(&SPANS[2]), None);
        assert_eq!(timeline.frame_age(), None);
        assert_eq!(timeline.queueing(), None);
        assert!(!timeline.is_complete());
    }

    #[test]
    fn duplicate_marks_keep_the_first_timestamp() {
        let mut timeline = FrameTimeline::new(FrameId::new(1));
        assert!(timeline.set(Stage::EncodeSubmit, at(1.0)));
        assert!(!timeline.set(Stage::EncodeSubmit, at(5.0)));
        assert_eq!(timeline.mark(Stage::EncodeSubmit), Some(at(1.0)));
    }

    #[test]
    fn wire_crossing_spans_are_flagged() {
        let network = SPANS.iter().find(|s| s.name == "network").unwrap();
        let encode = SPANS.iter().find(|s| s.name == "encode").unwrap();
        assert!(network.crosses_wire());
        assert!(!encode.crosses_wire());
        assert!(FRAME_AGE.crosses_wire());
    }
}
