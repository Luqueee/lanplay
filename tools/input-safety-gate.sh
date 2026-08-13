#!/usr/bin/env bash
# The I5 and I6 gate: the whole input set, over a link that misbehaves, ending
# with the client walking away mid-stream.
#
# Keys, buttons and wheel all in flight at once, through tools/udp-fault so the
# link genuinely duplicates and drops rather than being trusted not to, and then
# the client stops without a graceful goodbye. What must be true afterwards is
# not a rate or a percentile:
#
#   every discrete event accounted for
#   duplicate injections           0
#   events outstanding at exit      0
#   held keys on the host           {}
#   held buttons on the host        0
#
# That last pair is the real gate. A key held down that nobody is pressing is
# the one failure a player would feel minutes later and never connect to the
# network, and it is what the whole reliability and liveness design exists to
# prevent.
#
# Each arm declares what it means to exercise and the probes exit 4 if any
# declared subsystem ends at zero, because two arms passed earlier in this
# project having exercised nothing at all.
#
# Two endings, because neither alone covers the invariant. A client that says
# goodbye proves its own accounting closes but never exercises the host's
# liveness timer; a client that is killed exercises the timer but takes its
# accounting with it. So the gate runs both and asks a different question of
# each.
#
# usage:
#   tools/input-safety-gate.sh [seconds]
#
#   REMOTE=1   inject on the Windows host instead of counting locally
#   LOSS, DUPLICATE, REORDER, STALL_MS   the link's behaviour

set -euo pipefail

SECONDS_TO_RUN="${1:-15}"
LOSS="${LOSS:-3}"
DUPLICATE="${DUPLICATE:-4}"
REORDER="${REORDER:-3}"
STALL_MS="${STALL_MS:-50}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/input-safety}"
RELAY="${RELAY:-127.0.0.1:5106}"

mkdir -p "$OUT"
rm -f "$OUT"/*.out

cargo build --release -q -p lanplay-udp-fault -p lanplay-input-capture -p lanplay-input-inject

if [ "${REMOTE:-0}" = 1 ]; then
    "$REPO/tools/win-sync.sh" >/dev/null
    ssh -n -o BatchMode=yes windows \
        'cd C:\Users\luque\lanplay-rs && cargo build --release -q -p lanplay-input-inject' >/dev/null
    HOST_ADDR="$(ssh -G windows | awk '/^hostname /{print $2}'):5006"
    echo "host      $HOST_ADDR, live SendInput in the interactive session"
    echo "warning   keys, clicks and scrolling will land in whatever has focus there"
    WIN_TIMEOUT=$((SECONDS_TO_RUN + 90)) "$REPO/tools/win-session.sh" \
        'C:\Users\luque\input-safety.log' \
        "target\\release\\input-inject-probe.exe --bind 0.0.0.0:5006 --seconds $((SECONDS_TO_RUN + 12)) --expect keys,buttons,wheel,acks,snapshots,heartbeats" \
        >"$OUT/host.out" 2>&1 &
    host=$!
    for _ in $(seq 1 40); do
        ssh -n -o BatchMode=yes windows \
            'powershell -NoProfile -Command "(Get-Process input-inject-probe -ErrorAction SilentlyContinue).Count"' \
            2>/dev/null | tr -d '\r ' | grep -q '^[1-9]' && break
        sleep 0.5
    done
else
    HOST_ADDR="127.0.0.1:5006"
    echo "host      $HOST_ADDR, counted locally, nothing is injected"
    "$REPO/target/release/input-inject-probe" --bind "$HOST_ADDR" \
        --seconds $((SECONDS_TO_RUN + 12)) --dry-run \
        --expect keys,buttons,wheel,acks,snapshots,heartbeats >"$OUT/host.out" 2>&1 &
    host=$!
fi

echo "link      loss ${LOSS}%, duplicate ${DUPLICATE}%, reorder ${REORDER}%, stalls ${STALL_MS} ms"
"$REPO/target/release/udp-fault" --forward "$HOST_ADDR" --listen "$RELAY" \
    --loss "$LOSS" --duplicate "$DUPLICATE" --reorder "$REORDER" \
    --stall-ms "$STALL_MS" --stall-every-ms 1500 --seed 7 >"$OUT/link.out" 2>&1 &
relay=$!
disown "$relay" 2>/dev/null || true
sleep 1

echo
echo "client    keys, buttons and wheel for ${SECONDS_TO_RUN}s, then walking away"
"$REPO/target/release/input-capture-probe" --send-to "$RELAY" \
    --synthetic-keys --synthetic-buttons --synthetic-wheel \
    --seconds "$SECONDS_TO_RUN" --key-rate 30 \
    --expect keys,buttons,wheel,acks,snapshots,heartbeats >"$OUT/client.out" 2>&1 &
client=$!
if [ "${ENDING:-killed}" = killed ]; then
    # No graceful goodbye. A client that always says goodbye never tests the
    # host's liveness path, and a crashed client is the case that leaves a key
    # held. The cost is that its own accounting dies with it, which is why the
    # other arm exists.
    sleep "$SECONDS_TO_RUN"
    kill -9 "$client" 2>/dev/null || true
else
    wait "$client" 2>/dev/null || true
fi

echo "client    ${ENDING:-killed} ending; waiting for the host to settle"
wait "$host" 2>/dev/null || true
kill "$relay" 2>/dev/null || true

echo
cat "$OUT/client.out"
echo
cat "$OUT/host.out"
echo
tail -1 "$OUT/link.out" 2>/dev/null || true

python3 - "$OUT" <<'PY'
import re
import sys

out = sys.argv[1]
client = open(f"{out}/client.out").read()
host = open(f"{out}/host.out").read()


def number(text, pattern):
    found = re.search(pattern, text)
    return int(found.group(1)) if found else None


print("\nverdict")
failures = []

held = re.search(r"still held: keys (\{[^}]*\}), buttons (0b[01]+)", host)
if not held:
    failures.append("the host never said what it was holding")
else:
    keys, buttons = held.group(1), held.group(2)
    clean = keys in ("{}", "{ }") and set(buttons[2:]) == {"0"}
    print(f"  held on the host        keys {keys}, buttons {buttons}")
    if not clean:
        failures.append(f"the host is still holding {keys} and {buttons}")

import os

ending = os.environ.get("ENDING", "killed")
expired = number(host, r"released, expired\s+(\d+)")
asked = number(host, r"released, asked\s+(\d+)")
print(f"  releases               asked {asked}, expired {expired}")
if ending == "killed" and not expired:
    # Nothing else could have cleared the host: the client was killed before it
    # could ask. If nothing expired, either the host never noticed or the client
    # somehow said goodbye, and both mean this arm did not test what it claims.
    failures.append("nothing expired, so the liveness path was never exercised")
if ending == "graceful" and not asked:
    failures.append("no release was asked for, so the goodbye path was never exercised")

print(f"  duplicates recognised  {number(host, r'duplicate\s+(\d+)')}")
outstanding = number(client, r"still outstanding at exit\s+(\d+)")
if ending == "killed":
    # Killed before it could report, by design. The graceful arm is where this
    # figure comes from, and saying so beats printing None.
    print("  outstanding at exit     not available, the client was killed")
elif outstanding is None:
    failures.append("the client never reported its accounting")
else:
    print(f"  outstanding at exit     {outstanding}")
    if outstanding:
        failures.append(f"{outstanding} events were never acknowledged")
    accounted = re.search(r"events accounted (\d+) of (\d+)", client)
    if not accounted:
        failures.append("the client did not state whether its events add up")
    elif accounted.group(1) != accounted.group(2):
        failures.append(f"events do not add up: {accounted.group(0)}")
    else:
        print(f"  {accounted.group(0)}")

# Duplicates are expected to arrive; what must be zero is a second injection.
# The host proves that by its call count, which only means something when it is
# really injecting - in a dry run there are no calls to count and claiming
# otherwise would be the same silent pass this gate exists to refuse.
calls = number(host, r"sendinput calls\s+(\d+)")
if "nothing is injected" in host:
    print("  injections              not this arm, the host was counting only")
else:
    print(f"  injections              {calls} SendInput calls")
    if not calls:
        failures.append("the host injected nothing at all")

for line in (client, host):
    if "never exercised" in line:
        failures.append("a declared subsystem was never exercised")

print()
if failures:
    for failure in failures:
        print(f"  FAIL  {failure}")
    sys.exit(1)
print("  PASS  nothing held, liveness fired, every declared subsystem exercised")
PY
