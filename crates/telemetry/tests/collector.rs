//! Contracts the rest of the pipeline relies on: nothing is silently lost, and
//! anything that is lost is counted.

use std::time::Duration;

use lanplay_protocol::FrameId;
use lanplay_telemetry::{Stage, Telemetry, TelemetryConfig};

fn config() -> TelemetryConfig {
    TelemetryConfig {
        queue_capacity: 4096,
        ring_slots: 8,
        recent_frames: 16,
        poll_interval: Duration::from_micros(100),
        ..TelemetryConfig::default()
    }
}

#[test]
fn presented_frames_produce_a_full_timeline() {
    let telemetry = Telemetry::start(config());
    let recorder = telemetry.recorder();
    let frame = FrameId::new(1);

    for stage in Stage::ALL {
        recorder.mark(frame, stage);
    }

    assert!(telemetry.flush(Duration::from_secs(2)));
    let timeline = telemetry.frame(frame).expect("frame finalised on present");
    assert!(timeline.is_complete());
    assert_eq!(timeline.stages().count(), Stage::ALL.len());
    assert!(timeline.frame_age().is_some());

    let snapshot = telemetry.shutdown();
    assert_eq!(snapshot.counters.frames_presented, 1);
    assert_eq!(snapshot.counters.frames_incomplete, 0);
    assert_eq!(snapshot.counters.events_recorded, Stage::ALL.len() as u64);
    assert!(snapshot.is_lossless(), "{snapshot}");
}

#[test]
fn marks_arriving_out_of_stage_order_still_join_the_same_frame() {
    let telemetry = Telemetry::start(config());
    let recorder = telemetry.recorder();
    let frame = FrameId::new(3);

    // Client thread wins the race and reports decode before the host thread
    // has flushed capture: the timeline must still be assembled.
    recorder.mark(frame, Stage::DecodeComplete);
    recorder.mark(frame, Stage::FrameCreated);
    recorder.mark(frame, Stage::DecodeSubmit);
    recorder.mark(frame, Stage::PresentSubmit);

    assert!(telemetry.flush(Duration::from_secs(2)));
    let timeline = telemetry.frame(frame).expect("frame finalised");
    assert!(timeline.mark(Stage::FrameCreated).is_some());
    assert!(timeline.mark(Stage::DecodeComplete).is_some());
    assert!(timeline.is_complete());
}

#[test]
fn a_frame_that_never_presents_is_evicted_and_counted() {
    let telemetry = Telemetry::start(config());
    let recorder = telemetry.recorder();

    // Ring holds 8 frames; frame 1 never presents and is evicted by frame 9.
    recorder.mark(FrameId::new(1), Stage::FrameCreated);
    recorder.mark(FrameId::new(1), Stage::EncodeSubmit);
    for id in 2..=9 {
        let frame = FrameId::new(id);
        recorder.mark(frame, Stage::FrameCreated);
        recorder.mark(frame, Stage::PresentSubmit);
    }

    assert!(telemetry.flush(Duration::from_secs(2)));
    let snapshot = telemetry.snapshot();
    assert_eq!(snapshot.counters.frames_presented, 8);
    assert_eq!(snapshot.counters.frames_incomplete, 1);
    assert!(
        telemetry
            .frame(FrameId::new(1))
            .is_some_and(|t| !t.is_complete())
    );
}

#[test]
fn late_and_duplicate_marks_are_counted_not_merged() {
    let telemetry = Telemetry::start(config());
    let recorder = telemetry.recorder();
    let frame = FrameId::new(5);

    recorder.mark(frame, Stage::FrameCreated);
    recorder.mark(frame, Stage::FrameCreated);
    recorder.mark(frame, Stage::PresentSubmit);
    assert!(telemetry.flush(Duration::from_secs(2)));
    // Arrives after the frame was finalised on present.
    recorder.mark(frame, Stage::RenderSubmit);
    assert!(telemetry.flush(Duration::from_secs(2)));

    let snapshot = telemetry.shutdown();
    assert_eq!(snapshot.counters.duplicate_marks, 1);
    assert_eq!(snapshot.counters.late_events, 1);
    assert_eq!(snapshot.counters.frames_presented, 1);
    assert!(!snapshot.is_lossless());
}

#[test]
fn a_full_queue_drops_marks_instead_of_blocking() {
    let telemetry = Telemetry::start(TelemetryConfig {
        queue_capacity: 4,
        ring_slots: 8,
        // Park the collector so the queue cannot be drained during the test.
        poll_interval: Duration::from_secs(30),
        ..TelemetryConfig::default()
    });
    let recorder = telemetry.recorder();

    // Give the collector time to enter its long sleep on an empty queue.
    std::thread::sleep(Duration::from_millis(20));
    for id in 100..200 {
        recorder.mark(FrameId::new(id), Stage::FrameCreated);
    }

    let snapshot = telemetry.snapshot();
    assert!(
        snapshot.counters.events_dropped >= 90,
        "expected drops to be counted, got {snapshot}"
    );
}

#[test]
fn percentiles_reflect_recorded_spans() {
    let telemetry = Telemetry::start(config());
    let recorder = telemetry.recorder();

    // 100 frames with a deterministic 2 ms encode; the last one takes 20 ms.
    for id in 1..=100u64 {
        let frame = FrameId::new(id);
        let base = lanplay_telemetry::Timestamp::from_nanos(id * 100_000_000);
        let encode_ms = if id == 100 { 20 } else { 2 };
        recorder.mark_at(frame, Stage::EncodeSubmit, base);
        recorder.mark_at(
            frame,
            Stage::EncodeComplete,
            lanplay_telemetry::Timestamp::from_nanos(base.as_nanos() + encode_ms * 1_000_000),
        );
        recorder.mark_at(frame, Stage::PresentSubmit, base);
    }

    assert!(telemetry.flush(Duration::from_secs(2)));
    let snapshot = telemetry.shutdown();
    let encode = snapshot
        .spans
        .iter()
        .find(|span| span.name == "encode")
        .expect("encode span");

    assert_eq!(encode.count, 100);
    assert!(
        (encode.p50.as_millis_f64() - 2.0).abs() < 0.05,
        "{encode:?}"
    );
    assert!(encode.max.as_millis_f64() >= 19.0, "{encode:?}");
}
