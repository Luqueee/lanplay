#!/usr/bin/env bash
# N3's classifier, held against every session whose answer was written down when it
# was taken.
#
# A classifier that cannot label runs already documented will not label a live one,
# and finding that out costs no hardware time at all. So this needs nothing: no
# Windows host, no radio, no display, no audio device. It reads `results/`, builds the
# middle tier of the observation contract from each session with `display` withheld,
# classifies it, and holds the label against the diagnosis in the commit that produced
# the run.
#
# Three outcomes and the third is not a softer second:
#
#   0  every session with a recorded diagnosis carries the label it implies
#   1  a label disagrees with a diagnosis
#   2  refused - a session could not be read, carried no observations, the population
#      was empty, or the table below no longer describes the corpus
#
# The table is here rather than in the binary so that the ground truth is diffable
# beside the runner and so that the third negative control can mutate it with one
# substitution. Every row cites the commit it comes from. `UNESTABLISHED` means the
# session is read, classified and printed but not checked, because nothing written
# down says what it was found to be - guessing a label and then matching it is a
# criterion that cannot fail.
#
# Three negative controls run after the main pass and each must fire:
#
#   radio-absent   the same session with and without its radio trace must carry the
#                  same label, which is the whole point of NetworkObservation.radio
#                  being an Option
#   no-population  a session doctored to have counted nothing must refuse rather
#                  than pass
#   swapped        one row's verdict exchanged for another must make the pass fail,
#                  which proves the check reads the label rather than announcing it

set -euo pipefail

REPO="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO"

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

BIN="$REPO/target/release/classify-sessions"
TABLE="$WORK/expect.tsv"

refuse() {
    echo "REFUSE $*" >&2
    exit 2
}

row() {
    printf '%s\t%s\t%s\n' "$1" "$2" "$3" >>"$TABLE"
}

# ---------------------------------------------------------------------------
# The recorded diagnoses
# ---------------------------------------------------------------------------

: >"$TABLE"

# Video, channel 116 at 80 MHz. The access point was on Auto and had picked a band
# where a radio must keep monitoring for radar while it serves, and stalls arrive on a
# 220 ms period lasting 34 ms for a duty cycle of 3.9%.
row "b3-channel/ch116-return-r1.json" CadenceDegraded \
    "f3279b2: the return arm reproduces the DFS arm to within a rounding error on every counted metric, 220 ms period at sd 3.6, zero loss"
row "b3-channel/ch116-return-r2.json" CadenceDegraded \
    "f3279b2: same signature at sd 4.4"
row "b3-channel/ch116-return-r3.json" CadenceDegraded \
    "f3279b2: same signature at sd 4.9"

# Video, channel 36 at 80 MHz, nothing else changed. ba79a59 measured the 220 ms
# period not merely smaller but absent, and 32a826f closed 116 -> 36 -> 116 -> 36 with
# the verdict `channel 36 fixed  MITIGATION VALIDATED`.
row "b3-channel/ch36-r1.json" Healthy \
    "ba79a59: non-DFS, the 220 ms period absent, zero loss; 32a826f puts this arm inside a non-DFS population disjoint from the DFS one by 45 crossings a minute"
row "b3-channel/ch36-r2.json" Healthy \
    "ba79a59: non-DFS, period absent, eleven stalls in two minutes at no period, zero loss"
row "b3-channel/ch36-r3.json" TransientStall \
    "ba79a59: three stalls in two minutes and no interval anywhere near a period, worst interval 26.07 ms. Disturbances that did not recur, which the taxonomy calls a transient and explicitly not degradation, so this does not contradict the arm's clean verdict"
row "b3-channel/ch36-return-r1.json" Healthy \
    "32a826f: the worst of the four non-DFS runs and still nowhere near the DFS population; stalls 1149 ms apart at the median, which is not a clock"

# Video, the same 220 ms clock with Apple's peer-to-peer link taken down, which is how
# AWDL was ruled out as its cause.
row "awdl/awdl-down-r1.json" CadenceDegraded \
    "e5c2b6e: 220 ms period at sd 4.4, stalls 31.1 ms, duty 4.7%, with awdl0 down"
row "awdl/awdl-down-r2.json" CadenceDegraded \
    "e5c2b6e: 220 ms period at sd 3.7, stalls 33.5 ms, duty 3.6%; the mildest committed run carrying the clock, and the run the classifier's degraded floor was derived from"

# Video, the runs that showed the bunching is already present at the BPF tap.
row "pcap-parallel/cap-control.json" CadenceDegraded \
    "e604ce1: the socket column of the first paired run, 70.0 crossings a minute, stall gap p50 222 ms; awdl/report.txt records its period as 220 ms at sd 2.9"
row "pcap-parallel/parallel-r1.json" CadenceDegraded \
    "e604ce1: the socket column of the second paired run, 71.0 crossings a minute, stall gap p50 222 ms, period 220 ms at sd 3.4"
row "pcap-parallel/parallel-r2.json" CadenceDegraded \
    "e604ce1: the socket column of the third paired run, 119.4 crossings a minute, stall gap p50 222 ms. Its span is 99 s against a nominal 120 and 2528 access units never completed with no datagram lost, which nothing written down explains; that is a truncated capture and not a cadence figure, and the cadence diagnosis is the one this row checks"
row "pcap-parallel/nocap-control.json" UNESTABLISHED \
    "its 79.9 crossings a minute match no row in e604ce1's table, so which of that commit's arms it is cannot be established from what is written down"

# Video, ten minutes on the mitigated channel with the whole pipeline running.
row "soak-1080p120/soak.json" Healthy \
    "71c1714: 72000 of 72000, zero lost, 7.7 clusters a minute with stalls 2766 ms apart and the commit's own reading of that as no clock, 95.8% fresh ticks, both gates pass"

# Video, arms whose delivery block predates crates/link-metrics' tail counters. They
# carry percentiles and no counted crossing, no cluster count and no stall gap, so no
# middle tier can be built from them at all. Zero-filling that tail would have read as
# a run that crossed no threshold, which for b1-proximity/normal-r2 would have hidden a
# single 649.59 ms interval against a p99 of 17.76 ms.
B1_WHY="delivery predates crates/link-metrics, so there is no counted crossing to read; unreadable rather than clean, and ba79a59 separately records that moving a laptop cannot touch a regulatory timer"
for arm in close-r1 close-r2 close-r3 normal-r1 normal-r2 normal-r3 return-r1 return-r2 return-r3; do
    row "b1-proximity/$arm.json" REFUSED "$B1_WHY"
done
B5_WHY="delivery predates crates/link-metrics, so there is no counted crossing to read; unreadable rather than clean, and ba79a59 separately records that resizing a datagram cannot touch a regulatory timer"
for arm in 1200-r1 1200-r2 1200-r3 1350-r1 1350-r2 1350-r3 1400-r1 1400-r2 1400-r3; do
    row "b5-datagram-size/$arm.json" REFUSED "$B5_WHY"
done

# Video, the vblank phase experiments. Three of them are refused rather than
# classified: d35ed85 deliberately applied a 3.00 ms draw to the producer, and those
# arms delivered 105.32, 109.94 and 117.96 access units a second against a target of
# 120, so their threshold crossings are counted against a period the host was not
# holding and their whole access-unit shortfall is that under-production. An earlier
# draft labelled exactly these three SevereLoss, which is what the refusal prevents.
PHASE_UNDER="d35ed85 applied a 3.00 ms producer draw; the host delivered under 99 per cent of its target rate, so the crossings measure the producer rather than the link and the access-unit shortfall is that under-production"
row "phase/acting.json" REFUSED "$PHASE_UNDER"
row "phase/control.json" REFUSED "$PHASE_UNDER"
row "phase/sign-observe.json" REFUSED "$PHASE_UNDER"
row "phase/vblank-sign.json" UNESTABLISHED \
    "d8ad516 records what this arm found about the display phase and states nothing about the link, so no network diagnosis exists to check a label against"
for arm in 1 2 3 4 5 6; do
    row "phase/lottery/$arm.json" UNESTABLISHED \
        "d8ad516 reads these six as one free-running clock drifting at 0.022 ms/s; the finding is about the phase draw and says nothing about the link"
done

# Audio. Every committed envelope from macos/audio-render is refused, and the reason is
# a property of the instrument rather than of the links those arms ran on:
# crates/link-metrics is video-side and nothing in a receive envelope counts an arrival
# crossing against the frame grid. Its arrival figures are measured against a playout
# deadline whose offset is a parameter of the jitter buffer - A8 measured the same link
# reading 0.196 to 7.442 per cent as that parameter moved - and e9f68ed established that
# the concealment figure is source fidelity, which is experience and barred from
# deciding. The socket counters are real and are printed beside every refusal.
#
# An earlier draft classified these as UnknownDegradation, which turned "this cannot be
# read" into "something is wrong with the network" and printed the cleanest arm this
# project has recorded as a degradation. That is the defect these rows now guard.
AUDIO_WHY="no delivery tier: link-metrics is video-side and no receive envelope counts an arrival crossing against the frame grid. Its arrival figures are measured against a playout deadline, which e9f68ed establishes as source fidelity and this contract bars from deciding, and macos/audio-render's excess curve - which does count crossings independently of any target - postdates all forty committed envelopes"
for arm in t5-p1 t5-p2 t10-p1 t10-p2 t15-p1 t15-p2 t15-p3 t20-p1 t20-p2; do
    row "audio/jitter-target-a8/$arm.receiver.json" REFUSED "$AUDIO_WHY"
done
row "audio/jitter-target-a8/t20-p3.receiver.json" REFUSED \
    "$AUDIO_WHY. fbe503b records its 382 lost of 23997 at 1.59 per cent, the first real packet loss this project has measured, refused with the radio named rather than the buffer blamed - and that count is printed beside this refusal rather than becoming a verdict, because a verdict needs the tier that is missing and not the one that is present"
row "audio/jitter-target-a8/control.receiver.json" REFUSED \
    "$AUDIO_WHY. control.relay states the fault it injected: udp-fault holding every datagram for 400 ms every 2000 ms at loss 0.0 per cent, seed 20250815 - a periodic cadence fault with no loss, and the sharpest evidence in this corpus that the receiver of the day counted no arrival crossing"
row "audio/e2e-clean/clean-600s.receiver.json" REFUSED \
    "$AUDIO_WHY. 495adb1 and 229f2eb: zero of 120005 lost at -58 to -59 dBm with no render underrun in 112493 callbacks, the cleanest arm this project has recorded, and 3.82 per cent concealed by exactly the arrival tail. Delay rather than loss, and delay this envelope does not count"
row "audio/e2e-clean/clean-60s.receiver.json" REFUSED "$AUDIO_WHY"
row "audio/e2e-clean/broken-link.receiver.json" REFUSED \
    "$AUDIO_WHY. broken-link.relay states the injected fault: 400 ms held every 2000 ms at loss 0.0 per cent, the audio-e2e gate's own negative control, delay and not loss"
row "audio/e2e-corrected/clean-600s.receiver.json" REFUSED \
    "$AUDIO_WHY. 229f2eb calls this the contaminated run and 573840e uses its own first 120 s as the radio preflight's negative control, refusing at -4.305 dB/min as the link fell from -70 to -78 dBm. Zero of 120007 lost, so the radio tier has plenty to say and the middle tier still has nothing counted to read"
row "audio/e2e-corrected/clean-60s.receiver.json" REFUSED "$AUDIO_WHY"
row "audio/e2e-corrected/broken-link.receiver.json" REFUSED \
    "$AUDIO_WHY. broken-link.relay states the same injected 400 ms hold every 2000 ms at loss 0.0 per cent"

# ---------------------------------------------------------------------------
# The pass
# ---------------------------------------------------------------------------

cargo build --release -q -p lanplay-network-health \
    || refuse "the classifier did not build, so there is nothing here to hold against anything"
[ -x "$BIN" ] || refuse "$BIN is missing after a build that reported success"

echo "=== the committed corpus ==="
set +e
"$BIN" --results "$REPO/results" --expect "$TABLE"
PASS=$?
set -e
case "$PASS" in
    0) echo "PASS every recorded diagnosis agrees with its label" ;;
    1) echo "FAIL a label disagrees with a recorded diagnosis, listed above" ;;
    2) echo "the pass refused, and the line above says which session and why" >&2; exit 2 ;;
    *) refuse "the classifier exited $PASS, which is not an outcome it defines" ;;
esac

# ---------------------------------------------------------------------------
# Negative control: the radio tier absent
# ---------------------------------------------------------------------------
#
# Two copies of one arm in a corpus of their own, one with its trace beside it and one
# without. NetworkObservation.radio is an Option because CoreWLAN may decline, and
# nothing may depend on the answer; the two labels must be identical and the harness
# must not need a trace to reach one.

echo
echo "=== negative control: the radio tier absent ==="
NORADIO="$WORK/noradio"
mkdir -p "$NORADIO/b3-channel"
cp results/b3-channel/ch36-r1.json "$NORADIO/b3-channel/with-trace.json"
cp results/b3-channel/ch36-r1.wifi.csv "$NORADIO/b3-channel/with-trace.wifi.csv"
cp results/b3-channel/ch36-r1.json "$NORADIO/b3-channel/without-trace.json"

NORADIO_TABLE="$WORK/noradio.tsv"
{
    printf '%s\t%s\t%s\n' "b3-channel/with-trace.json" Healthy "ch36-r1 with its own wifi.csv beside it"
    printf '%s\t%s\t%s\n' "b3-channel/without-trace.json" Healthy "the same bytes with the trace removed"
} >"$NORADIO_TABLE"

set +e
NORADIO_OUT="$("$BIN" --as-found --results "$NORADIO" --expect "$NORADIO_TABLE" 2>&1)"
NORADIO_CODE=$?
set -e
echo "$NORADIO_OUT"
if [ "$NORADIO_CODE" -ne 0 ]; then
    refuse "the same arm with and without a radio trace did not classify the same way," \
        "so an absent radio tier stops the classifier and the Option is a lie"
fi
WITH="$(echo "$NORADIO_OUT" | awk '/with-trace.json/ {print $1}')"
WITHOUT="$(echo "$NORADIO_OUT" | awk '/without-trace.json/ {print $1}')"
[ -n "$WITH" ] && [ "$WITH" = "$WITHOUT" ] \
    || refuse "the two arms read as '$WITH' and '$WITHOUT', so the radio tier moved the verdict"
echo "$NORADIO_OUT" | grep -q "absent" \
    || refuse "neither arm reported an absent radio tier, so this control exercised nothing"
echo "FIRED both arms classified $WITH, one of them with no radio tier at all"

# ---------------------------------------------------------------------------
# Negative control: a session that counted nothing
# ---------------------------------------------------------------------------
#
# The same arm with its population set to zero. Every ratio in it becomes a zero over
# an absence, and a zero over an absence is not a healthy run. This must refuse, which
# is distinct from failing: nothing was measured, so nothing was decided.

echo
echo "=== negative control: a session with no observations ==="
EMPTY="$WORK/empty"
mkdir -p "$EMPTY/b3-channel"
sed 's/"expected": 14400,/"expected": 0,/' results/b3-channel/ch36-r1.json \
    >"$EMPTY/b3-channel/doctored.json"
grep -q '"expected": 0,' "$EMPTY/b3-channel/doctored.json" \
    || refuse "the doctored session still states a population, so this control was never armed"

EMPTY_TABLE="$WORK/empty.tsv"
printf '%s\t%s\t%s\n' "b3-channel/doctored.json" Healthy \
    "deliberately doctored to count nothing; a label here would be the bug" >"$EMPTY_TABLE"

set +e
EMPTY_OUT="$("$BIN" --as-found --results "$EMPTY" --expect "$EMPTY_TABLE" 2>&1)"
EMPTY_CODE=$?
set -e
echo "$EMPTY_OUT"
[ "$EMPTY_CODE" -eq 2 ] \
    || refuse "a session that counted nothing exited $EMPTY_CODE where 2 was owed;" \
        "an empty population was treated as a result"
echo "$EMPTY_OUT" | grep -q "REFUSE" \
    || refuse "the refusal did not say so, and a refusal nobody can read is a pass"
echo "FIRED an empty population refused rather than classifying"

# ---------------------------------------------------------------------------
# Negative control: a swapped verdict
# ---------------------------------------------------------------------------
#
# One row of the table above exchanged for another condition, everything else
# untouched. If the pass still agrees, it is announcing labels rather than checking
# them. ch36-r1 is chosen because it is a Healthy row on the mitigated channel and
# CadenceDegraded is the condition the whole channel result is about, so the exchange
# reaches a criterion rather than breaking a parse.

echo
echo "=== negative control: a recorded diagnosis swapped ==="
SWAPPED="$WORK/swapped.tsv"
sed 's|^b3-channel/ch36-r1.json\tHealthy\t|b3-channel/ch36-r1.json\tCadenceDegraded\t|' \
    "$TABLE" >"$SWAPPED"
if cmp -s "$TABLE" "$SWAPPED"; then
    refuse "the swap changed nothing, so this control was never armed"
fi

set +e
SWAPPED_OUT="$("$BIN" --results "$REPO/results" --expect "$SWAPPED" 2>&1)"
SWAPPED_CODE=$?
set -e
echo "$SWAPPED_OUT" | sed -n '/recorded diagnosis does not support/,$p'
[ "$SWAPPED_CODE" -eq 1 ] \
    || refuse "a swapped verdict exited $SWAPPED_CODE where 1 was owed;" \
        "the check is reading its own output rather than the corpus"
echo "$SWAPPED_OUT" | grep -q "b3-channel/ch36-r1.json" \
    || refuse "the failure did not name the session whose verdict was swapped"
echo "FIRED a swapped verdict made the pass fail, naming the session"

echo
if [ "$PASS" -eq 0 ]; then
    echo "PASS the corpus agrees and all three controls fired"
else
    echo "FAIL the controls fired and the corpus does not agree"
fi
exit "$PASS"
