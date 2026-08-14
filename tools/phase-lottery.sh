#!/usr/bin/env bash
# Is the presentation wait an average, or a draw a session is stuck with?
#
# The wait between a frame being decoded and the display next accepting it
# averages half a refresh period over a long soak, and it is easy to read that as
# a cost that averages out. One run says otherwise: over 56 s its phase stayed
# inside 1.00 to 2.04 ms and drifted 0.016 ms per second, which crosses an
# 8.33 ms period only every 524 s. If that holds, a session does not pay the
# average, it pays whatever it drew, for as long as it lasts.
#
# The difference matters more than the average does. A cost that averages out is
# worth a few milliseconds of tuning; a fixed cost drawn at random per session,
# anywhere from nearly nothing to a whole refresh period, is a lottery a player
# can lose for an entire evening. It also explains a mistake made earlier here: a
# two-arm comparison credited alignment with a 3 ms improvement when the two arms
# had simply drawn different phases.
#
# So this measures both halves and can refute either. Several short sessions, each
# long enough to measure a phase and far too short to sweep one:
#
# The first version of this asked the wrong question. It checked that the phase
# barely moves inside a session and covers a lot of the period between them, and
# passed - but a single free-running clock drifting slowly produces exactly that
# pair of numbers, and so does a fresh independent draw per session. The data told
# the difference and the criterion could not: six sessions came out strictly
# ordered, 0.98, 2.21, 3.26, 4.93, 5.77, 6.84 ms, which independent draws manage
# once in 720 times.
#
# So the test is quantitative and uses the clock. Every trace entry is stamped on
# this machine's monotonic clock, which keeps counting across processes, so the gap
# between one session's last sample and the next session's first is measurable. If
# the phase moved by that gap times the drift measured inside the sessions, the
# phase is one continuous clock and tearing a session down does nothing to it. If
# the jumps are unrelated to the elapsed time, the phase is re-rolled, and
# restarting until the draw is good is a real if inelegant lever.
#
# usage:
#   tools/phase-lottery.sh [sessions] [seconds-each]

set -euo pipefail

SESSIONS="${1:-6}"
SECONDS_EACH="${2:-45}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
# Stamped, and never cleared. Six sessions cost six minutes, and re-running this
# to re-read a verdict already destroyed one set: the old version cleared its
# output directory on startup, so launching it again to look at the last answer
# deleted the last answer. A run that cannot be repeated cheaply must not put its
# own results inside the blast radius of being repeated.
OUT="${OUT:-/tmp/phase-lottery/$(date +%Y%m%d-%H%M%S)}"
mkdir -p "$OUT"
echo "results   $OUT"

for session in $(seq 1 "$SESSIONS"); do
    # Observing, never acting: this is about the phase the pipeline lands on by
    # itself. A run that corrected its own phase would be measuring the corrector.
    IFACE=en0 BITRATE=40 MTU=1200 PHASE_ALIGN=observe \
        REPORT="$OUT/$session.json" "$REPO/tools/e2e-gate.sh" "$SECONDS_EACH" \
        >"$OUT/$session.log" 2>&1 || true
    wait_line="$(grep -E "presentation wait p50" "$OUT/$session.log" | tail -1 || true)"
    echo "session $session  ${wait_line:-no figures}"
    # A gap between sessions, so consecutive draws are not the same draw. The
    # phase moves 0.016 ms per second, so a few seconds changes nothing and the
    # independence comes from the session being torn down and rebuilt, not from
    # waiting.
    sleep 3
done

python3 - "$OUT" "$SESSIONS" <<'PY'
import json
import sys

out, sessions = sys.argv[1], int(sys.argv[2])
PERIOD_MS = 1000.0 / 120.0

rows = []
for session in range(1, sessions + 1):
    try:
        report = json.load(open(f"{out}/{session}.json"))
    except (OSError, ValueError):
        continue
    phase = report.get("phase") or {}
    trace = phase.get("trace") or []
    if len(trace) < 8:
        continue
    measured = [entry["phase_ms"] for entry in trace]
    rows.append(
        {
            "session": session,
            "first": measured[0],
            "last": measured[-1],
            "low": min(measured),
            "high": max(measured),
            "samples": len(measured),
            "start_ns": trace[0]["at_ns"],
            "end_ns": trace[-1]["at_ns"],
            "span_s": (trace[-1]["at_ns"] - trace[0]["at_ns"]) / 1e9,
        }
    )

print(f"\none period {PERIOD_MS:.2f} ms, {len(rows)} of {sessions} sessions measured a phase\n")
for row in rows:
    print(
        f"  session {row['session']}  phase {row['first']:>5.2f} -> {row['last']:>5.2f} ms"
        f"   range {row['high'] - row['low']:>4.2f} ms over {row['samples']:>3} samples"
    )

if len(rows) < 4:
    print("\nFAIL fewer than four sessions measured a phase, so neither half is answerable")
    sys.exit(1)

# Within a session: the largest range any one of them covered, and the drift rate
# they agree on, which is what predicts the jumps between them.
worst_within = max(row["high"] - row["low"] for row in rows)
rates = [
    (row["last"] - row["first"]) / row["span_s"] for row in rows if row["span_s"] > 1.0
]
drift = sum(rates) / len(rates)

print(f"\n  within a session, the widest range {worst_within:.2f} ms")
print(f"  the drift they agree on            {drift:+.4f} ms/s, a period every {abs(PERIOD_MS / drift):.0f} s")

# The question that decides whether a lever exists. Between one session's last
# sample and the next session's first the client process was torn down and rebuilt,
# and the monotonic clock kept counting through it. So the elapsed gap is known,
# and the drift predicts what the phase should have done over it. Nothing else in
# this experiment can tell a continuous clock from a fresh draw.
print("\n  session boundaries, predicted against measured")
residuals = []
for before, after in zip(rows, rows[1:]):
    elapsed = (after["start_ns"] - before["end_ns"]) / 1e9
    predicted = drift * elapsed
    # The short way round, because a boundary can straddle the wrap.
    measured = (after["first"] - before["last"] + PERIOD_MS / 2.0) % PERIOD_MS - PERIOD_MS / 2.0
    residuals.append(measured - predicted)
    print(
        f"    {before['session']} -> {after['session']}   {elapsed:>5.1f} s apart"
        f"   predicted {predicted:>+5.2f} ms   measured {measured:>+5.2f} ms"
        f"   off by {measured - predicted:>+5.2f} ms"
    )

worst_residual = max(abs(residual) for residual in residuals)
print(f"\n  worst disagreement {worst_residual:.2f} ms against a {PERIOD_MS:.2f} ms period")

failures = []
if worst_within > PERIOD_MS / 4.0:
    failures.append(
        f"a session's phase moved {worst_within:.2f} ms, more than a quarter of a period, so "
        "there is no single phase per session to reason about"
    )

print()
if failures:
    for failure in failures:
        print(f"FAIL {failure}")
    sys.exit(1)

# Not a pass or a fail: the two outcomes are both answers, and only one of them
# leaves a lever standing. Calling either a failure would be the harness having an
# opinion about what the machine ought to do.
if worst_residual < PERIOD_MS / 8.0:
    print(
        f"CONTINUOUS every boundary is explained by elapsed time and drift to within "
        f"{worst_residual:.2f} ms.\n"
        "           Tearing a session down does not re-roll the phase, so restarting until\n"
        "           the draw is good is not a lever. The phase is one free-running clock."
    )
else:
    print(
        f"RE-ROLLED a boundary moved the phase {worst_residual:.2f} ms more than elapsed time "
        "explains.\n"
        "           The phase is chosen afresh, so re-establishing a session until the draw\n"
        "           is acceptable is a real lever, and the only one left standing."
    )
PY
