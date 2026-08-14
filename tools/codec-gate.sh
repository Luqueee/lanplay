#!/usr/bin/env bash
# A2: what does Opus cost, and what does a short frame buy?
#
# Two questions, and only one of them has a right answer that a gate can enforce.
#
# The first is whether the encoder is irrelevant against the frame budget. That is
# a criterion: if encoding 5 ms of audio takes a meaningful fraction of 5 ms, the
# codec is in the latency path and everything downstream has to account for it. The
# plan asks for "much less than", and much less has to be a number or it is not a
# criterion at all, so it is a factor of ten here: an encoder at a tenth of the
# frame it is encoding cannot be the term that matters, and one above that has to
# be looked at.
#
# The second is what a 5 ms frame costs against a 10 ms one. That is not a
# criterion, it is a measurement the phase exists to produce, and the harness
# reports it rather than voting on it. Halving the packetisation delay costs
# bitrate, because Opus pays a fixed cost per packet, and the exchange rate is the
# number that decides the baseline. A first look gave 126 bytes for a 5 ms stereo
# frame against a 128 kbps target, which is 201 kbps effective, so the cost is not
# small and not something to assume.
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

mkdir -p "$OUT"
echo "results   $OUT"

cargo build --release -q -p lanplay-audio-codec

for frame_ms in 5 10; do
    "$REPO/target/release/audio-codec-probe" \
        --frame-ms "$frame_ms" --seconds "$SECONDS_TO_RUN" --bitrate-kbps 128 \
        >"$OUT/$frame_ms.out" 2>&1 || true
    echo "arm       ${frame_ms} ms done"
done

python3 - "$OUT" <<'PY'
import re
import sys

out = sys.argv[1]

# Ten, because "much less than the frame" has to be a number to be a criterion, and
# a tenth is the point past which the encoder stops being able to be the term that
# matters in a budget the rest of this project measures in whole milliseconds.
BUDGET_FRACTION = 10.0
LEFT_HZ, RIGHT_HZ = 997.0, 1997.0
TONE_TOLERANCE_HZ = 5.0


def arm(frame_ms):
    try:
        body = open(f"{out}/{frame_ms}.out").read()
    except OSError:
        return None

    def field(pattern):
        got = re.search(pattern, body, re.M)
        return got.group(1) if got else None

    encode = re.search(r"^encode us p50 (\d+) p95 (\d+) p99 (\d+) max (\d+)$", body, re.M)
    decode = re.search(r"^decode us p50 (\d+) p95 (\d+) p99 (\d+) max (\d+)$", body, re.M)
    packet = re.search(r"^packet bytes p50 (\d+) p95 (\d+) p99 (\d+) max (\d+)$", body, re.M)
    tone = re.search(r"^tone left ([\d.]+) right ([\d.]+)$", body, re.M)
    if not (encode and decode and packet):
        return None
    return {
        "frame_ms": frame_ms,
        "submitted": field(r"^frames submitted (\d+)$"),
        "returned": field(r"^frames returned (\d+)$"),
        "packets": field(r"^packets (\d+)$"),
        "encode": [int(g) for g in encode.groups()],
        "decode": [int(g) for g in decode.groups()],
        "bytes": [int(g) for g in packet.groups()],
        "kbps": field(r"^effective kbps ([\d.]+)$"),
        "tone": (float(tone.group(1)), float(tone.group(2))) if tone else None,
        "distinct": field(r"^tone channels distinct (yes|no)$"),
    }


arms = [a for a in (arm(5), arm(10)) if a]

print("\nwhat the codec costs\n")
print(f"  {'frame':<8} {'encode p50':>11} {'p99':>8} {'decode p50':>11} {'p99':>8} {'bytes p50':>10} {'p99':>7} {'kbps':>8}")
for a in arms:
    print(
        f"  {a['frame_ms']:>5} ms {a['encode'][0]:>9} us {a['encode'][2]:>6} us"
        f" {a['decode'][0]:>9} us {a['decode'][2]:>6} us"
        f" {a['bytes'][0]:>10} {a['bytes'][2]:>7} {float(a['kbps']):>8.1f}"
    )

print("\nwhat the samples say\n")
for a in arms:
    tone = f"{a['tone'][0]:.1f} / {a['tone'][1]:.1f} Hz" if a["tone"] else "not measured"
    print(
        f"  {a['frame_ms']:>5} ms  submitted {a['submitted']}  returned {a['returned']}"
        f"  packets {a['packets']}  tone {tone}  distinct {a['distinct']}"
    )

failures = []
if len(arms) < 2:
    failures.append("an arm produced no figures, so the comparison the phase exists for is missing")

for a in arms:
    budget_us = a["frame_ms"] * 1000.0 / BUDGET_FRACTION
    if a["packets"] is None or int(a["packets"]) == 0:
        failures.append(f"{a['frame_ms']} ms: no packet was produced, so every figure is an absence")
        continue
    if a["submitted"] != a["returned"]:
        failures.append(
            f"{a['frame_ms']} ms: {a['submitted']} frames in against {a['returned']} out - "
            "Opus is lossy in amplitude and exact in length, so a length that disagrees is a defect"
        )
    if a["encode"][2] > budget_us:
        failures.append(
            f"{a['frame_ms']} ms: encode p99 {a['encode'][2]} us against a {budget_us:.0f} us "
            f"budget, a tenth of the frame - the codec is in the latency path"
        )
    if a["tone"] is None:
        failures.append(f"{a['frame_ms']} ms: the tone was not measured, so nothing proves the codec carried audio")
    else:
        left, right = a["tone"]
        if abs(left - LEFT_HZ) > TONE_TOLERANCE_HZ or abs(right - RIGHT_HZ) > TONE_TOLERANCE_HZ:
            failures.append(
                f"{a['frame_ms']} ms: the decoded tone reads {left:.1f} / {right:.1f} Hz against "
                f"{LEFT_HZ:.0f} / {RIGHT_HZ:.0f}"
            )
    if a["distinct"] != "yes":
        failures.append(f"{a['frame_ms']} ms: the decoded channels are not distinct")

# The measurement the phase produces, stated rather than voted on. Reported even
# when something failed, because the exchange rate is the deliverable and a failing
# encoder does not make it uninteresting.
if len(arms) == 2:
    short, long = arms[0], arms[1]
    premium = float(short["kbps"]) / float(long["kbps"]) - 1.0
    saved_ms = long["frame_ms"] - short["frame_ms"]
    print(
        f"\n  FINDING a {short['frame_ms']} ms frame costs {premium * 100:+.1f} % bitrate against "
        f"{long['frame_ms']} ms\n          and buys {saved_ms:.0f} ms of packetisation delay: "
        f"{float(short['kbps']):.1f} against {float(long['kbps']):.1f} kbps"
    )

print()
if failures:
    for failure in failures:
        print(f"FAIL {failure}")
    sys.exit(1)
print(
    "PASS both frame durations round-trip the tone with the sample count exact, and the\n"
    "     encoder stays under a tenth of the frame it encodes"
)
PY
