#!/usr/bin/env bash
# A4: does the jitter buffer stay bounded, and does it conceal rather than stall?
#
# Four arms through the same relay, three of them broken on purpose. The clean arm
# says the machinery is correct; the broken ones say it does the right thing when the
# network does the wrong thing, and they are the reason this gate exists at all - a
# buffer only tested on a path that never loses anything has had its whole purpose
# left unexercised.
#
# The criterion the plan cares about most is not latency, it is that occupancy never
# grows without bound. A buffer that absorbed a stall by growing would trade a fault
# that ends for latency that never recovers, and it would look healthy in every
# counter except the one nobody printed. So every arm is checked against the
# buffer's own ceiling.
#
# The other one that matters is the difference between concealing and stalling. Audio
# is an ordered continuous stream, not latest-frame-wins: a missing frame is filled
# by the codec's own concealment and the stream carries on in order. So the loss arm
# must show concealment happening - a loss arm with zero concealed frames either lost
# nothing or hid the evidence, and both are failures of this gate rather than passes.
#
# Seeded, so an arm that fails fails the same way when it is run again.
#
# usage:
#   tools/jitter-gate.sh [seconds]

set -euo pipefail

SECONDS_TO_RUN="${1:-20}"
TARGET_MS=10
FRAME_MS=5
RELAY_PORT=5108
SINK_PORT=5109
SEED=42
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/jitter-gate/$(date +%Y%m%d-%H%M%S)}"

mkdir -p "$OUT"
echo "results   $OUT"

cargo build --release -q -p lanplay-audio-codec -p lanplay-udp-fault
PROBE="$REPO/target/release/audio-jitter-probe"
FAULT="$REPO/target/release/udp-fault"

run_arm() {
    local name="$1"
    shift
    pkill -f "udp-fault --listen 127.0.0.1:$RELAY_PORT" 2>/dev/null || true
    sleep 0.3
    # The relay sits between the sender and the receiver so the faults are applied to
    # real datagrams on a real socket rather than simulated inside the buffer's own
    # tests. A buffer that only ever saw a fault its own test constructed has not met
    # one.
    "$FAULT" --listen "127.0.0.1:$RELAY_PORT" --forward "127.0.0.1:$SINK_PORT" \
        --seed "$SEED" "$@" >"$OUT/$name.relay" 2>&1 &
    local relay=$!
    sleep 0.5
    "$PROBE" --bind "0.0.0.0:$SINK_PORT" --send-to "127.0.0.1:$RELAY_PORT" \
        --seconds "$SECONDS_TO_RUN" --frame-ms "$FRAME_MS" --target-ms "$TARGET_MS" \
        >"$OUT/$name.out" 2>&1 || true
    kill "$relay" 2>/dev/null || true
    wait "$relay" 2>/dev/null || true
    echo "arm       $name done"
}

run_arm clean
run_arm loss --loss 2
run_arm stall --stall-ms 40 --stall-every-ms 3000
run_arm churn --reorder 5 --reorder-hold-ms 8 --duplicate 3

python3 - "$OUT" "$TARGET_MS" "$FRAME_MS" <<'PY'
import re
import sys

out, target_ms, frame_ms = sys.argv[1], float(sys.argv[2]), float(sys.argv[3])

# The buffer's own policy, restated here because a gate that invented its own ceiling
# would be testing a number nobody implemented: three times the target, and never
# less than the target plus four frames.
CEILING_MS = max(3.0 * target_ms, target_ms + 4.0 * frame_ms)
LEFT_HZ, RIGHT_HZ = 997.0, 1997.0
TONE_TOLERANCE_HZ = 5.0


def arm(name):
    try:
        body = open(f"{out}/{name}.out").read()
    except OSError:
        return None

    def num(pattern):
        got = re.search(pattern, body, re.M)
        return int(got.group(1)) if got else None

    occupancy = re.search(
        r"^occupancy ms p50 ([\d.]+) p95 ([\d.]+) p99 ([\d.]+) max ([\d.]+)$", body, re.M
    )
    overruns = re.search(r"^overruns (\d+) dropping (\d+) frames$", body, re.M)
    continuity = re.search(r"^continuity expected (\d+) played (\d+)$", body, re.M)
    tone = re.search(r"^tone left ([\d.]+) right ([\d.]+)$", body, re.M)
    return {
        "name": name,
        "received": num(r"^packets received (\d+)$"),
        "sent": num(r"^packets sent (\d+)$"),
        "late": num(r"^packets late (\d+)$"),
        "duplicate": num(r"^packets duplicate (\d+)$"),
        "reordered": num(r"^packets reordered (\d+)$"),
        "played": num(r"^frames played (\d+)$"),
        "concealed": num(r"^frames concealed (\d+)$"),
        "underruns": num(r"^underruns (\d+)$"),
        "overruns": [int(g) for g in overruns.groups()] if overruns else None,
        "occupancy": [float(g) for g in occupancy.groups()] if occupancy else None,
        "continuity": [int(g) for g in continuity.groups()] if continuity else None,
        "tone": (float(tone.group(1)), float(tone.group(2))) if tone else None,
    }


arms = [a for a in (arm("clean"), arm("loss"), arm("stall"), arm("churn")) if a]

print(f"\ntarget {target_ms:.0f} ms, ceiling {CEILING_MS:.0f} ms, frame {frame_ms:.0f} ms\n")
print(f"  {'arm':<7} {'sent':>6} {'recv':>6} {'late':>5} {'dup':>5} {'reord':>6} {'conceal':>8} {'under':>6} {'over':>5} {'occ p99':>8} {'occ max':>8}")
for a in arms:
    occ = a["occupancy"] or [0, 0, 0, 0]
    over = a["overruns"] or [0, 0]
    print(
        f"  {a['name']:<7} {a['sent']:>6} {a['received']:>6} {a['late']:>5} {a['duplicate']:>5}"
        f" {a['reordered']:>6} {a['concealed']:>8} {a['underruns']:>6} {over[0]:>5}"
        f" {occ[2]:>8.1f} {occ[3]:>8.1f}"
    )

print()
for a in arms:
    tone = f"{a['tone'][0]:.1f} / {a['tone'][1]:.1f} Hz" if a["tone"] else "not measured"
    cont = a["continuity"] or [0, 0]
    hole = cont[0] - cont[1]
    print(f"  {a['name']:<7} tone {tone}   continuity expected {cont[0]} played {cont[1]} hole {hole}")

failures = []
findings = []

for a in arms:
    where = a["name"]
    if not a["sent"]:
        failures.append(f"{where}: nothing was sent, so every zero is an absence")
        continue
    if a["tone"] is None:
        failures.append(f"{where}: the tone was not measured, so nothing proves audio was played")
    else:
        left, right = a["tone"]
        if abs(left - LEFT_HZ) > TONE_TOLERANCE_HZ or abs(right - RIGHT_HZ) > TONE_TOLERANCE_HZ:
            failures.append(
                f"{where}: played tone {left:.1f} / {right:.1f} Hz against "
                f"{LEFT_HZ:.0f} / {RIGHT_HZ:.0f} - concealment forever reads as audio otherwise"
            )

    # The criterion the plan cares about most, and it applies to every arm including
    # the broken ones: a stall may cost continuity but it may never cost unbounded
    # latency.
    if a["occupancy"] is None:
        failures.append(f"{where}: occupancy was not reported, which is the criterion")
    elif a["occupancy"][3] > CEILING_MS + frame_ms:
        failures.append(
            f"{where}: occupancy reached {a['occupancy'][3]:.1f} ms against a "
            f"{CEILING_MS:.0f} ms ceiling - the buffer grew instead of holding its bound"
        )

    if where == "clean":
        # Loopback through a relay that was told to break nothing. Anything here is
        # this code's own doing.
        for label, value in (("underruns", a["underruns"]), ("concealed", a["concealed"]),
                             ("late", a["late"]), ("duplicate", a["duplicate"])):
            if value:
                failures.append(f"clean: {value} {label} on a path that was told to break nothing")
        if a["occupancy"] and abs(a["occupancy"][0] - target_ms) > frame_ms:
            failures.append(
                f"clean: occupancy settled at {a['occupancy'][0]:.1f} ms against a "
                f"{target_ms:.0f} ms target"
            )
        if a["continuity"] and a["continuity"][0] != a["continuity"][1]:
            failures.append(
                f"clean: continuity expected {a['continuity'][0]} against {a['continuity'][1]} "
                "played - a clean path has no holes to explain"
            )
    elif where == "loss":
        # The arm exists to exercise concealment. Zero concealed frames means either
        # nothing was lost or the mechanism was bypassed, and neither is a pass.
        if not a["concealed"]:
            failures.append(
                "loss: nothing was concealed, so either the relay dropped nothing or the "
                "buffer filled a gap some other way - the arm proved nothing either way"
            )
        else:
            findings.append(
                f"loss: {a['concealed']} frames concealed of {a['sent']} sent, "
                f"{100.0 * a['concealed'] / a['sent']:.2f} %, and the tone survived it"
            )
        if a["underruns"]:
            failures.append(
                f"loss: {a['underruns']} underruns - a lost frame should be concealed, not "
                "leave the sink with nothing"
            )
    elif where == "stall":
        # This arm exercises concealment and late discard, and it CANNOT exercise the
        # ceiling. A bounded delay cannot make a real-time stream arrive faster than
        # real time: after a stall of D the newest frame leads the cursor by the target
        # and the oldest is already behind it, so occupancy after the burst is at most
        # the target however long D was. The ceiling exists for a sink that consumes
        # slower than the source produces - a clock difference, which is A7 - and this
        # gate says so rather than crediting the arm with a path it cannot reach.
        if not a["concealed"] and not a["late"]:
            failures.append(
                "stall: nothing was concealed and nothing arrived late, so the stall never "
                "reached the buffer and the arm tested nothing"
            )
        else:
            hole = (a["continuity"][0] - a["continuity"][1]) if a["continuity"] else 0
            findings.append(
                f"stall: {a['late']} frames arrived past their moment and were discarded, "
                f"{a['concealed']} concealed, {a['underruns']} underruns, and the "
                f"{hole} sample hole is exactly {a['underruns']} frames of {hole // max(a['underruns'], 1)}"
            )
            findings.append(
                "the ceiling was NOT exercised by any arm and cannot be by a delay: only a "
                "sink slower than its source can breach it, which is A7's subject"
            )
    elif where == "churn":
        if not a["reordered"] and not a["duplicate"]:
            failures.append(
                "churn: no reordering and no duplicate arrived, so the arm tested nothing"
            )
        else:
            findings.append(
                f"churn: {a['reordered']} reordered and {a['duplicate']} duplicates absorbed, "
                f"{a['concealed']} concealed, {a['underruns']} underruns"
            )

print()
for finding in findings:
    print(f"FINDING {finding}")
print()
if failures:
    for failure in failures:
        print(f"FAIL {failure}")
    sys.exit(1)
print(
    "PASS the buffer holds its bound under loss, a stall and churn, conceals rather than\n"
    "     stalling, and plays the tone in every arm"
)
PY
