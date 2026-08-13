#!/usr/bin/env bash
# Real capture into a real game, which is the first test here whose most
# important result is not a number.
#
# Everything up to now has been synthetic on both ends: posted events, counted
# datagrams, deliberately broken links. All of it necessary and none of it able
# to answer the question that decides whether a virtual HID device is worth
# building - does the game see the input, and does it feel right.
#
# So the counters below are the supporting evidence and the operator is the
# instrument. What the counters can settle: whether the game's window received
# anything at all, whether Windows refused any injection, whether keys were
# captured on the Mac in the first place, and whether anything was left held.
# What only a person can settle: whether the camera tracks the hand.
#
# The Mac becomes unusable while this runs, on purpose. The cursor is detached
# and the keyboard is forwarded, which is what capture means. Command, control
# and option together hand it back.
#
# usage:
#   tools/game-input-test.sh [seconds]

set -euo pipefail

SECONDS_TO_RUN="${1:-60}"
PORT=5006
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/game-input}"

mkdir -p "$OUT"
rm -f "$OUT"/*.out

cargo build --release -q -p lanplay-input-capture -p lanplay-input-inject
"$REPO/tools/win-sync.sh" >/dev/null
"$REPO/tools/win-ssh.sh" \
    'cd C:\Users\luque\lanplay-rs && cargo build --release -q -p lanplay-input-inject' >/dev/null

HOST_IP="$(ssh -G windows | awk '/^hostname /{print $2}')"
echo "host      $HOST_IP:$PORT, live SendInput in the interactive session"
echo "client    real capture, no synthetic anything"

# No fault injection. A game test that also fights a broken link cannot say
# which of the two produced a stutter, and the link has already been measured
# under far worse conditions than a LAN gives it.
WIN_TIMEOUT=$((SECONDS_TO_RUN + 120)) "$REPO/tools/win-session.sh" \
    'C:\Users\luque\game-input.log' \
    "target\\release\\input-inject-probe.exe --bind 0.0.0.0:$PORT --seconds $((SECONDS_TO_RUN + 20)) --expect keys,acks" \
    >"$OUT/host.out" 2>&1 &
host=$!

for _ in $(seq 1 40); do
    "$REPO/tools/win-ssh.sh" \
        'powershell -NoProfile -Command "(Get-Process input-inject-probe -ErrorAction SilentlyContinue).Count"' \
        2>/dev/null | tr -d '\r ' | grep -q '^[1-9]' && break
    sleep 0.5
done
echo "injector  listening"

cat <<'INSTRUCTIONS'

  Click anywhere on the Mac to take capture. From that moment the Mac's mouse
  and keyboard drive Windows and the Mac's own cursor stops moving, which is
  what capture means and not a fault.

  Play. Move the camera, drive, boost, jump, use the buttons.

  Command, control and option together, command first, hands it back.

INSTRUCTIONS

"$REPO/target/release/input-capture-probe" --send-to "$HOST_IP:$PORT" \
    --seconds "$SECONDS_TO_RUN" --keys \
    --expect motion,keys,acks,captures 2>&1 | tee "$OUT/client.out" || true

echo
echo "waiting for the host to settle"
wait "$host" 2>/dev/null || true
echo
cat "$OUT/host.out"

python3 - "$OUT" <<'PY'
import re
import sys

out = sys.argv[1]
client = open(f"{out}/client.out").read()
host = open(f"{out}/host.out").read()


def find(text, pattern):
    got = re.search(pattern, text)
    return int(got.group(1)) if got else None


print("\nwhat the counters can settle")
for label, value, want_zero in (
    ("mouse events captured", find(client, r"mouse events (\d+)"), False),
    ("keys captured", find(client, r"key datagrams (\d+)"), False),
    ("capture cycles", find(client, r"capture cycles\s+(\d+)"), False),
    ("motion injected", find(host, r"motion\s+(\d+)"), False),
    ("keys injected", find(host, r"\nkeys\s+(\d+)"), False),
    ("injections refused by Windows", find(host, r"refused\s+(\d+)"), True),
    ("events abandoned", find(client, r"abandoned\s+(\d+)"), True),
    ("keys held at the end", 0 if re.search(r"still held: keys \{\}", host) else 1, True),
):
    verdict = ""
    if value is None:
        verdict = "  not reported"
    elif want_zero and value:
        verdict = "  SHOULD BE ZERO"
    elif not want_zero and not value:
        verdict = "  SHOULD NOT BE ZERO"
    print(f"  {label:<32} {value}{verdict}")

print(
    "\nwhat only you can settle: did the camera follow your hand, did the car\n"
    "respond, did anything stutter or lag in a way the numbers above do not\n"
    "explain."
)
PY
