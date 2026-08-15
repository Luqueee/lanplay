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
# Everything here runs on this machine. A2 is isolated by design, the tone
# generator is arithmetic, and libopus builds here; nothing needs the lab host.
#
# usage:
#   tools/codec-gate.sh [seconds]

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

status=0
for frame_ms in 5 10; do
    echo
    if [[ ! -s "$OUT/$frame_ms.json" ]]; then
        echo "FAIL the ${frame_ms} ms arm emitted no envelope, so the comparison the phase exists"
        echo "     for is missing; what it printed is in $OUT/$frame_ms.out"
        status=1
        continue
    fi
    "$XTASK" verdict "$OUT/$frame_ms.json" || status=1
done

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
if [[ "$status" -ne 0 ]]; then
    echo "FAIL an arm did not hold what it stated, and the block above it says which and why"
    exit 1
fi
echo "PASS both frame durations round-trip the tone with the sample count exact, and the"
echo "     encoder stays under a tenth of the frame it encodes"
