#!/usr/bin/env bash
# Is this link in a condition anybody can interpret a run against?
#
# Every audio measurement this project has taken was taken on a link nobody had
# characterised. A6 was sampled at -52 dBm and 1200 Mbps immediately before it started
# and read -70 dBm at t=0 falling to -78 by t=120, at 288 to 432 Mbps; A8 swept thirteen
# arms whose median signal ran from -70 to -67 dBm while the rate those arms negotiated
# ran from 288 to 576 Mbps. Neither run is wrong. Both are uninterpretable, because the
# variable nobody was studying moved further than the one under study. This gate is the
# thing whose absence allowed that, asked before a run rather than reconstructed after
# it.
#
# The verdict is two-valued and it is not the usual two. A link inside the band is a
# PASS meaning start your run. A link outside it is REFUSED and exits 2, and the next
# reader will want to make that a failure, so: a radio that moves is not a defect in
# this system. It is the absence of a condition under which this system can be measured
# at all, and calling it a failure would be this gate claiming something about a
# pipeline it never touched. There is no exit 1 here and there is no arrangement of the
# air that produces one.
#
# For the same reason the band does not go through `xtask verdict` and the checks are
# not an envelope. A check in an envelope is a statement about a measurement that
# happened, and the evaluator maps a criterion that was read and disagreed onto Failed
# unconditionally, which is correct and which is exactly the mapping this gate must not
# make. If a link outside the band means nothing was measured then the band is a
# precondition and not a criterion, and a precondition belongs where `xtask gates
# --runnable` and audio-e2e-gate.sh's preflight block already put theirs: asked first,
# refusing before anything is timed.
#
# ---------------------------------------------------------------------------
# Where the band came from
# ---------------------------------------------------------------------------
#
# One number, measured, applied three ways. Across A8's thirteen sweep arms the median
# signal ran from -70 to -67 dBm, a spread of three decibels, and over those same arms
# the median negotiated rate ran from 288 to 576 Mbps, a factor of two. Three decibels
# is therefore the measured amount of level movement that doubles this radio's rate, and
# a run that moves by it carries inside itself the difference that made those thirteen
# arms incomparable with each other. That is the budget. It is a ceiling and not a
# comfort margin.
#
# The population it was checked against is nineteen windows of 60 to 600 seconds:
# thirteen A8 sweep arms and A6's first two minutes, all committed on channel 36, and
# five taken for this gate on channel 100 and committed beside it. Eighteen of them
# were taken on a link that was not moving within itself, and A6's was the one everybody
# agrees was contaminated. `tools/radio-preflight.sh --band` recomputes the whole table
# from those files, so the arithmetic below is code rather than a comment that can rot.
#
# What separates the contaminated window from the other eighteen is movement and only
# movement. Its least-squares slope is -4.305 dB/min against a population whose slopes
# have mean 0.454 and standard deviation 0.411 dB/min and whose largest is 1.388, and
# the step between its half medians is -6.0 dB against a population inside +-1.5 dB.
#
# What does not separate them is the statistic a reader reaches for first. Signal range,
# max minus min, is 3 to 7 dB over the eighteen and 10 dB over the contaminated one -
# a gap too thin to put a limit in, and the 7 dB end of it belongs to the steadiest
# window ever measured here, ten minutes that moved 1.09 dB in total. Range cannot
# express a three decibel budget because a link that is not moving already spends more
# than three decibels of it standing still. It is reported and it is not a criterion.
#
# Coverage does not separate them either, and the reason is worth recording because it
# nearly became a criterion. A6's window holds 110 rows over 119.9 seconds, which reads
# as ten absent rows and an eight per cent hole until the row spacing is looked at: that
# trace was taken at 1100 ms and 110 rows is every row it was ever due. Coverage is
# computed here against the interval a trace was actually taken at, never against a
# nominal second, and the control arm below is handed 1100 ms for that reason.
#
# The window's length is derived rather than chosen. Projecting a slope measured over a
# short window across a long run is a prediction, and the prediction has an error: at
# this link's residual scatter of 0.68 to 1.26 dB the standard error of the slope
# measured 0.108 to 0.198 dB/min over the 120 second windows, 0.458 over the 60 second
# one and 0.013 over the 600 second one. Ten times the 60 second figure is 4.6 dB, so a
# one minute window cannot tell a link that moves the whole budget over a ten minute run
# from one that does not move at all, and a window that cannot resolve the budget has
# not measured anything. That is a criterion here, and it is what makes 120 seconds the
# default rather than taste.
#
# Three of the six criteria have never fired on any window ever measured: coverage, the
# channel-width-and-band criterion, and the rate's halves. They are guards against
# failure modes this link has not yet shown, and saying so is more useful than implying
# they were tuned.
#
# ---------------------------------------------------------------------------
# What this gate does not decide
# ---------------------------------------------------------------------------
#
# It says nothing about level. The only level this repository has evidence about is the
# one A6 lost continuity at, and there is no measurement here of a level that works, so
# a signal floor would be an invented number wearing a criterion's clothes. A steady
# link at -78 dBm passes this gate, and an A6 that then fails on it is a real result
# about a weak link rather than a contaminated one. The level is recorded in the label
# instead, which is the half of this that A8 lacked.
#
# Nor does it refuse a channel for being DFS, though it records that it is one. The
# five windows committed here were taken on channel 100, which requires radar detection,
# and across ten unbroken minutes of it the trace shows no hold, no vacate, 600 rows of
# 600, and 1.09 dB of projected movement - steadier on every statistic than the thirteen
# channel 36 windows the sweep was taken on, at twice their rate and seven decibels
# better signal. Refusing that on principle would refuse the best link this project has
# measured, over a mechanism the trace does not show, which is inventing a threshold.
# What the trace also cannot do is exclude a radar hold, because a hold is rare and
# abrupt and ten minutes at 1 Hz has no power against it. So the band is recorded in the
# label and carried into the run, a window in which the channel, the width or the band
# changes is refused because a vacate is measurable, and a run on a DFS channel is not
# comparable with one that is not.
#
# usage:
#   tools/radio-preflight.sh [WINDOW_SECONDS]
#   tools/radio-preflight.sh --band
#
# env:
#   RUN_SECONDS   the run this is a preflight for, in seconds; default 600, A6's long arm
#   INTERVAL_MS   sampling interval; default 1000, and the floor radio-sample enforces
#                 is 50 because one CoreWLAN association read costs 3.2 ms at p50
#   OUT           where the window and its label are written
#   WINDOW_CSV    a window already recorded, read instead of taking a new one. Two uses,
#                 both real: every audio harness already writes a radio trace beside its
#                 run, so a run that has finished can be asked afterwards whether it was
#                 ever interpretable; and the refusal path is exercisable without
#                 waiting for the weather to turn, which is the only way a gate whose
#                 subject is the weather can be tested at all. INTERVAL_MS must then be
#                 the interval that trace was taken at
#   CONTROL_CSV   the control window, overridable so that the check on the control -
#                 that it refused on the deciding criterion and not merely refused - can
#                 be shown firing rather than assumed
#
# exit 0  PASS: the link held still for long enough to project across the run, so start it
# exit 2  REFUSED: it did not, or the window could not be read, or the control arm came
#         out positive. Never 1: this gate has no opinion about the pipeline

set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
MODE="${1:-window}"
RUN_SECONDS="${RUN_SECONDS:-600}"
INTERVAL_MS="${INTERVAL_MS:-1000}"

# The one window in this repository that everybody agrees was contaminated, and the
# negative control. A synthetic band tightened until it fires tests the arithmetic
# against itself, and a scan run underneath the sampler depends on the radio's mood on
# the day; this is a real measured window, it is the run this gate exists to have
# stopped, and it costs no wall clock. Its interval is 1100 ms and not 1000, which is
# the near-miss recorded above.
CONTROL_CSV="${CONTROL_CSV:-$REPO/results/audio/e2e-corrected/radio-trace-first-120s.csv}"
CONTROL_INTERVAL_MS="${CONTROL_INTERVAL_MS:-1100}"
CONTROL_RUN_S=600

# Every trace the band rests on, so that `--band` recomputes rather than recites.
BAND_GLOBS=(
    "$REPO/results/audio/radio-preflight/*.csv"
    "$REPO/results/audio/jitter-target-sweep/*.radio.csv"
)

if [ "$MODE" = "--band" ]; then
    python3 "$REPO/tools/radio-preflight.py" band "$CONTROL_CSV" "$CONTROL_INTERVAL_MS" \
        "${BAND_GLOBS[@]}"
    exit 0
fi

WINDOW_S="${1:-120}"

OUT="${OUT:-/tmp/radio-preflight/$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$OUT"
echo "results   $OUT"
if [ -n "${WINDOW_CSV:-}" ]; then
    echo "window    replayed from $WINDOW_CSV at ${INTERVAL_MS} ms, as a preflight for a"
    echo "          ${RUN_SECONDS} s run"
else
    echo "window    ${WINDOW_S} s at ${INTERVAL_MS} ms, as a preflight for a ${RUN_SECONDS} s run"
fi
echo "requires  an association this Mac can read; nothing crosses the air here, so this"
echo "          gate needs no second machine and no endpoint, and a Mac with no"
echo "          association is refused with that as the reason rather than filtered out"
echo "          of a listing"

# A sampler left behind by an interrupted window holds nothing and breaks nothing, but a
# second one running underneath the next window is another reader of the same interface
# and this gate's whole subject is not disturbing the radio it measures. Armed only on
# the branch that starts one: a replay owns no process, and a trap that killed a sampler
# it did not start would reach into whatever else on this machine was mid-window.
cleanup() {
    pkill -f "$REPO/target/release/radio-sample" 2>/dev/null || true
}

if [ -n "${WINDOW_CSV:-}" ]; then
    cp "$WINDOW_CSV" "$OUT/window.csv"
    echo "replayed  $(($(wc -l <"$OUT/window.csv") - 1)) rows, nothing sampled"
else
    trap cleanup EXIT INT TERM
    if ! cargo build --release -q -p lanplay-radio-sample; then
        echo
        echo "REFUSE radio-sample would not build, so no window was taken and nothing here"
        echo "       says anything about the link in either direction"
        exit 2
    fi

    # --seconds rather than the positional spelling: a window whose length is silently
    # not the length that was asked for is the defect that filed a two minute trace as
    # covering a seventeen minute run, and the named form is refused rather than
    # defaulted.
    set +e
    "$REPO/target/release/radio-sample" --seconds "$WINDOW_S" --interval-ms "$INTERVAL_MS" \
        >"$OUT/window.csv" 2>"$OUT/window.err"
    sampled=$?
    set -e
    if [ "$sampled" -ne 0 ]; then
        echo
        echo "REFUSE radio-sample exited $sampled: $(tr -d '\n' <"$OUT/window.err")"
        echo "       There is no association to characterise, so there is no link to be"
        echo "       inside or outside a band"
        exit 2
    fi
    echo "sampled   $(($(wc -l <"$OUT/window.csv") - 1)) rows"
fi
echo

set +e
python3 "$REPO/tools/radio-preflight.py" gate \
    "$OUT/window.csv" "$INTERVAL_MS" "$RUN_SECONDS" \
    "$CONTROL_CSV" "$CONTROL_INTERVAL_MS" "$CONTROL_RUN_S" \
    "$OUT/label.json"
verdict=$?
set -e
exit "$verdict"
