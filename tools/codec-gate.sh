#!/usr/bin/env bash
# A2: what does Opus cost, and what does a short frame buy?
#
# Two questions, and only one of them has a right answer that a gate can enforce.
#
# The first is whether the encoder is irrelevant against the frame budget. That is
# a criterion: if encoding 5 ms of audio takes a meaningful fraction of 5 ms, the
# codec is in the latency path and everything downstream has to account for it. The
# plan asks for "much less than", and much less has to be a number or it is not a
# criterion at all, so it is a factor of ten: an encoder at a tenth of the frame it
# is encoding cannot be the term that matters, and one above that has to be looked
# at. The number now lives in the envelope the probe emits, where it is stated next
# to the sentence that derives it.
#
# The second is what a 5 ms frame costs against a 10 ms one. That is not a
# criterion, it is a measurement the phase exists to produce, and the harness
# reports it rather than voting on it. Halving the packetisation delay costs
# bitrate, because Opus pays a fixed cost per packet, and the exchange rate is the
# number that decides the baseline. A first look gave 126 bytes for a 5 ms stereo
# frame against a 128 kbps target, which is 201 kbps effective, so the cost is not
# small and not something to assume.
#
# Nothing here parses the probe's prose. Each arm writes one JSON envelope and
# `xtask verdict` decides it, which is the arrangement `docs/testing.md` argues for
# and this is the first gate to use it: the regular expressions this script used to
# carry were the same ones that read 6001 captured packets as none in a sibling
# harness. The two numbers the cross-arm finding needs come back out of the
# envelopes through the same parser, so a renamed observation stops the gate rather
# than printing an empty string.
#
# There is a third arm and it must fail. The two above state criteria about the
# audio that came back, and a criterion nobody has seen disagree is a criterion
# nobody has any reason to trust; the control arm sends the contract tone in with
# its two channels exchanged, and this gate fails if the evaluator passes it. What
# it is aimed at, what it was chosen over and what its numbers have to look like
# are argued where it runs.
#
# Everything here runs on this machine. A2 is isolated by design, the tone
# generator is arithmetic, and libopus builds here; nothing needs the lab host.
#
# usage:
#   tools/codec-gate.sh [seconds]
#
# exit 0  both measuring arms held what they stated, and the control did not
# exit 1  a measuring arm did not hold, or the control did, and the block above
#         its verdict names the criterion and the numbers it was decided on
# exit 2  refused: an arm stated a criterion nobody could decide, named in the same
#         block, so nothing here says whether the encoder holds against the frame
#         budget either way - and a control nobody could decide has not been shown
#         to fail, which is a different thing from having failed

set -euo pipefail

SECONDS_TO_RUN="${1:-30}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/codec-gate/$(date +%Y%m%d-%H%M%S)}"
PROBE="$REPO/target/release/audio-codec-probe"
XTASK="$REPO/target/release/xtask"

mkdir -p "$OUT"
echo "results   $OUT"

cargo build --release -q -p lanplay-audio-codec -p xtask

# Every arm carries the same arguments, and the commit only when git can say what
# it is: a probe told a hash nobody read out of a repository would record a
# provenance that is worse than an absent one.
ARM_ARGS=(--seconds "$SECONDS_TO_RUN" --bitrate-kbps 128)
if COMMIT="$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null)"; then
    ARM_ARGS+=(--commit "$COMMIT")
fi

for frame_ms in 5 10; do
    # The keyed report still goes to a file, because it is what a person reads
    # when a gate fails, and the envelope beside it is what decides.
    "$PROBE" --frame-ms "$frame_ms" --envelope "$OUT/$frame_ms.json" "${ARM_ARGS[@]}" \
        >"$OUT/$frame_ms.out" 2>&1 || true
    echo "arm       ${frame_ms} ms done"
done

# The negative control. Same probe, same criteria, same evaluator: the only
# difference from the 5 ms arm above is that the contract tone goes into the
# encoder with its two channels exchanged, and this gate fails if `xtask verdict`
# passes the document that comes out. Judging it any other way would be a second
# mechanism beside the one under test, and then a green control would be evidence
# about the second mechanism.
#
# It is aimed at the pair of criteria naming a frequency per channel, which is
# this gate's strongest claim: the two tones sit a thousand hertz apart so that
# channel order is provable rather than assumed, and nothing had ever shown that
# claim capable of disagreeing. Three other candidates were weighed and dropped. A
# frame duration Opus does not permit never reaches a criterion at all, because
# the probe refuses its own argument before a document exists - which is the
# near-miss the index recorded as a debt rather than a control. A truncated packet
# makes libopus return an error and the arm then emits nothing, which is what a
# broken harness looks like rather than a criterion firing. A decoder created for
# one channel against a stereo encoder does fire the sample-count criterion, but
# it also hands the detector mono audio read as stereo, and 997 Hz decimated that
# way aliases to 1994 Hz - three hertz from the right channel's contract tone and
# inside the five hertz tolerance - so the arm would report the right channel
# holding, for a reason nobody could defend. The fourth, an encoder held to a
# bandwidth too narrow for 1997 Hz, does not exist to be built: the narrowest Opus
# offers is narrowband at 4 kHz and it carries both tones comfortably.
#
# So the exchange is the smallest perturbation that reaches the criteria. Same
# spectrum, same level, same frame duration, same bitrate target, and only the
# channel assignment moved.
"$PROBE" --frame-ms 5 --swap-tone-channels --envelope "$OUT/control.json" "${ARM_ARGS[@]}" \
    >"$OUT/control.out" 2>&1 || true
echo "arm       control done (5 ms, the two contract tones exchanged)"

status=0
refused=0
for frame_ms in 5 10; do
    echo
    if [[ ! -s "$OUT/$frame_ms.json" ]]; then
        echo "FAIL the ${frame_ms} ms arm emitted no envelope, so the comparison the phase exists"
        echo "     for is missing; what it printed is in $OUT/$frame_ms.out"
        status=1
        continue
    fi
    # Two and one are different answers and are kept apart here. `xtask verdict`
    # refuses a document whose criterion had no number to read, and an arm nobody
    # could decide is not an arm that disagreed: reading the refusal as a failure
    # would put this gate back to claiming it had tested something it had not.
    code=0
    "$XTASK" verdict "$OUT/$frame_ms.json" || code=$?
    if [[ "$code" -ge 2 ]]; then
        refused=1
    elif [[ "$code" -ne 0 ]]; then
        status=1
    fi
done

echo
echo "control   the arm below must fail, and this gate fails if it does not"
echo

# The same parser every other number here comes through, and the word `absent`
# rather than an empty string when a document reports no such observation: an
# empty string compared against a target is how a harness reads a number that is
# not there as a zero and calls it a match.
observation() {
    "$XTASK" verdict --observation "$1" "$2" 2>/dev/null || echo absent
}

if [[ ! -s "$OUT/control.json" ]]; then
    # Not an early exit: the exchange rate below is the deliverable of the two
    # measuring arms and a control that never wrote anything does not make it
    # less interesting.
    echo "FAIL the control emitted no envelope, so the criteria it exists to exercise are as"
    echo "     unproven as they were before it was written; what it printed is in"
    echo "     $OUT/control.out"
    status=1
else
    code=0
    "$XTASK" verdict "$OUT/control.json" || code=$?

    if [[ "$code" -eq 0 ]]; then
        echo
        echo "FAIL the control passed. The two criteria naming a frequency per channel cannot"
        echo "     tell 997 Hz from 1997 Hz where it matters, so every arm that held them held"
        echo "     nothing, and the exchange rate below is all this run produced"
        status=1
    elif [[ "$code" -ge 2 ]]; then
        echo
        echo "REFUSE the control could not be decided, so it has not been shown to fail and the"
        echo "       criteria it is aimed at stand exactly where a gate with no control at all"
        echo "       leaves them"
        refused=1
    else
        # A control that failed is not yet a control that worked. A3 wired one an
        # hour before this and it fired on losing 2000 datagrams of 2000 to a relay
        # pointed the wrong way, certifying criteria it never reached. So the
        # numbers come back through the same evaluator and are held against what an
        # exchange, and only an exchange, produces: the 5 ms arm's own packet count,
        # no frame count disagreement at all, and the two contract frequencies the
        # other way round.
        awk -v packets="$(observation packets "$OUT/control.json")" \
            -v expected="$(observation packets "$OUT/5.json")" \
            -v disagreement="$(observation frame_count_disagreement "$OUT/control.json")" \
            -v left="$(observation tone_left_hz "$OUT/control.json")" \
            -v right="$(observation tone_right_hz "$OUT/control.json")" '
        function numeric(text) { return text ~ /^-?[0-9]+([.][0-9]+)?$/ }
        function away(value, target) { return value > target ? value - target : target - value }
        BEGIN {
            held = 1
            if (!numeric(packets) || !numeric(expected) || packets != expected) {
                printf "  the control encoded %s packets where the 5 ms arm encoded %s, so it did not\n", packets, expected
                printf "  run the work the criteria were stated over\n"
                held = 0
            }
            if (!numeric(disagreement) || disagreement != 0) {
                printf "  the control mislaid %s frames between encoder and decoder, which is a broken\n", disagreement
                printf "  path and not an exchanged pair of tones\n"
                held = 0
            }
            if (!numeric(left) || away(left, 1997) > 5) {
                printf "  the left channel read %s Hz where 1997 Hz, the right channel tone, went in\n", left
                held = 0
            }
            if (!numeric(right) || away(right, 997) > 5) {
                printf "  the right channel read %s Hz where 997 Hz, the left channel tone, went in\n", right
                held = 0
            }
            if (held) {
                printf "  CONTROL the two criteria naming a frequency per channel disagreed, and the\n"
                printf "          four beside them held: %s packets, no frame count disagreement,\n", packets
                printf "          %.1f Hz on the left and %.1f Hz on the right\n", left, right
                exit 0
            }
            printf "\nFAIL the control failed, but not on what it was aimed at, so the two frequency\n"
            printf "     criteria are as unproven as before and the numbers above are the harness\n"
            exit 1
        }' || status=1
    fi
fi

# The measurement the phase produces, stated rather than voted on, and printed
# even when an arm failed a criterion: the exchange rate is the deliverable and a
# slow encoder does not make it uninteresting.
if [[ -s "$OUT/5.json" && -s "$OUT/10.json" ]]; then
    short_kbps="$("$XTASK" verdict --observation effective_kbps "$OUT/5.json")"
    long_kbps="$("$XTASK" verdict --observation effective_kbps "$OUT/10.json")"
    awk -v short="$short_kbps" -v long="$long_kbps" 'BEGIN {
        printf "\n  FINDING a 5 ms frame costs %+.1f %% bitrate against 10 ms\n", (short / long - 1) * 100
        printf "          and buys 5 ms of packetisation delay: %.1f against %.1f kbps\n", short, long
    }'
fi

echo
# The failure first when a run produced both, because an arm that was decided and
# disagreed says more about Opus than one that could not be decided at all.
if [[ "$status" -ne 0 ]]; then
    echo "FAIL an arm did not hold what it stated, and the block above it says which and why"
    exit 1
fi
if [[ "$refused" -ne 0 ]]; then
    echo "REFUSE an arm stated a criterion with nothing to decide it on, named above, so this"
    echo "       run says neither that the encoder holds against the frame budget nor that it"
    echo "       does not"
    exit 2
fi
echo "PASS both frame durations round-trip the tone with the sample count exact, the"
echo "     encoder stays under a tenth of the frame it encodes, and the control arm was"
echo "     refused by the two criteria that make channel order provable"
