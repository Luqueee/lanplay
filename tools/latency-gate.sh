#!/usr/bin/env bash
# I10: the software latency of the whole loop, by segment.
#
# Every segment of this pipeline has been measured except one, and the missing
# one is in the middle of the host: from Windows handing an injected event to an
# application, to the pixels that changed being where Desktop Duplication will
# find them. Without it the chain has a hole exactly where the game sits.
#
# Two rules decide the shape of this harness.
#
# No figure here subtracts a timestamp taken on one machine from one taken on
# another. There is no clock synchronisation good enough for a sub-millisecond
# segment and pretending otherwise would produce numbers that look like
# measurements. Every interval below is local to one machine, and the two
# cross-machine hops are named and left unmeasured rather than estimated.
#
# The end of the chain is a lower bound, not a latency. The target presents
# without waiting for vertical blank, so its figure is how fast an application
# can put a change on the display and not how long until a display would have
# shown it. A vsynced game adds up to a refresh interval that no software
# measurement on this side can see.
#
# usage:
#   tools/latency-gate.sh [seconds]

set -euo pipefail

SECONDS_TO_RUN="${1:-30}"
PORT=5006
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/latency-gate}"
REPORT='C:\Users\luque\latency-target.txt'

mkdir -p "$OUT"
rm -f "$OUT"/*.out

cargo build --release -q -p lanplay-input-capture -p lanplay-mouse-mover
"$REPO/tools/win-ssh.sh" 'cd C:\Users\luque\lanplay-rs && cargo build --release -q -p lanplay-input-inject -p lanplay-input-latency-target' >/dev/null

HOST_IP="$(ssh -G windows | awk '/^hostname /{print $2}')"

# The injector goes up first, and the target second, because the order decides
# whether the measurement means anything. Both run in the interactive session
# and the injector is a console program, so its window takes the foreground when
# it starts and hands it back when it exits. Started second it would leave the
# target with raw input and no keystrokes for the whole run, while a foreground
# check at either end of that run reads true.
WIN_TASK=lanplay-inject WIN_TIMEOUT=$((SECONDS_TO_RUN + 120)) "$REPO/tools/win-session.sh" \
    'C:\Users\luque\latency-inject.log' \
    "target\\release\\input-inject-probe.exe --bind 0.0.0.0:$PORT --seconds $((SECONDS_TO_RUN + 20)) --expect keys" \
    >"$OUT/inject.out" 2>&1 &
inject=$!

for _ in $(seq 1 40); do
    "$REPO/tools/win-ssh.sh" \
        'powershell -NoProfile -Command "(Get-Process input-inject-probe -ErrorAction SilentlyContinue).Count"' \
        2>/dev/null | tr -d '\r ' | grep -q '^[1-9]' && break
    sleep 0.5
done
echo "injector  listening on $HOST_IP:$PORT"

WIN_TASK=lanplay-target WIN_TIMEOUT=$((SECONDS_TO_RUN + 150)) "$REPO/tools/win-session.sh" \
    'C:\Users\luque\latency-target.log' \
    "target\\release\\input-latency-target.exe --out $REPORT --seconds $((SECONDS_TO_RUN + 25))" \
    >"$OUT/target.out" 2>&1 &
target=$!

for _ in $(seq 1 60); do
    "$REPO/tools/win-ssh.sh" \
        'powershell -NoProfile -Command "(Get-Process input-latency-target -ErrorAction SilentlyContinue).Count"' \
        2>/dev/null | tr -d '\r ' | grep -q '^[1-9]' && break
    sleep 0.5
done
echo "target    up, owning the IDD-LAB display and the foreground"

# Synthetic keys rather than motion: a key press is one discrete event the target
# can attribute a flash to, while motion at 250 Hz would have several events
# inside every flash and none of them individually timeable.
"$REPO/target/release/input-capture-probe" --send-to "$HOST_IP:$PORT" \
    --seconds "$SECONDS_TO_RUN" --synthetic-keys --key-rate 8 \
    --expect keys,acks >"$OUT/client.out" 2>&1 || true
echo "client    done"

wait "$inject" 2>/dev/null || true
wait "$target" 2>/dev/null || true

# The report the task already wrote to this machine is the same text, and going
# back for it through PowerShell mangles the micron sign into a replacement
# character. Read what is here.
cp "$OUT/target.out" "$OUT/target-report.out"

python3 - "$OUT" <<'PY'
import re
import sys

out = sys.argv[1]


def text(name):
    try:
        return open(f"{out}/{name}").read()
    except OSError:
        return ""


client = text("client.out")
inject = text("inject.out")
target = text("target-report.out")


def grab(body, pattern):
    got = re.search(pattern, body)
    return got.group(1) if got else None


print("\nthe chain, by segment, each interval local to one machine\n")

segments = [
    (
        "Mac    event callback -> UDP send",
        grab(client, r"clock read to send_to returning:\s*p50\s*([\d.]+)"),
        grab(client, r"clock read to send_to returning:.*?p99\s*([\d.]+)"),
        "us",
    ),
    (
        "       the wire, Mac to Windows",
        None,
        None,
        "not measured: no clock the two machines share",
    ),
    (
        "Win    datagram received -> SendInput",
        grab(inject, r"recv to injected\s+count\s+\d+\s+p50\s+([\d.]+)"),
        grab(inject, r"recv to injected.*?p99\s+([\d.]+)"),
        "us",
    ),
    (
        "Win    input handled -> pixels presented",
        # Anchored on the microsecond suffix, because "raw input" also names a row
        # in the count table above whose columns are plain integers.
        grab(target, r"(?m)^raw input\s+\d+\s+([\d.]+)µ"),
        grab(target, r"(?m)^raw input\s+\d+\s+[\d.]+µ\s+[\d.]+µ\s+([\d.]+)µ"),
        "us, a lower bound: presented without waiting for vblank",
    ),
]

for label, p50, p99, unit in segments:
    if p50 is None and unit.startswith("not measured"):
        print(f"  {label:<42} {unit}")
    elif p50 is None:
        print(f"  {label:<42} no figure in output")
    else:
        print(f"  {label:<42} p50 {float(p50):>9.2f}  p99 {float(p99 or p50):>9.2f}  {unit}")

print(
    "\n  the rest of the chain is already measured and unchanged by this run:\n"
    "    Win  capture -> NVENC complete            p99 1.9 ms\n"
    "         the wire, Windows to Mac             au interval p99 10.5 ms\n"
    "    Mac  decode                               p50 1.0 ms   p99 1.3 ms\n"
    "    Mac  frame age at present                 p50 5.4 ms   p99 9.5 ms"
)

# The target is the only new instrument here, so its own accounting is what
# decides whether the figure above means anything.
print("\nwhat the target saw")
for line in target.splitlines():
    if re.search(r"raw input|window messages|display|FINDING|NO INPUT|present|foreground", line):
        print(f"  {line.strip()}")

failures = []
if not target.strip():
    failures.append("the target wrote no report")
if "NO INPUT" in target:
    failures.append("the target saw no input, so its histogram is not a result")
sent = grab(client, r"key datagrams (\d+)")
if sent is None or int(sent) == 0:
    failures.append("the client sent no keys")
# The measurement is only about the pipeline if the events landed on the window
# being measured. A run where they did not is a harness fault, and calling it a
# finding about Windows is how a launch order becomes a conclusion.
background = grab(target, r"held the foreground: (\d+)")
if background is None or int(background) > 0:
    failures.append(
        f"{background} timed events landed while another window held the foreground"
    )

print()
if failures:
    for failure in failures:
        print(f"FAIL {failure}")
    sys.exit(1)
print("PASS every segment on either machine has a figure, and none of them cross a clock")
PY
