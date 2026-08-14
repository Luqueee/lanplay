#!/usr/bin/env bash
# Does phase alignment remove the wait, or did the run get lucky?
#
# The largest term in this pipeline's latency is the wait between a frame being
# ready and the viewer's display next being willing to show it. With two
# unsynchronised 120 Hz clocks that wait averages half a refresh period, and a
# 600 s soak measured percentiles indistinguishable from the prediction for a
# uniformly distributed phase. So there is a number to beat and a reason to
# expect it, which is the rare case where a threshold means something.
#
# It still cannot be the whole criterion. A single arm under a threshold cannot
# tell an alignment loop that worked from a run whose phase happened to be
# favourable, and the two look identical in the report. So this runs both arms
# and requires them to disagree in the direction the mechanism predicts: the
# unaligned arm at half a period, the aligned arm well under it. An alignment
# that changes nothing fails here, and so does a gate that would have passed
# without the mechanism.
#
# The rate is checked as carefully as the latency. A phase shift that drops a
# frame buys 4 ms at a cost nobody asked for, and a phase shift that becomes a
# rate change buys it by falling behind, so both arms must deliver the same
# number of frames per second.
#
# usage:
#   tools/phase-gate.sh [seconds]

set -euo pipefail

# Two sweeps of the phase, and not a second less. The two clocks differ by about
# five parts per million, which was measured rather than taken from the nominal
# rates: a 25 s trace drifted 0.97 ms, so the phase crosses a period roughly every
# 210 s. An arm shorter than that does not sample the phase, it samples wherever
# the phase happened to be, and comparing two such arms compares two independent
# draws. That mistake was made here first: a 40 s pair showed 5.51 ms against
# 2.38 ms and the aligned arm had simply started at 1.89 ms, already on target,
# holding 25 times out of 45.
SWEEP_S=210
SECONDS_TO_RUN="${1:-$((SWEEP_S * 2))}"
if [ "$SECONDS_TO_RUN" -lt "$((SWEEP_S * 2))" ]; then
    echo "an arm of ${SECONDS_TO_RUN} s cannot sample the phase: it sweeps a period every ${SWEEP_S} s," >&2
    echo "so anything shorter than $((SWEEP_S * 2)) s compares two arbitrary starting phases" >&2
    exit 64
fi
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/phase-gate}"

mkdir -p "$OUT"
rm -f "$OUT"/*.log "$OUT"/*.json


# A daemon left over from before this mechanism existed ignores every request,
# and the aligned arm would then be identical to the control: the conclusion
# would be that alignment does nothing. So the build has to be checked.
#
# Checked rather than restarted, because restarting it is not something this
# harness can do. `ensure-lab-source.ps1` starts the producer from inside a
# scheduled task and verifies it one second later, which passes; the task then
# ends and takes its child with it. The lab has been running on a producer
# started outside a task and left alone for days, which is why that has never
# shown. Killing it here and trusting the preflight to bring it back produced
# "capture produced no frame for one second" and one access unit out of 4800.
#
# Refusing loudly is worth more than either silently measuring nothing or
# quietly measuring the wrong binary.
"$REPO/tools/win-sync.sh" >/dev/null
stale="$("$REPO/tools/win-ssh.sh" 'powershell -NoProfile -Command "$p = Get-Process present-source -ErrorAction SilentlyContinue; $b = Get-Item C:\Users\luque\lanplay-rs\target\release\present-source.exe -ErrorAction SilentlyContinue; if (-not $p) { \"absent\" } elseif (-not $b) { \"nobinary\" } elseif ($p.StartTime -lt $b.LastWriteTime) { \"stale\" } else { \"current\" }"' 2>/dev/null | tr -d '\r ')"
case "$stale" in
current) echo "source    producer is the current build" ;;
*)
    echo "source    the producer is $stale, so a phase request would reach nothing" >&2
    echo "          start it by hand on the host, outside a scheduled task:" >&2
    echo "          target\\release\\present-source.exe --width 1920 --height 1080 \\" >&2
    echo "              --fps 120 --seconds 0 --fullscreen --monitor 1" >&2
    exit 1
    ;;
esac
for arm in off on; do
    echo "=== phase alignment $arm ==="
    # Both arms are otherwise identical, including the interface and the
    # bitrate, because the comparison is only about the phase.
    IFACE=en0 BITRATE=40 MTU=1200 PHASE_ALIGN="$arm" \
        REPORT="$OUT/$arm.json" "$REPO/tools/e2e-gate.sh" "$SECONDS_TO_RUN" \
        >"$OUT/$arm.log" 2>&1 || true
    grep -E "presentation wait|fresh ticks|rendered |superseded |phase" "$OUT/$arm.log" |
        sed 's/^/  /' || true
done

python3 - "$OUT" <<'PY'
import json
import re
import sys

out = sys.argv[1]

# One period at 120 Hz, and the wait an unaligned stream is predicted to average
# because its phase is uniformly distributed inside the period.
PERIOD_MS = 1000.0 / 120.0
HALF = PERIOD_MS / 2.0


def arm(name):
    """One arm's figures, read from its own report rather than from a threshold."""
    try:
        text = open(f"{out}/{name}.log").read()
        report = json.load(open(f"{out}/{name}.json"))
    except (OSError, ValueError):
        return None
    wait = re.search(r"presentation wait p50\s+([\d.]+) ms.*?p99\s+([\d.]+) ms", text)
    rendered = re.search(r"rendered\s+(\d+)", text)
    fresh = re.search(r"fresh ticks\s+([\d.]+)%", text)
    if not wait or not rendered:
        return None
    phase = report.get("phase") or {}
    return {
        "p50": float(wait.group(1)),
        "p99": float(wait.group(2)),
        "rendered": int(rendered.group(1)),
        "fresh": float(fresh.group(1)) if fresh else None,
        "enabled": phase.get("enabled"),
        "ran": phase.get("ran"),
        # The margin the loop chose from its own measured scatter, which is the
        # number a run has to be read against rather than a constant it aimed at.
        "margin": phase.get("margin_ms"),
        "spread": phase.get("spread_ms"),
        "shifts": phase.get("shifts"),
        "reason": phase.get("unavailable_reason"),
    }


off, on = arm("off"), arm("on")

print(f"\none period {PERIOD_MS:.2f} ms, an unaligned phase is predicted to wait {HALF:.2f} ms\n")
for name, data in (("alignment off", off), ("alignment on", on)):
    if data is None:
        print(f"  {name:<16} no figures")
        continue
    print(
        f"  {name:<16} presentation wait p50 {data['p50']:>6.2f} ms  p99 {data['p99']:>6.2f} ms"
        f"   rendered {data['rendered']:>6}   fresh {data['fresh']}%"
        f"   shifts {data['shifts']}"
    )

failures = []
if off is None or on is None:
    failures.append("an arm produced no figures, so there is nothing to compare")
else:
    # The boundary is derived rather than picked: the mechanism aims frames a
    # stated margin in front of the deadline, and an unaligned phase averages
    # half a period, so the midpoint between the two is the only place a
    # decision can sit without the gate inventing a number of its own. Reading
    # the margin out of the run's own report also stops the gate and the code
    # drifting apart when somebody retunes the loop.
    target = on["margin"]
    if target is None:
        failures.append("the aligned arm did not report the margin it chose")
    else:
        boundary = (target + HALF) / 2.0
        print(f"\n  the loop chose a {target:.2f} ms margin from a {on['spread']:.2f} ms spread, so the")
        print(f"  boundary between the two predictions is {boundary:.2f} ms\n")
        # The negative control, and the half that makes the other half mean
        # something: an unaligned arm that already sits where alignment aims
        # proves the phase was favourable, not that the mechanism works.
        if off["p50"] < boundary:
            failures.append(
                f"the unaligned arm waited {off['p50']:.2f} ms, on the aligned side of "
                f"{boundary:.2f} ms, so this run cannot tell the mechanism from a "
                "favourable phase"
            )
        if on["p50"] > boundary:
            failures.append(
                f"the aligned arm waited {on['p50']:.2f} ms, on the unaligned side of "
                f"{boundary:.2f} ms"
            )
    # The mechanism has to have acted, and the control has to have been silent.
    if not on["ran"]:
        failures.append(f"the loop never ran in the aligned arm: {on['reason']}")
    if on["shifts"] == 0:
        failures.append("the aligned arm asked for no shift, so nothing was aligned")
    if off["enabled"] or off["shifts"]:
        failures.append("the unaligned arm was not a control: it ran the loop")
    # Bought with frames rather than with phase.
    if on["rendered"] < off["rendered"] * 0.98:
        failures.append(
            f"the aligned arm rendered {on['rendered']} against {off['rendered']}: "
            "the wait went down because frames went missing"
        )
    if on["fresh"] is not None and off["fresh"] is not None and on["fresh"] < off["fresh"] - 1.0:
        failures.append(
            f"fresh ticks fell from {off['fresh']}% to {on['fresh']}%: the phase moved "
            "off the display's opportunities rather than onto them"
        )

print()
if failures:
    for failure in failures:
        print(f"FAIL {failure}")
    sys.exit(1)
print(
    f"PASS alignment moved the wait from {off['p50']:.2f} ms to {on['p50']:.2f} ms at the "
    f"same frame rate, and the control sat at the predicted {HALF:.2f} ms rather than "
    "anywhere convenient"
)
PY
