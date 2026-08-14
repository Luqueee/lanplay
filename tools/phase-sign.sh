#!/usr/bin/env bash
# Which way does a delay move the phase?
#
# Derived twice from first principles, and the machine disagreed both times. A
# source delayed by d makes its frame ready d later, so the gap to the display's
# next opportunity should shrink by d and the wait with it. What a live run
# produced instead was a loop alternating between d and one period minus d,
# which is what a controller does when its plant responds with the opposite sign.
#
# Two derivations against one measurement is a bad trade, so this stops deriving.
# The client observes without acting, one known delay is applied to the producer
# by hand halfway through, and the phase before is compared with the phase after.
# Nothing here needs a theory to be read.
#
# The delay is deliberately a small fraction of a period. Large enough to be far
# outside the measured scatter, small enough that it cannot wrap, because a shift
# that wraps makes both signs fit the result and that is the one outcome this
# cannot afford.
#
# usage:
#   tools/phase-sign.sh [seconds] [delay-ms]

set -euo pipefail

SECONDS_TO_RUN="${1:-70}"
DELAY_MS="${2:-3}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/phase-sign}"

mkdir -p "$OUT"
rm -f "$OUT"/*.log "$OUT"/*.json

# The producer takes its requests as an 8-byte loopback datagram: the four bytes
# LPPH and a little-endian u32 of nanoseconds. Sent from the host itself, because
# the port is bound to loopback and nothing off the machine can reach it.
delay_ns=$(python3 -c "print(int($DELAY_MS * 1_000_000))")
cat >"$OUT/shift.ps1" <<PS1
\$bytes = [byte[]]@(0x4C, 0x50, 0x50, 0x48) + [System.BitConverter]::GetBytes([uint32]$delay_ns)
\$client = New-Object System.Net.Sockets.UdpClient
\$client.Send(\$bytes, \$bytes.Length, '127.0.0.1', 5010) | Out-Null
\$client.Close()
Write-Output "sent $DELAY_MS ms as $delay_ns ns"
PS1
scp -q "$OUT/shift.ps1" 'windows:C:/Users/luque/phase-shift-once.ps1'

# Halfway, so each half has the same number of samples and the same share of
# whatever the link was doing.
(
    sleep $((SECONDS_TO_RUN / 2 + 12))
    "$REPO/tools/win-ssh.sh" \
        'powershell -NoProfile -ExecutionPolicy Bypass -File C:\Users\luque\phase-shift-once.ps1' \
        >"$OUT/shift.log" 2>&1 || true
    echo "shift     applied at the halfway mark"
) &
shifter=$!

IFACE=en0 BITRATE=40 MTU=1200 PHASE_ALIGN=observe REPORT="$OUT/observe.json" \
    "$REPO/tools/e2e-gate.sh" "$SECONDS_TO_RUN" >"$OUT/observe.log" 2>&1 || true
wait "$shifter" 2>/dev/null || true

echo
cat "$OUT/shift.log" 2>/dev/null || echo "the shift was never sent"
grep -E "presentation wait|phase alignment|fresh ticks" "$OUT/observe.log" | sed 's/^/  /'
echo
"$REPO/tools/win-ssh.sh" \
    'powershell -NoProfile -Command "Get-Content C:\Users\luque\idd-present.stderr.log -Tail 2"' \
    2>/dev/null | tr -d '\r' | sed 's/^/  /'

python3 - "$OUT" "$DELAY_MS" "$SECONDS_TO_RUN" <<'PY'
import json
import re
import sys

out, delay = sys.argv[1], float(sys.argv[2])
report = json.load(open(f"{out}/observe.json"))
phase = report.get("phase") or {}
period = 1000.0 / 120.0

# Comparing the ends of a run cannot answer this. The two clocks beat, and that
# drift moves the phase about 250 us every second, so it sweeps a whole period
# every 33 s - further than the shift does over any run long enough to gather
# samples. The first version of this script did compare the first phase against
# the last and announced a sign from a movement the drift could account for twice
# over.
#
# The step is found in the series instead of being located by time. Lining up an
# instant taken in a shell with one taken on the client's monotonic clock would
# need an offset between two clocks, which is exactly what this project refuses to
# invent everywhere else. And it is unnecessary: the drift moves the phase about
# an eighth of a millisecond between decisions, so a three millisecond step is
# more than twenty times the largest innocent movement and stands out on its own.
DRIFT_MS_PER_S = 0.25
DECISION_S = 0.5

# A starved display link produces a refusal that reads like a phase problem. One
# run here fired 887 callbacks in 70 s against the 119.97 Hz it reported as
# nominal, so the estimator saw 43 of the 48 samples a decision needs and declined
# every one of them - correctly, and for a reason nothing in the output tied to
# the display. Named here so the next one cannot be mistaken for anything else.
refreshes = re.search(r"fresh ticks\s+[\d.]+% \((\d+) of (\d+) refreshes", open(f"{out}/observe.log").read())
display_hz = re.search(r"display\s+([\d.]+) Hz", open(f"{out}/observe.log").read())
if refreshes and display_hz:
    callbacks = int(refreshes.group(2))
    expected = float(display_hz.group(1)) * float(sys.argv[3])
    if callbacks < expected * 0.9:
        print(
            f"\nFAIL the display link fired {callbacks} times against {expected:.0f} expected at "
            f"{display_hz.group(1)} Hz: it was suspended for most of the run, so nothing measured\n"
            "     against it means anything"
        )
        sys.exit(1)

trace = phase.get("trace") or []
print(f"\n  a delay of {delay:.2f} ms was applied once, halfway through")
print(f"  phase samples in the trace: {len(trace)}")

if phase.get("shifts") or any(entry.get("sent") for entry in trace):
    print("\nFAIL the estimator sent shifts of its own, so the movement has two causes")
    sys.exit(1)
dropped = phase.get("trace_dropped") or 0
if dropped:
    print(f"\nFAIL {dropped} trace entries were dropped, so a gap could hide the step")
    sys.exit(1)
if len(trace) < 8:
    print(
        "\nFAIL the report carries no usable phase trace, and the ends of a run cannot\n"
        f"     answer this: the drift sweeps a period every {period / DRIFT_MS_PER_S / 1000:.0f} s"
    )
    sys.exit(1)


def short_way(difference):
    """A phase is an angle: 7.4 ms to 1.0 ms is forward, not most of a period back."""
    return (difference + period / 2.0) % period - period / 2.0


phases = [entry["phase_ms"] for entry in trace]
steps = [short_way(b - a) for a, b in zip(phases, phases[1:])]
innocent = DRIFT_MS_PER_S * DECISION_S
candidates = [(abs(s), i, s) for i, s in enumerate(steps) if abs(s) > delay / 2.0]

print(f"  the drift can move it {innocent:.3f} ms between decisions")
print(f"  movements larger than half the delay: {len(candidates)}")

if not candidates:
    print(
        f"\nFAIL nothing in the series moved by more than {delay / 2.0:.2f} ms, so the shift\n"
        "     the producer says it applied did not reach the phase at all"
    )
    sys.exit(1)
if len(candidates) > 1:
    # More than one step means something other than the one applied delay moved
    # the phase, and no single one of them can be attributed to it.
    print(f"     at indices {[i for _, i, _ in candidates]}, sizes {[round(s, 2) for _, _, s in candidates]}")
    print("\nFAIL more than one movement is large enough to be the shift")
    sys.exit(1)

_, index, moved = candidates[0]
print(f"  one step, {moved:+.2f} ms, between decisions {index} and {index + 1}")
print(f"  against a delay of {delay:+.2f} ms, {abs(moved) / innocent:.0f}x the innocent movement")
print(
    "\nA DELAY REDUCES THE PHASE, so the loop's assumption is right and the"
    "\noscillation has another cause."
    if moved < 0
    else "\nA DELAY INCREASES THE PHASE, so the loop's correction has to be negated."
)
PY
