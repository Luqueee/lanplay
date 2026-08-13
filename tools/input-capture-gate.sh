#!/usr/bin/env bash
# The I9 gate: capture, release, and everything that must not leak across them.
#
# Two kinds of criterion here, and keeping them apart is the point. Some numbers
# must be zero and some must not, and a gate that treats every zero as good news
# is how two arms passed earlier in this project having exercised nothing at all.
# So each criterion below says which it is, and a population that should have
# been non-zero fails exactly as loudly as a leak.
#
#   reported only        releases caused by expiry, because the host outlives
#                        the client here so that late retransmissions land
#
#   must be zero        capture clicks that reached the host
#                       release hotkey events that reached the host
#                       events applied from before a release barrier
#                       remote events while uncaptured
#                       keys and buttons held at the end
#
#   must not be zero    capture cycles
#                       events refused while uncaptured
#                       events accepted after recapture
#                       releases requested, and acknowledged
#
# usage:
#   tools/input-capture-gate.sh [cycles]
#
#   REMOTE=1   inject on the Windows host instead of counting locally
#   LOSS, DUPLICATE, REORDER   the link's behaviour, so late delivery is real

set -euo pipefail

CYCLES="${CYCLES:-${1:-25}}"
LOSS="${LOSS:-3}"
DUPLICATE="${DUPLICATE:-4}"
REORDER="${REORDER:-5}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/input-capture-gate}"
RELAY_BIND="${RELAY_BIND:-0.0.0.0:5106}"
RELAY="${RELAY:-127.0.0.1:5106}"

mkdir -p "$OUT"
rm -f "$OUT"/*.out

cargo build --release -q -p lanplay-udp-fault -p lanplay-input-capture -p lanplay-input-inject

# Reordering matters more here than loss. Sequence C needs an event generated
# before a release to arrive after it, which is exactly what a reordering link
# produces and what the host's barrier exists to refuse.
if [ "${REMOTE:-0}" = 1 ]; then
    "$REPO/tools/win-sync.sh" >/dev/null
    "$REPO/tools/win-ssh.sh" 'cd C:\Users\luque\lanplay-rs && cargo build --release -q -p lanplay-input-inject' >/dev/null
    HOST_ADDR="$(ssh -G windows | awk '/^hostname /{print $2}'):5006"
    echo "host      $HOST_ADDR, live SendInput in the interactive session"
    echo "warning   input will land in whatever has focus there"
    WIN_TIMEOUT=$((CYCLES * 4 + 120)) "$REPO/tools/win-session.sh" \
        'C:\Users\luque\input-capture-gate.log' \
        "target\\release\\input-inject-probe.exe --bind 0.0.0.0:5006 --seconds $((CYCLES * 2 + 30)) --expect keys,buttons,acks" \
        >"$OUT/host.out" 2>&1 &
    host=$!
    for _ in $(seq 1 40); do
        "$REPO/tools/win-ssh.sh" \
            'powershell -NoProfile -Command "(Get-Process input-inject-probe -ErrorAction SilentlyContinue).Count"' \
            2>/dev/null | tr -d '\r ' | grep -q '^[1-9]' && break
        sleep 0.5
    done
else
    HOST_ADDR="127.0.0.1:5006"
    echo "host      $HOST_ADDR, counted locally, nothing is injected"
    "$REPO/target/release/input-inject-probe" --bind "$HOST_ADDR" \
        --seconds $((CYCLES * 2 + 30)) --dry-run --expect keys,buttons,acks \
        >"$OUT/host.out" 2>&1 &
    host=$!
fi

echo "link      loss ${LOSS}%, duplicate ${DUPLICATE}%, reorder ${REORDER}%"
"$REPO/target/release/udp-fault" --forward "$HOST_ADDR" --listen "$RELAY_BIND" \
    --loss "$LOSS" --duplicate "$DUPLICATE" --reorder "$REORDER" \
    --reorder-hold-ms 40 --seed 11 >"$OUT/link.out" 2>&1 &
relay=$!
disown "$relay" 2>/dev/null || true
sleep 1

echo
echo "client    $CYCLES capture cycles through sequences A to D"
"$REPO/target/release/input-capture-probe" --send-to "$RELAY" \
    --cycles "$CYCLES" --expect keys,buttons,acks,captures,suppressed \
    >"$OUT/client.out" 2>&1 &
client=$!
wait "$client" 2>/dev/null || echo "      client exited $?"

echo "client    done; waiting for the host to settle"
wait "$host" 2>/dev/null || true
kill "$relay" 2>/dev/null || true

echo
cat "$OUT/client.out"
echo
cat "$OUT/host.out"

python3 - "$OUT" "$CYCLES" <<'PY'
import re
import sys

out, cycles = sys.argv[1], int(sys.argv[2])
client = open(f"{out}/client.out").read()
host = open(f"{out}/host.out").read()
failures = []
print("\nverdict")


def find(text, pattern):
    got = re.search(pattern, text)
    return int(got.group(1)) if got else None


def must_be_zero(label, value):
    if value is None:
        failures.append(f"{label} was never reported")
        print(f"  {label:<38} not reported")
        return
    print(f"  {label:<38} {value}")
    if value:
        failures.append(f"{label} should be zero and is {value}")


def must_not_be_zero(label, value, least=1):
    if value is None:
        failures.append(f"{label} was never reported")
        print(f"  {label:<38} not reported")
        return
    print(f"  {label:<38} {value}")
    if value < least:
        failures.append(f"{label} is {value}, below the {least} this arm requires")


print("  must not be zero")
must_not_be_zero("capture cycles", find(client, r"capture cycles\s+(\d+)"), cycles)
must_not_be_zero(
    "events refused while uncaptured", find(client, r"refused while uncaptured\s+(\d+)")
)
must_not_be_zero("capture clicks suppressed", find(client, r"capture clicks suppressed\s+(\d+)"))
must_not_be_zero("hotkey events suppressed", find(client, r"hotkey events suppressed\s+(\d+)"))
must_not_be_zero("releases sent", find(client, r"releases sent\s+(\d+)"))
must_not_be_zero("releases the host applied", find(host, r"released, asked\s+(\d+)"))

print("  must be zero")
must_be_zero("events applied from before a barrier", find(host, r"superseded applied\s+(\d+)") or 0)
# Expiry is reported and does not vote. The host is given a longer window than
# the client on purpose, so that a late retransmission still lands, which means
# it always ends up sweeping a client that has already finished. Requiring zero
# here was a criterion that could not be met by a correct run - the sweep is the
# invariant working - and a gate with an impossible criterion is worse than one
# with a missing one, because it trains a reader to ignore a failure.
expired = find(host, r"released, expired\s+(\d+)")
print(f"  {'releases caused by expiry':<38} {expired}   reported, does not vote")
must_be_zero("keys held at the end", 0 if re.search(r"still held: keys \{\}", host) else 1)
must_be_zero(
    "buttons held at the end", 0 if re.search(r"buttons 0b0+\s*$", host, re.M) else 1
)
must_be_zero("events abandoned", find(client, r"abandoned\s+(\d+)"))
must_be_zero("events outstanding at exit", find(client, r"still outstanding at exit\s+(\d+)"))

# The pair that only means something together: the barrier must refuse what came
# before it and admit what came after, and a gate that checked only the refusal
# would pass a client that had stopped sending anything at all.
print("  the barrier, both directions")
superseded = find(host, r"superseded\s+(\d+)")
must_not_be_zero("late pre-barrier events refused", superseded)
must_not_be_zero("events accepted after recapture", find(host, r"applied\s+(\d+)"))

accounted = re.search(r"events accounted (\d+) of (\d+)", client)
if not accounted:
    failures.append("the client did not state whether its events add up")
elif accounted.group(1) != accounted.group(2):
    failures.append(f"events do not add up: {accounted.group(0)}")
else:
    print(f"  accounting                             {accounted.group(0)}")

if "never exercised" in client or "never exercised" in host:
    failures.append("a declared subsystem was never exercised")

print()
if failures:
    for failure in failures:
        print(f"  FAIL  {failure}")
    sys.exit(1)
print("  PASS  nothing leaked across a capture boundary and nothing is held")
PY
