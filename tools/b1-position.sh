#!/usr/bin/env bash
# B1: does moving the Mac closer to the access point change the link?
#
# One position per invocation, labelled, so a session is a sequence of
# labelled tandas rather than one long sweep: the Mac has to be physically
# moved between them and no script can do that.
#
# The order matters and is the caller's job: baseline in the normal position,
# then close to the access point, then back to normal. The access point has
# already been seen changing channel between sittings on its own, so a
# comparison of one tanda against an older one cannot separate distance from
# whatever the radio did in between. The return tanda is what makes the
# middle one attributable.
#
# Every run is `--link-only`: no window, no display link, no AppKit. Nothing
# in the result depends on what the Mac's screen was doing, which is the
# entire reason this experiment can be run on a machine somebody is using.
#
# usage:
#   tools/b1-position.sh <label> [seconds] [repeats]
#
#   IFACE=en0  BITRATE=40  MTU=1200  OUT=/tmp/b1

set -euo pipefail

LABEL="${1:?usage: b1-position.sh <label> [seconds] [repeats]}"
SECONDS_TO_RUN="${2:-60}"
REPEATS="${3:-3}"
IFACE="${IFACE:-en0}"
BITRATE="${BITRATE:-40}"
MTU="${MTU:-1200}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/b1}"

mkdir -p "$OUT"
echo "b1        position \"$LABEL\", $REPEATS runs of ${SECONDS_TO_RUN}s"
echo "settings  ${BITRATE} Mbps, MTU $MTU, best-effort, link-only on $IFACE"
echo "output    $OUT"

# The association has to be settled before the first run, and a fixed sleep
# is the wrong instrument: it is either too short after a move or wasted
# after none. Wait for the radio to report the same channel and a stable
# RSSI twice in a row instead.
settle() {
    local previous="" current="" stable=0 attempt
    for attempt in $(seq 1 30); do
        current="$("$REPO/target/release/radio-sample" 1 100 2>/dev/null |
            tail -1 | cut -d, -f6,7,8)"
        if [ -n "$current" ] && [ "$current" = "$previous" ]; then
            stable=$((stable + 1))
            [ "$stable" -ge 2 ] && {
                echo "radio     settled on $current after ${attempt}s"
                return 0
            }
        else
            stable=0
        fi
        previous="$current"
        sleep 1
    done
    echo "radio     did not settle in 30 s; last seen $current" >&2
}

settle

for rep in $(seq 1 "$REPEATS"); do
    printf '\n[%d/%d] %s\n' "$rep" "$REPEATS" "$LABEL"
    # Never system_profiler: its report lists other networks, which it can
    # only fill by scanning, and a scan takes the radio off channel. Doing
    # that once a second turned an 11 ms p99 into a 133 ms one - the
    # instrument manufactured the bunching this experiment looks for.
    "$REPO/target/release/radio-sample" $((SECONDS_TO_RUN + 30)) 1000 \
        >"$OUT/$LABEL-r$rep.wifi.csv" 2>/dev/null &
    sampler=$!
    IFACE="$IFACE" BITRATE="$BITRATE" MTU="$MTU" LINK_ONLY=1 QUIET=1 \
        REPORT="$OUT/$LABEL-r$rep.json" \
        "$REPO/tools/e2e-gate.sh" "$SECONDS_TO_RUN" \
        >"$OUT/$LABEL-r$rep.log" 2>&1 </dev/null ||
        echo "      run returned $? (a failing gate is a data point, not an abort)"
    kill "$sampler" 2>/dev/null || true
    grep -oE "au delivery.*" "$OUT/$LABEL-r$rep.log" | tail -1 | sed 's/^/      /' || true
    sleep 5
done

echo
python3 "$REPO/tools/link-report.py" "$OUT"
