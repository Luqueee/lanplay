#!/usr/bin/env bash
# WHAT THIS GATE PROVES, in one sentence that names the property and not the mechanism.
#
# Then the paragraphs that a reader six months from now needs and cannot reconstruct:
# why this property matters, what the criteria below are derived from, and what this gate
# deliberately does NOT cover. That last part is the one most often left out and the one
# that stops somebody reading a pass as broader than it is.
#
# If the gate has an arm that must fail, say which and why. If it does not have one yet,
# say so here as well as in tools/gates.toml - a gate whose failure mode has never been
# observed is a gate nobody has grounds to trust, and writing that down is what keeps it
# from being forgotten.
#
# usage:
#   tools/NAME-gate.sh [seconds]

set -euo pipefail

SECONDS_TO_RUN="${1:-30}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
# Stamped and never cleared. Six minutes of measurement was lost once to a gate that
# emptied its output directory on startup, so re-running it to re-read a verdict deleted
# the verdict.
OUT="${OUT:-/tmp/NAME-gate/$(date +%Y%m%d-%H%M%S)}"

mkdir -p "$OUT"
echo "results   $OUT"

# Anything this gate starts, this gate ends, including on an interrupt. A process that
# survives holds a port, and the next thing to bind it fails for a reason that has
# nothing to do with it - a false failure indistinguishable from a real one.
cleanup() {
    pkill -f "the-helper-this-gate-spawns" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

cargo build --release -q -p THE-CRATE

# ---- arms ------------------------------------------------------------------
# One arm that must pass and one that must fail. If the failing arm is not available yet,
# leave the comment saying so rather than deleting the section.
#
# Seed every arm that injects a fault, so an arm that fails fails the same way twice.

run_arm() {
    local name="$1"
    shift
    "$REPO/target/release/THE-PROBE" --seconds "$SECONDS_TO_RUN" "$@" \
        >"$OUT/$name.out" 2>&1 || true
    echo "arm       $name done"
}

run_arm clean
# run_arm broken --the-fault-that-must-be-caught

# ---- verdict ---------------------------------------------------------------

python3 - "$OUT" "$SECONDS_TO_RUN" <<'PY'
import re
import sys

out, seconds = sys.argv[1], float(sys.argv[2])
body = open(f"{out}/clean.out").read()


def num(pattern):
    # Multiline, always. A pattern anchored with ^ and no re.M once read a run of 6001
    # packets as having captured none, and the gate failed a clean run.
    got = re.search(pattern, body, re.M)
    return int(got.group(1)) if got else None


failures = []
findings = []

# --- must not be zero: the evidence that the run happened at all ---
#
# This section is not optional and it goes first. Without it every check below passes
# hardest when nothing happened, which is the most common way a gate lies.
population = num(r"^events observed (\d+)$")
if not population:
    failures.append("nothing was observed, so every zero below is an absence and not a result")

# --- must be zero: the faults, each naming its population ---
#
# A zero is only meaningful against something that could have been non-zero. Name it.
faults = num(r"^faults (\d+)$")
if faults is None:
    failures.append("the fault count was not reported, which is the criterion")
elif population and faults:
    failures.append(f"{faults} faults over {population} events")

# --- bounds, each with its derivation written down ---
#
# A bound whose reason cannot be written is a bound nobody can review, and four of the
# five criterion defects in this repository's history would have been visible in the
# writing. State what the number comes from, not just what it is.
budget_us = 5000.0 / 10.0  # a tenth of the frame this encodes: see docs/testing.md
measured = num(r"^cost us p99 (\d+)$")
if measured is not None and measured > budget_us:
    failures.append(
        f"p99 {measured} us against a {budget_us:.0f} us budget, a tenth of the period - "
        "the thing being measured is in the latency path"
    )

# --- findings: measured, and not voting ---
#
# Above the verdict so they survive a failure. A failing arm does not make its
# measurement uninteresting.
rate = num(r"^rate (\d+)$")
if rate:
    findings.append(f"the rate was {rate}, which is what this gate exists to produce")

print()
for finding in findings:
    print(f"FINDING {finding}")
print()
if failures:
    for failure in failures:
        print(f"FAIL {failure}")
    sys.exit(1)
print("PASS the sentence at the top of this file, in the past tense and with its numbers")
PY
