#!/usr/bin/env bash
# A5: can a ring buffer feed CoreAudio for five minutes without a gap?
#
# The render callback is the hardest real-time deadline in this project. A dropped video
# frame is replaced by the next one and a listener never learns of it; a callback that
# arrives unfilled is a click, and there is no version of a click that goes unnoticed.
# So the criterion is zero, not small.
#
# Five minutes because that is what shows a slow drift in occupancy. Twenty seconds
# cannot: the first arrangement of this producer underran five times in twenty seconds,
# and the arrangement that fixed it still underran ten times in three hundred - which a
# twenty second run would have called clean two times out of three.
#
# The tone plays at -40 dBFS. The measurement is entirely digital, so amplitude buys it
# nothing, and the machine may be sitting next to somebody.
#
# usage:
#   tools/render-gate.sh [seconds]

set -euo pipefail

SECONDS_TO_RUN="${1:-300}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/render-gate/$(date +%Y%m%d-%H%M%S)}"

mkdir -p "$OUT"
echo "results   $OUT"

cargo build --release -q -p lanplay-audio-render
"$REPO/target/release/audio-render-probe" --seconds "$SECONDS_TO_RUN" \
    >"$OUT/render.out" 2>&1 || true
echo "run       done"
echo
cat "$OUT/render.out"

python3 - "$OUT" "$SECONDS_TO_RUN" <<'PY'
import re
import sys

out, seconds = sys.argv[1], float(sys.argv[2])
body = open(f"{out}/render.out").read()


def num(pattern):
    got = re.search(pattern, body, re.M)
    return int(got.group(1)) if got else None


def real(pattern):
    got = re.search(pattern, body, re.M)
    return float(got.group(1)) if got else None


callbacks = num(r"^callbacks (\d+)$")
underruns = num(r"^underruns (\d+)$")
underrun_frames = num(r"^underrun frames (\d+)$")
overruns = num(r"^overruns (\d+)$")
missing = num(r"^frames missing (\d+)$")
residual = num(r"^frames still in the ring (\d+)$")
ring = num(r"^ring frames (\d+) being")
rate = real(r"^measured frames per second ([\d.]+)$")
declared = num(r"^output format (\d+) Hz")
occupancy = re.search(
    r"^ring occupancy frames p50 (\d+) p95 (\d+) p99 (\d+) max (\d+)$", body, re.M
)
interval = re.search(
    r"^callback interval us p50 (\d+) p95 (\d+) p99 (\d+) max (\d+)$", body, re.M
)
contract = re.search(r"^format matches contract (yes|no)$", body, re.M)

failures = []

# A run measured under a policy nobody granted is a measurement of something else. The
# quality-of-service band this replaced was granted every time and still underran twelve
# times in one run of three hundred seconds, so the presence of the deadline is not a
# detail to assume - it is the thing that changed.
policy = re.search(r"^audio-render: producer scheduled as (.+)$", body, re.M)
if policy is None:
    failures.append("the probe did not say how its producer was scheduled")
elif "time constraint" not in policy.group(1):
    failures.append(
        f"the producer ran as {policy.group(1)}: without a deadline the underrun count "
        "below describes the scheduler rather than this design"
    )

# Nothing may pass by having produced no evidence: zero underruns over zero callbacks
# is the shape of clean this project has been burnt by repeatedly.
expected = seconds * 48000.0 / 256.0
if not callbacks:
    failures.append("the callback never fired, so every zero here is an absence")
elif callbacks < expected * 0.9:
    failures.append(
        f"{callbacks} callbacks against about {expected:.0f} expected: the stream stopped "
        "part way and the distributions describe a prefix"
    )

if underruns is None or underruns > 0:
    failures.append(
        f"{underruns} callbacks went unfilled, {underrun_frames} frames of silence - the "
        "criterion is zero because a click is not a dropped frame"
    )
if overruns is None or overruns > 0:
    failures.append(f"{overruns} overruns: the producer had nowhere to write")
if missing is None or missing > 0:
    failures.append(f"{missing} frames went missing, which is not the ring being full")
if residual is not None and ring is not None and residual > ring:
    failures.append(f"the ring holds {residual} frames of a {ring} frame ring")
if contract is None or contract.group(1) != "yes":
    failures.append("the device is not the format the contract needs, so nothing downstream applies")

# A steady callback is what makes the occupancy figure mean anything: a cadence that
# wandered would explain a dip without the producer being late at all.
if interval:
    p50, _, p99, largest = (int(g) for g in interval.groups())
    if largest > p50 * 2:
        failures.append(
            f"the callback interval reached {largest} us against a {p50} us median, so the "
            "device was not keeping its own cadence and the occupancy says nothing"
        )

print("\nhow the producer was scheduled\n")
print(f"  {policy.group(1) if policy else 'not reported'}")

print("\nwhat the device is\n")
print(f"  declared rate     {declared} Hz")
print(f"  measured rate     {rate} Hz  ({(rate / declared - 1) * 1e6:+.0f} ppm)" if rate and declared else "")
if interval:
    print(f"  callback interval p50 {interval.group(1)} us  max {interval.group(4)} us")
if occupancy:
    print(f"  ring occupancy    p50 {occupancy.group(1)}  min-to-max {occupancy.group(1)}..{occupancy.group(4)} of {ring} frames")

# The number A7 will need, stated here because this is the only place it is measured on
# this machine and it is worth having before the phase that depends on it.
if rate and declared:
    ppm = (rate / declared - 1) * 1e6
    print(
        f"\n  FINDING this output clock runs {ppm:+.0f} ppm against its own nominal rate. The\n"
        f"          host's capture clock measured -15 ppm, so a stream between them drifts by\n"
        f"          their difference: about {abs(ppm - -15) * 600 / 1000:.0f} ms over ten minutes, which is more than a\n"
        "          10 ms jitter buffer holds. A7 is not hypothetical."
    )

print()
if failures:
    for failure in failures:
        print(f"FAIL {failure}")
    sys.exit(1)
print(
    f"PASS {callbacks} callbacks over {seconds:.0f} s with no gap: nothing unfilled, nothing\n"
    "     overwritten, nothing missing, and the cadence held"
)
PY
