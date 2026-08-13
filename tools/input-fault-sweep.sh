#!/usr/bin/env bash
# I7: does reliable input survive a bad link, rather than being assumed to.
#
# Everything measured before this crossed a LAN that lost nothing, so the
# retransmission and snapshot machinery had never actually run. It turned out
# not to work: the first sweep at five per cent loss abandoned 385 of 403
# events, because the acknowledgement was anchored at the bottom and one
# permanently lost id blocked every id more than 32 above it. That is the kind
# of thing only a deliberately bad link finds.
#
# Each arm runs the whole path through tools/udp-fault, which drops, duplicates,
# reorders and stalls to order with a seeded generator so a failing arm can be
# repeated exactly.
#
#   client ──► udp-fault ──► input-inject-probe
#          ◄──          ◄──
#
# The figure that decides an arm is not the loss rate. It is whether the host
# ends holding a key nobody is pressing, because that is the failure the whole
# reliability design exists to prevent and the only one a player would feel
# minutes later.
#
# usage:
#   tools/input-fault-sweep.sh [seconds-per-arm]
#
#   HOST=127.0.0.1:5006   where the injector listens
#   RATE=40               key events a second

set -euo pipefail

SECONDS_PER_ARM="${1:-12}"
HOST="${HOST:-127.0.0.1:5006}"
RATE="${RATE:-40}"
RELAY="${RELAY:-127.0.0.1:5106}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/input-fault}"

mkdir -p "$OUT"
cargo build --release -q -p lanplay-udp-fault -p lanplay-input-capture -p lanplay-input-inject

# Loss, duplicate, reorder, stall milliseconds, stall interval. The clean arm
# first, so a failure in it condemns the harness rather than the link.
arms=(
    "clean 0 0 0 0 0"
    "loss-tenth 0.1 0 0 0 0"
    "loss-one 1 0 0 0 0"
    "loss-five 5 0 0 0 0"
    "reorder 0 0 5 0 0"
    "duplicate 0 5 0 0 0"
    "stall 0 0 0 50 1000"
    "everything 5 2 3 50 2000"
)

printf '%-12s %8s %8s %8s %8s %8s %10s  %s\n' \
    arm sent retx aband ackd snaps outstd "held on host"

for arm in "${arms[@]}"; do
    read -r name loss dup reorder stall_ms stall_every <<<"$arm"
    pkill -f input-inject-probe >/dev/null 2>&1 || true
    pkill -f udp-fault >/dev/null 2>&1 || true
    sleep 0.5

    "$REPO/target/release/input-inject-probe" --bind "$HOST" \
        --seconds $((SECONDS_PER_ARM + 8)) --dry-run >"$OUT/$name.host" 2>&1 &
    host=$!
    "$REPO/target/release/udp-fault" --forward "$HOST" --listen "$RELAY" \
        --loss "$loss" --duplicate "$dup" --reorder "$reorder" \
        --stall-ms "$stall_ms" --stall-every-ms "${stall_every:-5000}" \
        --seed 42 >"$OUT/$name.net" 2>&1 &
    relay=$!
    # Detached from job control so killing it at the end of an arm does not
    # print a termination notice into the middle of the table.
    disown "$relay" 2>/dev/null || true
    sleep 1

    "$REPO/target/release/input-capture-probe" --send-to "$RELAY" \
        --synthetic-keys --seconds "$SECONDS_PER_ARM" --key-rate "$RATE" \
        >"$OUT/$name.client" 2>&1 || true

    # The injector outlives the client so a late retransmission still lands,
    # and the held set it prints is read after that has settled.
    wait "$host" 2>/dev/null || true
    kill "$relay" 2>/dev/null || true

    python3 - "$OUT" "$name" <<'PY'
import re, sys
out, name = sys.argv[1], sys.argv[2]
client = open(f"{out}/{name}.client").read()
host = open(f"{out}/{name}.host").read()


def one(text, pattern, default="?"):
    found = re.search(pattern, text)
    return found.group(1) if found else default


held = one(host, r"still held: keys (\{[^}]*\})", "not reported")
print(
    f"{name:<12} "
    f"{one(client, r'reliable events sent (\d+)'):>8} "
    f"{one(client, r'retransmissions (\d+)'):>8} "
    f"{one(client, r'abandoned\s+(\d+)'):>8} "
    f"{one(client, r'acknowledged\s+(\d+)'):>8} "
    f"{one(client, r'snapshots sent (\d+)'):>8} "
    f"{one(client, r'still outstanding at exit\s+(\d+)'):>10}  "
    f"{held}{'' if held in ('{}', '{ }') else '   STUCK'}"
)
PY
done

pkill -f input-inject-probe >/dev/null 2>&1 || true
pkill -f udp-fault >/dev/null 2>&1 || true
echo
echo "detail in $OUT"
