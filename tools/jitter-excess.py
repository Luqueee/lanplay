#!/usr/bin/env python3
"""The arithmetic A8.1's harness decides on, in one place both it and its own
self-test call.

Separate from `tools/jitter-excess.sh` for the reason `tools/radio-preflight.py`
is separate from its shell: the instruments in this project have been wrong more
often than the code they measured, and a verdict that can only be exercised by
spending ten minutes of radio is a verdict nobody exercises. Every refusal below
is reachable from `selftest`, which builds a document that trips it and requires
it to fire.

It reads the receiver's JSON document with a JSON parser and never its prose. The
excess table lives under `environment.excess` because that is the one part of the
envelope schema that takes free-form values, and `xtask verdict --observation`
reaches only the flat observations, so a second flattening in shell would be a
second set of answers that can disagree with the first.

Nothing here is a criterion in the envelope sense. `xtask verdict` decides the
receiver's own checks and the shell calls it for exactly that; what is here are
this gate's preconditions - the conditions under which a curve means anything at
all - plus its negative control. A precondition read as a criterion is the defect
`tools/radio-preflight.sh` documents at length: a refusal mapped onto Failed
claims something about a pipeline nobody touched.
"""

import contextlib
import io
import json
import sys

# ---------------------------------------------------------------------------
# Where the length of a run comes from
# ---------------------------------------------------------------------------
#
# Derived, not chosen, and the derivation is the answer to "how long".
#
# The correlated unit is the cluster and not the frame, so the precision of any
# rate quoted here goes as one over the square root of the CLUSTER count. Thirty
# clusters puts the fractional standard error at 18 per cent: a factor of two
# between two thresholds is then three standard errors and a difference of a
# quarter is not claimed. That bound is applied per threshold by the receiver -
# below it the count is reported and the rate is withheld.
#
# How long thirty clusters takes is a question about the threshold. A6 measured
# 2.08 per cent of its datagrams past a 10 ms target over 600 s and 1.68 per cent
# over 60 s, with p99 only 0.8 ms late against that target - so the population
# past 20 ms is well under one per cent, order two per thousand. At the wire's 200
# datagrams a second that is 0.4 late frames a second, and at the cluster sizes
# A8's bursts showed it is of order six clusters a minute. Thirty clusters is then
# 300 s at 20 ms, and 600 s is that with a factor of two in hand.
#
# Six hundred seconds is also the arm A6 already ran on this link, so it is a
# length this pair of machines has demonstrated it can hold rather than a number
# this file invented.
#
# And the derivation has a consequence that has to be stated rather than hidden:
# at 100 ms this link produced roughly one arrival per two-minute arm, so thirty
# clusters there would be an hour of measurement and this link does not stand
# still for an hour. No run this gate can take will quote a rate at 100 ms. The
# curve reaches out there because its SHAPE says whether this is one heavy
# distribution or a normal regime with a second class of stall behind it, and that
# is read off the histogram rather than off a rate.
MINIMUM_SPAN_S = 600.0

# The receiver's own block, restated because the block count is one of the things
# refused on and a reader should not have to open Rust to find it.
BLOCK_SECONDS = 10.0

# What A7 measured this pair at, referred to the Mac's timebase. A finding and
# never a criterion: A7 measured a pair of crystals directly and this measures the
# same pair through a radio and a jitter buffer, so a disagreement is a result
# about one of the two instruments.
A7_PPM = 9.29

# Above this the curve reports a shape and authorises nothing. Targets above 20 ms
# are a product decision about the latency budget, taken elsewhere; the rows above
# it are here because the shape is the diagnostic.
AUTHORISED_MS = 20


class Refused(Exception):
    """A condition under which nothing here says anything either way."""


def observation(document, name):
    value = document.get("observations", {}).get(name)
    if value is None:
        raise Refused(
            f"the document states no {name}, so the criterion that reads it was never "
            f"evaluated; an absent observation is not a zero, and this project has read one "
            f"as a zero five times"
        )
    return value


def curve_of(document):
    """The excess table, or the reason there is not one."""
    excess = document.get("environment", {}).get("excess")
    if excess is None:
        raise Refused(
            "the document carries no environment.excess table at all, so it was written by a "
            "receiver that does not record the primitive this gate is about"
        )
    if "thresholds" not in excess:
        raise Refused(
            f"the receiver filed {excess.get('arrivals', 0)} arrivals, dropped "
            f"{excess.get('arrivals_dropped', 0)} for want of room, saw "
            f"{excess.get('repeated_frames', 0)} repeated timeline positions and covered "
            f"{excess.get('blocks', 0)} blocks of "
            f"{excess.get('block_seconds', BLOCK_SECONDS):.0f} s, and produced no curve; those "
            f"four counts are the reason, and each is stated rather than left to be inferred "
            f"from the absence"
        )
    return excess


def preconditions(document, minimum_span_s=MINIMUM_SPAN_S):
    """Every condition a curve from this run means anything under.

    Each raises rather than returns, and each names the number it read. A refusal
    that does not carry its own number sends its reader into a document of two
    hundred of them to work out which one it was.
    """
    span = document.get("run", {}).get("span_s")
    if span is None:
        raise Refused("the document states no run span, so nothing here knows how long it was")

    # Loss first, because it is the one that makes every other figure a different
    # quantity. A lost frame is a timeline position nothing arrived for, so it can
    # neither extend a cluster nor close one, and a curve computed across holes is
    # a curve over a stream that was not sent.
    lost = observation(document, "rtp_lost")
    expected = observation(document, "rtp_expected")
    if expected <= 0:
        raise Refused(
            "the sequence numbers describe a span of no packets, so there is no population "
            "and a loss of zero over it is an absence rather than a clean link"
        )
    if lost > 0:
        raise Refused(
            f"{lost:.0f} of {expected:.0f} packets never arrived, which is "
            f"{lost * 100.0 / expected:.3f} per cent; a curve is a statement about delay and a "
            f"lost packet has none, so this run is refused rather than analysed - one arm of the "
            f"A8 sweep lost 382 of 23997 and that is the reason it was refused too"
        )

    # Playout continuity, and its companion. Zero underruns over zero callbacks is
    # what a device that never ran looks like, and it is the single most common way
    # a gate here has lied.
    callbacks = observation(document, "render_callbacks")
    underruns = observation(document, "render_underruns")
    if callbacks <= 0:
        raise Refused(
            f"the device ran no IO cycles, so its underrun count of {underruns:.0f} describes a "
            f"device that was never asked for audio"
        )
    if underruns > 0:
        raise Refused(
            f"the device was handed silence on {underruns:.0f} of {callbacks:.0f} cycles. That is "
            f"a click a listener hears, and it means this run measured a machine that could not "
            f"keep a ring full rather than a link that delivered late - 40 of 40 committed "
            f"envelopes in this repository read zero here, so a non-zero is a change at this end "
            f"and not in the air"
        )

    # An off-grid timestamp has no timeline position, so it is not in the
    # population the curve is computed over. A non-zero count means the population
    # and the stream are not the same set of frames.
    off_grid = observation(document, "rtp_off_grid")
    if off_grid > 0:
        raise Refused(
            f"{off_grid:.0f} packets carried a timestamp off the frame grid, so they have no "
            f"position on the timeline and are not in the curve's population; the curve would "
            f"describe a subset of the stream without saying which"
        )

    if span < minimum_span_s:
        raise Refused(
            f"the run covered {span:.1f} s against the {minimum_span_s:.0f} s a cluster rate "
            f"needs. Thirty clusters is where a rate's fractional standard error reaches 18 per "
            f"cent, and at the two per thousand of frames this link put past 20 ms that is "
            f"300 s; {minimum_span_s:.0f} s is that with a factor of two in hand, and is the arm "
            f"A6 already held on this link"
        )

    # The instrument's own accounting, refused before its output is read. A trace
    # that filled up measured the first part of a run.
    dropped = observation(document, "excess_arrivals_dropped")
    if dropped > 0:
        raise Refused(
            f"{dropped:.0f} arrivals were dropped for want of room in the trace, so anything "
            f"computed from it describes the first part of a run; the curve is refused rather "
            f"than reported short"
        )
    repeated = observation(document, "excess_repeated_frames")
    if repeated > 0:
        raise Refused(
            f"{repeated:.0f} arrivals claimed a timeline position another had already taken, "
            f"which splits one cluster into two and cannot happen on a stream whose duplicates "
            f"the buffer refuses; the instrument disagrees with itself"
        )
    if observation(document, "excess_arrivals") <= 0:
        raise Refused(
            "no arrival reached the trace at all, so every threshold below is an absence rather "
            "than a zero"
        )

    curve = curve_of(document)

    # Two accountings of the same fact, and they have to agree. Loss is zero above,
    # so the timeline cannot have holes in it, and a break here with no loss there
    # would mean the sequence accounting and the timestamp accounting disagree
    # about what arrived.
    missing = curve["frames_missing"]
    breaks = curve["sequence_breaks"]
    if missing > 0 or breaks > 0:
        raise Refused(
            f"the timeline has {missing} positions nothing arrived for across {breaks} breaks, "
            f"while the sequence numbers report no loss at all; the two accountings disagree, "
            f"and until that is settled neither is evidence"
        )

    blocks_needed = int(minimum_span_s / BLOCK_SECONDS)
    if curve["blocks"] < blocks_needed:
        raise Refused(
            f"the run covered {curve['blocks']} blocks of {BLOCK_SECONDS:.0f} s against the "
            f"{blocks_needed} a {minimum_span_s:.0f} s run has; the drift is fitted through one "
            f"minimum per block, so a run with fewer blocks than its own length implies has gaps "
            f"the fit would be interpolating across"
        )

    # Raising a threshold delays playout strictly, so every frame late at the
    # smaller threshold is late at the larger. A curve that rises was computed
    # against a reference that moved between two of its own rows, which would make
    # every row of it a different measurement.
    rows = curve["thresholds"]
    for before, after in zip(rows, rows[1:]):
        if after["late"] > before["late"]:
            raise Refused(
                f"the curve rises: {before['ms']} ms left {before['late']} frames late and "
                f"{after['ms']} ms left {after['late']}. Raising a threshold cannot make more "
                f"frames late on one population, so this is one reference per row rather than "
                f"one population"
            )
    return curve


def report(document, curve):
    """The curve, the cluster table and the findings, for a person."""
    drift = curve["drift"]

    print(f"population {curve['population']} arrivals over {curve['stream_s']:.1f} s of stream "
          f"time, {curve['blocks']} blocks of {BLOCK_SECONDS:.0f} s")
    print(f"           0 packets lost, 0 timeline holes, "
          f"{observation(document, 'render_underruns'):.0f} render underruns over "
          f"{observation(document, 'render_callbacks'):.0f} callbacks")
    print()

    # Two quantities and both printed. The delay slope is what the correction
    # subtracts and the source rate is its negation; a fast source makes the
    # subtracted RTP term outrun arrival time, so they have opposite signs. This
    # gate's first radio run compared the wrong one against A7 and reported a
    # disagreement that was a convention error, so the comparison is recomputed
    # here from the source rate rather than taken from the document's own boolean:
    # a finding this harness prints is a finding this harness owns.
    source_ppm = drift["source_ppm"]
    agrees = (source_ppm * A7_PPM > 0
              and abs(A7_PPM) / 2.0 <= abs(source_ppm) <= abs(A7_PPM) * 2.0)
    print("drift")
    print(f"  source clock      {source_ppm:+.2f} ppm referred to this Mac's timebase, fitted "
          f"through one minimum per block")
    print(f"  A7's figure       {A7_PPM:+.2f} ppm for this same pair of machines")
    print(f"  delay slope       {drift['delay_slope_ppm']:+.2f} ppm, which is what the correction "
          f"subtracts; a fast source")
    print(f"                    makes the RTP term outrun arrival time, so the two signs are "
          f"opposite by construction")
    print(f"  all points        {drift['source_ppm_all_points']:+.2f} ppm, the estimator a burst "
          f"destroys, here to be compared against")
    print(f"  accumulated       {drift['accumulated_ms']:+.2f} ms of delay over the run, against "
          f"the 5 ms between adjacent targets")
    print(f"  removed from p99  {curve['raw']['p99_ms'] - curve['corrected']['p99_ms']:+.3f} ms, "
          f"and {curve['raw']['max_ms'] - curve['corrected']['max_ms']:+.3f} ms from the maximum")
    if agrees:
        print(f"  FINDING           the two agree to within a factor of "
              f"{max(abs(source_ppm), abs(A7_PPM)) / min(abs(source_ppm), abs(A7_PPM)):.2f}, so "
              f"the correction is the")
        print("                    clock pair and not something this run invented. They are not "
              "owed an exact")
        print("                    match: A7 compared crystals directly and this compares the "
              "source's audio")
        print("                    clock against this Mac's monotonic clock through a radio and a "
              "jitter buffer.")
    else:
        print(f"  FINDING           the fitted {source_ppm:+.2f} ppm and A7's {A7_PPM:+.2f} ppm "
              f"DISAGREE by more than a factor")
        print("                    of two. One of the two measurements is wrong. Neither may be "
              "cited until")
        print("                    that is settled, and this gate says so rather than proceeding "
              "quietly.")
    print()

    # Before the table, because a reader who meets the 5 ms row first will read
    # half the population late as a broken link.
    cadence = curve.get("pair_cadence_ms")
    if cadence is not None:
        row = row_at(curve, cadence)
        print(f"pair cadence, and it is not the link")
        print(f"  the {cadence} ms row has {row['late_fraction'] * 100:.2f} per cent of the "
              f"population late, in clusters of one")
        print(f"  frame separated by gaps of one frame. That alternation is not something a radio "
              f"can do:")
        print(f"  a burst is consecutive by definition, so no link leaves every second frame on "
              f"time. Two")
        print(f"  Opus frames ride in one captured packet, so both arrive at one instant while "
              f"the second")
        print(f"  sits a frame later in stream time and its excess is exactly a frame lower. A6.1 "
              f"measured")
        print(f"  the same thing from the other side at -4.996 ms per pair at p50, 96 per cent of "
              f"pairs")
        print(f"  inside the [-5,-4) ms bucket over 8998, 9000 and 120004 pairs, and found the "
              f"first member")
        print(f"  is the one that goes late in practice: 524 against 384, 476 against 354, 8594 "
              f"against 6391.")
        print()
        print(f"  What that establishes is a FLOOR: a target below the pair spacing cannot hold "
              f"both members")
        print(f"  of a pair, so {cadence} ms is structurally unreachable on this sender for a "
              f"reason that has")
        print(f"  nothing to do with the air. What it does not establish is that spacing the pair "
              f"in the")
        print(f"  sender would be an improvement: that would also delay the second frame by a "
              f"frame in real")
        print(f"  time, and whether the floor it removes is worth the delay it adds is arithmetic "
              f"nobody has")
        print(f"  done. An argument from this structure for spacing was made and retracted "
              f"earlier in this")
        print(f"  session for being wrong by a sign; nothing here revives it.")
    else:
        print("pair cadence      no row alternates, so no row is the sender's packing and every")
        print("                  row below is the link")
    print()

    print("excess above this run's own best case, in milliseconds")
    for name in ("raw", "corrected"):
        shape = curve[name]
        print(f"  {name:<10} p50 {shape['p50_ms']:7.3f}  p95 {shape['p95_ms']:7.3f}  "
              f"p99 {shape['p99_ms']:7.3f}  max {shape['max_ms']:8.3f}  "
              f"past 100 ms {shape['over_100ms']}")
    print()

    print(f"the curve, on drift-corrected excess. Rows above {AUTHORISED_MS} ms are shape and "
          f"authorise no target")
    print(f"  {'T ms':>5}{'late':>8}{'late %':>9}{'/min':>9}{'raw':>8}"
          f"{'clusters':>10}{'/min':>9}{'frames p50/p95/max':>20}"
          f"{'worst ms':>10}{'gap ms p50/min':>16}{'per block':>12}")
    for row in curve["thresholds"]:
        rate = f"{row['clusters_per_min']:9.2f}" if row["rate_quotable"] else f"{'-':>9}"
        frames = (f"{row['cluster_frames_p50']}/{row['cluster_frames_p95']}/"
                  f"{row['cluster_frames_max']}") if row["clusters"] else "-"
        worst = f"{row['cluster_worst_max_ms']:.1f}" if row["clusters"] else "-"
        gap = (f"{row['cluster_gap_p50_ms']:.0f}/{row['cluster_gap_min_ms']:.0f}"
               if row["cluster_gap_p50_ms"] is not None else "-")
        block = (f"{row['block_clusters_per_min_min']:.0f}-{row['block_clusters_per_min_max']:.0f}"
                 if row["block_clusters_per_min_min"] is not None else "-")
        mark = " " if row["ms"] <= AUTHORISED_MS else "*"
        print(f" {mark}{row['ms']:>5}{row['late']:>8}{row['late_fraction'] * 100:>9.4f}"
              f"{row['late_per_min']:>9.2f}{row['late_raw']:>8}{row['clusters']:>10}{rate}"
              f"{frames:>20}{worst:>10}{gap:>16}{block:>12}")
    print()
    print("  * the shape above 20 ms says whether this is one heavy distribution or a normal")
    print("    regime with a second class of stall behind it. A figure at 30, 50 or 80 ms")
    print("    authorises nothing: what the latency budget pays is decided elsewhere.")
    print("  a rate is a dash where the run saw fewer clusters than one needs. That is not a")
    print("    rate of zero, it is a count too small for a rate, and the count is beside it.")
    print("  the last column is the per-block spread of the cluster rate. Uncertainty here")
    print("    comes from time blocks and never from a binomial over frames: the frames inside")
    print("    a cluster are one event, so a binomial would overstate the precision by the")
    print("    mean cluster size.")
    print()

    # Beside the curve rather than instead of it. The concealment ratio is what A6
    # and A8 decided on, and it belongs here as a consequence of the curve rather
    # than as the thing being measured: at zero render underruns in every envelope
    # this project has committed, the device was fed throughout and what a target
    # buys is fidelity.
    concealed = observation(document, "concealed_samples")
    expected = observation(document, "samples_expected")
    target = document["run"]["args"]["target_ms"]
    print(f"beside it, at the {target:.0f} ms target this run was actually configured with:")
    print(f"  source concealment    {concealed:.0f} of {expected:.0f} samples, "
          f"{concealed * 100.0 / expected if expected else 0.0:.4f} per cent replaced by the "
          f"concealer")
    print(f"  playout continuity    {observation(document, 'render_underruns'):.0f} underruns "
          f"over {observation(document, 'render_callbacks'):.0f} callbacks, which is the count "
          f"that means audible silence")


def row_at(curve, at_ms):
    for row in curve["thresholds"]:
        if row["ms"] == at_ms:
            return row
    raise Refused(f"the curve reports no {at_ms} ms row, so the control cannot be decided on it")


def control(clean, faulted, injected_pct, hold_ms):
    """Whether the negative control fired, and whether it fired for its reason.

    The control injects a known delay on a known fraction of the datagrams, and
    what it has to demonstrate is that the curve SEES it. A curve that cannot see
    a population deliberately put in front of it is a curve whose clean arm means
    nothing, and this is the only arrangement that tests that: a synthetic
    assertion about the arithmetic would also pass on an instrument reading its own
    scratch buffer.

    Three things are required of the control and one of the clean arm. Both
    directions, because a criterion that cannot fail is worse than no criterion,
    and a control that fires whatever happens has proved nothing about the criteria
    it exists to exercise.
    """
    faulted_curve = curve_of(faulted)
    clean_curve = curve_of(clean)
    # Below the hold, so the whole injected population is above the threshold, and
    # well above the link's own tail, so little of the radio is counted in it.
    below = row_at(faulted_curve, 30)
    # Above the hold by half of it again, so a point mass at the hold is entirely
    # underneath and only a heavy tail could put anything here.
    above = row_at(faulted_curve, 60)
    clean_below = row_at(clean_curve, 30)

    seen_pct = below["late_fraction"] * 100.0
    verdicts = [
        # The fraction. A factor of two either way: udp-fault draws uniformly and a
        # 120000 datagram run at five per cent is six thousand events, so sampling
        # error is a fraction of a per cent and the tolerance is there for the
        # link's own contribution rather than for noise.
        (
            "the injected population is in the curve",
            injected_pct / 2.0 <= seen_pct <= injected_pct * 2.0,
            f"{seen_pct:.3f} per cent of frames past 30 ms against {injected_pct:.1f} injected",
        ),
        # A cliff and not a tail. The injected distribution is a point mass at the
        # hold, so a curve reporting the same fraction at 60 ms as at 30 is not
        # reading the injected shape - it is reading something else that is large.
        (
            "the injected shape is a step and not a tail",
            below["late"] > 0 and above["late"] * 10 < below["late"],
            f"{below['late']} frames past 30 ms and {above['late']} past 60 ms, against a "
            f"{hold_ms:.0f} ms hold",
        ),
        # Where the step is. At five per cent injected, p95 sits inside the injected
        # population, so it has to land on the hold.
        (
            "the step is where it was injected",
            abs(faulted_curve["corrected"]["p95_ms"] - hold_ms) <= 10.0,
            f"p95 at {faulted_curve['corrected']['p95_ms']:.2f} ms against a {hold_ms:.0f} ms hold",
        ),
        # And the companion, without which the three above are satisfied by an
        # instrument that reports five per cent past 30 ms on every run it sees.
        (
            "the clean arm does not show it",
            clean_below["late_fraction"] * 100.0 * 10.0 < seen_pct,
            f"{clean_below['late_fraction'] * 100.0:.4f} per cent past 30 ms on the clean arm "
            f"against {seen_pct:.3f} on the control",
        ),
    ]

    print("control   the injected distribution has to be visible in the curve, and the clean arm")
    print("          has to not show it. A curve that cannot see a population put in front of it")
    print("          is a curve whose clean arm says nothing.")
    print()
    held = True
    for name, ok, detail in verdicts:
        print(f"  {'HOLD ' if ok else 'BROKE'}  {name}: {detail}")
        held = held and ok
    return 0 if held else 1


def selftest():
    """Every refusal, fired.

    A criterion nobody has seen fire is decoration. Each case below mutates one
    field of a document that otherwise passes and requires the named refusal; the
    unmutated document is required to pass, which is the companion that keeps this
    from being satisfied by a function that refuses everything.
    """

    def good():
        rows = []
        for index, ms in enumerate([5, 10, 15, 20, 25, 30, 40, 50, 60, 80, 100]):
            late = 4000 - index * 350
            rows.append({
                "ms": ms, "late": late, "late_raw": late + 5,
                "late_fraction": late / 120000.0, "late_per_min": late / 10.0,
                "clusters": late // 2, "clusters_per_min": late / 20.0,
                "rate_quotable": late // 2 >= 30,
                "cluster_frames_p50": 1, "cluster_frames_p95": 3, "cluster_frames_max": 9,
                "cluster_ms_max": 45.0,
                "cluster_worst_p50_ms": 7.0, "cluster_worst_p95_ms": 20.0,
                "cluster_worst_max_ms": 91.0,
                "cluster_gap_min_ms": 10.0, "cluster_gap_p50_ms": 300.0,
                "cluster_gap_max_ms": 9000.0,
                "block_clusters_per_min_min": 6.0, "block_clusters_per_min_p50": 18.0,
                "block_clusters_per_min_max": 60.0,
            })
        return {
            "gate": "audio-e2e-gate",
            "run": {"started_unix_ms": 1, "span_s": 600.5, "args": {"target_ms": 10.0}},
            "observations": {
                "rtp_lost": 0, "rtp_expected": 120000, "rtp_off_grid": 0,
                "render_callbacks": 112500, "render_underruns": 0,
                "excess_arrivals": 120001, "excess_arrivals_dropped": 0,
                "excess_repeated_frames": 0,
                "concealed_samples": 240, "samples_expected": 28800000,
            },
            "environment": {"excess": {
                "arrivals": 120001, "arrivals_dropped": 0, "repeated_frames": 0,
                "blocks": 60, "block_seconds": 10.0, "bin_us": 250, "minimum_clusters": 30,
                "population": 120001, "stream_s": 600.0,
                "frames_missing": 0, "sequence_breaks": 0,
                "pair_cadence_ms": 5,
                "drift": {"source_ppm": 9.1, "source_ppm_all_points": -3.2,
                          "delay_slope_ppm": -9.1, "delay_slope_ppm_all_points": 3.2,
                          "blocks_fitted": 60, "accumulated_ms": -5.46,
                          "reference_source_ppm": 9.29, "agrees_with_reference": True},
                "raw": {"p50_ms": 0.4, "p95_ms": 2.0, "p99_ms": 9.0, "max_ms": 96.0,
                        "over_100ms": 0, "bins": []},
                "corrected": {"p50_ms": 0.4, "p95_ms": 1.9, "p99_ms": 8.4, "max_ms": 91.0,
                              "over_100ms": 0, "bins": []},
                "thresholds": rows,
            }},
        }

    def drop(document, *path):
        node = document
        for key in path[:-1]:
            node = node[key]
        del node[path[-1]]
        return document

    def observe(document, name, value):
        document["observations"][name] = value
        return document

    def excess(document, name, value):
        document["environment"]["excess"][name] = value
        return document

    def shorten(document, span_s):
        document["run"]["span_s"] = span_s
        return document

    def rising(document):
        return excess(document, "thresholds", [
            dict(row, late=row["late"] + (9999 if row["ms"] == 20 else 0))
            for row in document["environment"]["excess"]["thresholds"]
        ])

    cases = [
        ("a document with no span", lambda d: drop(d, "run", "span_s"), "no run span"),
        ("an absent observation", lambda d: drop(d, "observations", "rtp_lost"),
         "states no rtp_lost"),
        ("a span of no packets", lambda d: observe(d, "rtp_expected", 0), "span of no packets"),
        ("one lost packet", lambda d: observe(d, "rtp_lost", 1), "never arrived"),
        ("a device that never ran", lambda d: observe(d, "render_callbacks", 0),
         "ran no IO cycles"),
        ("one render underrun", lambda d: observe(d, "render_underruns", 1), "handed silence"),
        ("an off-grid timestamp", lambda d: observe(d, "rtp_off_grid", 1), "off the frame grid"),
        ("a run one second too short", lambda d: shorten(d, 599.0),
         "against the 600 s a cluster rate needs"),
        ("a trace that overflowed", lambda d: observe(d, "excess_arrivals_dropped", 1),
         "dropped for want of room"),
        ("a repeated timeline position", lambda d: observe(d, "excess_repeated_frames", 1),
         "already taken"),
        ("no arrivals at all", lambda d: observe(d, "excess_arrivals", 0),
         "no arrival reached the trace"),
        ("no excess table", lambda d: drop(d, "environment", "excess"),
         "no environment.excess table"),
        ("a table with no curve", lambda d: drop(d, "environment", "excess", "thresholds"),
         "produced no curve"),
        ("a hole in the timeline", lambda d: excess(d, "frames_missing", 4),
         "the two accountings disagree"),
        ("a break in the timeline", lambda d: excess(d, "sequence_breaks", 1),
         "the two accountings disagree"),
        ("too few blocks", lambda d: excess(d, "blocks", 59), "blocks of 10 s against the 60"),
        ("a curve that rises", rising, "the curve rises"),
    ]

    failures = []
    print("refusals  each case below mutates one field of a document that otherwise passes, and")
    print("          the named refusal has to fire on it. The unmutated document has to pass,")
    print("          which is what keeps this from being satisfied by a function that refuses")
    print("          everything.")
    print()
    try:
        preconditions(good())
        print("  HOLD    the unmutated document passes every precondition")
    except Refused as why:
        failures.append(f"the unmutated document was refused: {why}")
        print(f"  BROKE   the unmutated document was refused: {why}")

    for name, mutate, wanted in cases:
        try:
            preconditions(mutate(good()))
        except Refused as why:
            if wanted in str(why):
                print(f"  FIRED   {name}")
            else:
                failures.append(f"{name} refused for the wrong reason: {why}")
                print(f"  WRONG   {name}: refused, but for '{why}'")
        else:
            failures.append(f"{name} was not refused at all")
            print(f"  MISSED  {name}: not refused")

    # The printer too, rendered and thrown away. A missing key in the report is a
    # gate that refuses nothing, measures for ten minutes and then dies on its
    # last line, which is the most expensive place in the run to find one.
    document = good()
    try:
        with contextlib.redirect_stdout(io.StringIO()) as rendered:
            report(document, preconditions(document))
        if "the curve" in rendered.getvalue() and "pair cadence" in rendered.getvalue():
            print("  HOLD    the report renders every key the curve carries")
        else:
            failures.append("the report rendered without the curve or the cadence in it")
            print("  BROKE   the report rendered without the curve or the cadence in it")
    except Exception as why:  # noqa: BLE001 - any raise here is the defect
        failures.append(f"the report raised {why!r}")
        print(f"  BROKE   the report raised {why!r}")

    # And the control's own decision, both ways, without a radio. The real control
    # runs over the air; this is the check that the decision function itself can
    # say no.
    clean, faulted = good(), good()
    for row in faulted["environment"]["excess"]["thresholds"]:
        row["late"] = 6000 if row["ms"] <= 30 else 0
        row["late_fraction"] = row["late"] / 120000.0
    faulted["environment"]["excess"]["corrected"]["p95_ms"] = 40.0
    for row in clean["environment"]["excess"]["thresholds"]:
        row["late"] = 3 if row["ms"] <= 30 else 0
        row["late_fraction"] = row["late"] / 120000.0
    print()
    if control(clean, faulted, 5.0, 40.0) != 0:
        failures.append("a control carrying the injected shape was read as not firing")
    print()
    if control(clean, clean, 5.0, 40.0) == 0:
        failures.append("a control that injected nothing was read as firing")

    print()
    for failure in failures:
        print(f"BROKE {failure}")
    if failures:
        print(f"\nREFUSE {len(failures)} of this gate's own criteria did not behave")
        return 1
    print("PASS every refusal fired on a document built to trip it, the unmutated document held,")
    print("     and the control decision said yes to an injected shape and no to none")
    return 0


def main(argv):
    if not argv:
        print(__doc__)
        return 2
    mode = argv[0]
    if mode == "selftest":
        return selftest()
    if mode == "curve":
        document = json.load(open(argv[1]))
        minimum = float(argv[2]) if len(argv) > 2 else MINIMUM_SPAN_S
        report(document, preconditions(document, minimum))
        return 0
    if mode == "control":
        clean = json.load(open(argv[1]))
        faulted = json.load(open(argv[2]))
        return control(clean, faulted, float(argv[3]), float(argv[4]))
    print(f"unknown mode {mode}")
    return 2


if __name__ == "__main__":
    try:
        sys.exit(main(sys.argv[1:]))
    except Refused as why:
        print()
        print(f"REFUSE {why}")
        sys.exit(2)
