#!/usr/bin/env bash
# Refuse a release whose lab evidence is older than the code it covers.
#
# CI in this repository cannot test the system: no NVENC engine, no virtual display
# driver, no second machine, no radio, no panel. So a green check says the code compiles
# and its pure logic holds, and nothing more. The real verification is a set of
# harnesses driven against real hardware, and their output lives under results/.
#
# Which leaves one gap that nothing else closes. A release built from a green check
# would quote those numbers in its notes as though they were current, when the code may
# have moved underneath them. This is the check that makes the un-CI-able verification a
# release requirement: if the newest commit touching a subsystem is later than the
# newest evidence recorded for it, the numbers describe a different program and the
# release stops.
#
# Coarse on purpose, for now. It compares a subsystem's source tree against a results
# directory, which catches the case that matters - code changed, nobody re-ran the
# gates - without pretending to know which gate covers which function. The finer
# version reads the `results` path from tools/gates.toml per gate; this one earns its
# place first.
#
# usage:
#   tools/check-evidence.sh

set -euo pipefail

# Subsystem source, and the evidence directory that stands for it. A subsystem with no
# entry here is a subsystem this check is silent about, which is why the list is
# asserted against the tree below rather than trusted.
SUBSYSTEMS=(
    "crates/audio-codec:results/audio"
    "windows/audio-capture:results/audio"
    "crates/transport:results/audio"
    "macos/input-capture:results/input-gate"
    "windows/input-inject:results/input-gate"
    "crates/input-protocol:results/input-fault"
    "macos/client:results/soak-1080p120"
    "windows/capture:results/soak-1080p120"
    "windows/encoder-nvenc:results/soak-1080p120"
)

# Every source tree named above has to exist, or the check is quietly covering less
# than it claims. This is the failure that makes a stale-evidence check useless: it
# passes because it looked at nothing.
missing=()
for entry in "${SUBSYSTEMS[@]}"; do
    src="${entry%%:*}"
    [ -d "$src" ] || missing+=("$src")
done
if [ ${#missing[@]} -gt 0 ]; then
    echo "these subsystems are named here but not in the tree, so the check covers less" >&2
    echo "than it claims: ${missing[*]}" >&2
    exit 2
fi

stale=0
echo "subsystem                     code committed        evidence recorded"
for entry in "${SUBSYSTEMS[@]}"; do
    src="${entry%%:*}"
    evidence="${entry##*:}"

    # The commit date of the last change to the source, not the file's mtime: a checkout
    # rewrites every mtime and the comparison would then always pass.
    code_at="$(git log -1 --format=%ct -- "$src" 2>/dev/null || echo 0)"

    if [ -d "$evidence" ]; then
        # And the commit date of the last change to the evidence, for the same reason.
        evidence_at="$(git log -1 --format=%ct -- "$evidence" 2>/dev/null || echo 0)"
    else
        evidence_at=0
    fi

    if [ "$evidence_at" = 0 ]; then
        printf "%-29s %-21s %s\n" "$src" "$(date -u -r "$code_at" '+%Y-%m-%d %H:%M' 2>/dev/null || echo '?')" "NONE"
        echo "  no evidence has ever been recorded under $evidence" >&2
        stale=$((stale + 1))
    elif [ "$code_at" -gt "$evidence_at" ]; then
        printf "%-29s %-21s %s\n" "$src" \
            "$(date -u -r "$code_at" '+%Y-%m-%d %H:%M' 2>/dev/null || echo '?')" \
            "$(date -u -r "$evidence_at" '+%Y-%m-%d %H:%M' 2>/dev/null || echo '?')  STALE"
        stale=$((stale + 1))
    else
        printf "%-29s %-21s %s\n" "$src" \
            "$(date -u -r "$code_at" '+%Y-%m-%d %H:%M' 2>/dev/null || echo '?')" \
            "$(date -u -r "$evidence_at" '+%Y-%m-%d %H:%M' 2>/dev/null || echo '?')"
    fi
done

echo
if [ "$stale" -gt 0 ]; then
    cat >&2 <<'WHY'
This release is refused. For each subsystem marked STALE or NONE above, the code has
moved since anybody last ran the harnesses that verify it, so the numbers a release
would quote describe a different program.

Run the gates that cover it - `cargo run -p xtask -- gates --runnable` says which can
run where you are - commit their output under results/, and tag again.
WHY
    exit 1
fi
echo "every subsystem's evidence is at least as new as the code it covers"
