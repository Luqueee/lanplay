//! Contracts the rest of the pipeline relies on: nothing is silently lost, and
//! anything that is lost is counted.

use std::time::Duration;

use lanplay_protocol::FrameId;
use lanplay_telemetry::{ClockDomain, Segment, Stage, Telemetry, TelemetryConfig, Timestamp};

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
fn percentiles_reflect_recorded_segments() {
    let telemetry = Telemetry::start(config());
    let recorder = telemetry.recorder();

    // 100 frames with a deterministic 2 ms encode; the last one takes 20 ms.
    for id in 1..=100u64 {
        let frame = FrameId::new(id);
        let base = Timestamp::from_nanos(id * 100_000_000);
        let encode_ms = if id == 100 { 20 } else { 2 };
        recorder.mark_at(frame, Stage::EncodeSubmit, base);
        recorder.mark_at(
            frame,
            Stage::EncodeComplete,
            Timestamp::from_nanos(base.as_nanos() + encode_ms * 1_000_000),
        );
        recorder.mark_at(frame, Stage::PresentSubmit, base);
    }

    assert!(telemetry.flush(Duration::from_secs(2)));
    let snapshot = telemetry.shutdown();
    let encode = snapshot.segment(Segment::Encode);

    assert_eq!(encode.count, 100);
    assert!(
        (encode.p50.as_millis_f64() - 2.0).abs() < 0.05,
        "{encode:?}"
    );
    assert!(encode.max.as_millis_f64() >= 19.0, "{encode:?}");
    assert!(!snapshot.p99_is_soaked(), "100 frames must not claim a p99");
}

#[test]
fn marks_from_two_clocks_are_measured_but_flagged() {
    // What phase 5 will do: merge the host's marks, recorded on the Windows
    // clock, into a timeline the Mac is assembling.
    let telemetry = Telemetry::start(config());
    let local = telemetry.recorder();
    let remote = local.with_domain(ClockDomain::LocalWindows);
    let frame = FrameId::new(11);

    remote.mark_at(
        frame,
        Stage::NetworkSendFirst,
        Timestamp::from_nanos(1_000_000),
    );
    local.mark_at(
        frame,
        Stage::NetworkReceiveLast,
        Timestamp::from_nanos(1_400_000),
    );
    local.mark_at(
        frame,
        Stage::PresentSubmit,
        Timestamp::from_nanos(3_000_000),
    );

    assert!(telemetry.flush(Duration::from_secs(2)));
    let snapshot = telemetry.shutdown();

    assert_eq!(snapshot.segment(Segment::Transit).count, 1);
    assert_eq!(snapshot.counters.cross_domain_segments, 1);
    assert_eq!(snapshot.clock_domain, ClockDomain::local());
}

#[test]
fn unmeasured_time_is_reported_as_gap_not_absorbed() {
    let telemetry = Telemetry::start(config());
    let recorder = telemetry.recorder();
    let frame = FrameId::new(21);

    // Decoder-only pipeline: 4 ms of the frame's life has no instrumentation.
    recorder.mark_at(frame, Stage::FrameCreated, Timestamp::from_nanos(0));
    recorder.mark_at(frame, Stage::DecodeSubmit, Timestamp::from_nanos(4_000_000));
    recorder.mark_at(
        frame,
        Stage::DecodeComplete,
        Timestamp::from_nanos(5_600_000),
    );
    recorder.mark_at(
        frame,
        Stage::PresentSubmit,
        Timestamp::from_nanos(6_000_000),
    );

    assert!(telemetry.flush(Duration::from_secs(2)));
    let snapshot = telemetry.shutdown();

    assert_eq!(snapshot.segment(Segment::Decode).count, 1);
    assert_eq!(snapshot.segment(Segment::Capture).count, 0);
    assert_eq!(snapshot.unattributed_gap.count, 1);
    assert!(
        (snapshot.unattributed_gap.p50.as_millis_f64() - 4.4).abs() < 0.1,
        "{:?}",
        snapshot.unattributed_gap
    );
}

#[test]
fn a_window_sees_a_collapse_that_the_cumulative_view_hides() {
    // The whole reason windows exist: a run that is healthy for most of its
    // length and terrible for a moment reports a healthy cumulative p99,
    // because percentiles cannot be differenced.
    let telemetry = Telemetry::start(TelemetryConfig {
        queue_capacity: 1 << 16,
        ..config()
    });
    let recorder = telemetry.recorder();

    let mut present = |frame: u64, age_ms: f64| {
        let base = Timestamp::from_nanos(frame * 8_333_333);
        recorder.mark_at(FrameId::new(frame), Stage::FrameCreated, base);
        recorder.mark_at(
            FrameId::new(frame),
            Stage::PresentSubmit,
            Timestamp::from_nanos(base.as_nanos() + (age_ms * 1_000_000.0) as u64),
        );
    };

    // Five thousand good frames, then forty bad ones: a third of a second of
    // collapse at 120 fps, which is 0.8% of the run and therefore sits below
    // the cumulative p99 entirely.
    for frame in 1..=5_000 {
        present(frame, 5.0);
    }
    assert!(telemetry.flush(Duration::from_secs(2)));
    let healthy = telemetry.take_window();
    assert_eq!(healthy.presented, 5_000);
    assert!((healthy.local_age.p99.as_millis_f64() - 5.0).abs() < 0.2);

    for frame in 5_001..=5_040 {
        present(frame, 90.0);
    }
    assert!(telemetry.flush(Duration::from_secs(2)));
    let collapsed = telemetry.take_window();
    assert_eq!(collapsed.presented, 40);
    assert!(
        collapsed.local_age.p99.as_millis_f64() > 80.0,
        "the window must show the collapse: {:?}",
        collapsed.local_age
    );

    // The cumulative view averages it away, which is exactly the trap.
    let snapshot = telemetry.shutdown();
    assert!(
        snapshot.local_age.p99.as_millis_f64() < 80.0,
        "cumulative p99 {:?} should hide the 40 bad frames among 1000 good ones",
        snapshot.local_age
    );
}

#[test]
fn an_empty_window_reports_nothing_rather_than_the_previous_one() {
    let telemetry = Telemetry::start(config());
    let recorder = telemetry.recorder();
    recorder.mark_at(
        FrameId::new(1),
        Stage::FrameCreated,
        Timestamp::from_nanos(0),
    );
    recorder.mark_at(
        FrameId::new(1),
        Stage::PresentSubmit,
        Timestamp::from_nanos(5_000_000),
    );
    assert!(telemetry.flush(Duration::from_secs(2)));

    assert_eq!(telemetry.take_window().presented, 1);
    let empty = telemetry.take_window();
    assert_eq!(empty.presented, 0);
    assert_eq!(empty.local_age.count, 0);
}
