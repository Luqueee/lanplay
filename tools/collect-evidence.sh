#!/usr/bin/env bash
# Assemble the lab evidence into release notes.
#
# The numbers themselves, not a summary of them. Whoever installs a build should be able
# to read what was actually measured, on what hardware and when, rather than a sentence
# claiming it was fine. That matters more here than in most projects because CI cannot
# test this system at all - no NVENC engine, no virtual display driver, no second
# machine, no radio, no panel - so the only verification that exists is what a person
# ran against real hardware and committed under results/.
#
# It also states what was NOT measured. A release note listing four passing gates reads
# as a system verified; the same note saying which nine of eighteen harnesses have never
# had a negative control, and which subsystems have no evidence at all, reads as what it
# is. The first is how a green check becomes a claim nobody made.
#
# usage:
#   tools/collect-evidence.sh <output.md>

set -euo pipefail

OUT="${1:?usage: collect-evidence.sh <output.md>}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"

{
    printf '# %s\n\n' "${GITHUB_REF_NAME:-$(git -C "$REPO" describe --tags --always 2>/dev/null || echo local)}"
    printf 'Built from %s.\n\n' "$(git -C "$REPO" rev-parse --short HEAD)"

    cat <<'PREAMBLE'
## What was verified, and by what

Continuous integration proves this code compiles and that its pure logic holds. It
cannot prove the system works: the behaviour that matters needs an encoder, a virtual
display driver, two machines, a radio and a panel, and no hosted runner has any of
them. Everything below was measured on real hardware and committed to the repository.

PREAMBLE

    printf '## Evidence recorded\n\n'
    if [ -d "$REPO/results" ]; then
        # Ordered by what the tree says rather than by mtime: a checkout rewrites every
        # mtime, and a release note ordered by them would claim a freshness it invented.
        for dir in "$REPO"/results/*/; do
            [ -d "$dir" ] || continue
            name="$(basename "$dir")"
            at="$(git -C "$REPO" log -1 --format='%ad' --date=short -- "results/$name" 2>/dev/null || echo unknown)"
            files="$(find "$dir" -type f | wc -l | tr -d ' ')"
            printf -- '- **%s** — %s files, last recorded %s\n' "$name" "$files" "$at"
        done
    else
        printf 'None. This release has no lab evidence at all.\n'
    fi
    printf '\n'

    printf '## What was not verified\n\n'
    if command -v cargo >/dev/null 2>&1 && [ -f "$REPO/tools/gates.toml" ]; then
        # Read from the index rather than restated here, so that a gate gaining a
        # negative control disappears from this list without anybody editing it.
        cargo run -q -p xtask --manifest-path "$REPO/Cargo.toml" -- gates --debt 2>/dev/null ||
            printf 'The gate index could not be read, so this section is empty rather than complete.\n'
    else
        printf 'The gate index could not be read, so this section is empty rather than complete.\n'
    fi
    printf '\n'

    cat <<'CLOSING'
## Reading this honestly

A harness with no negative control has never been observed to fail. It may still be
right, and it is not evidence of the same weight as one whose failure mode has been
seen. The list above is a debt, not a disclaimer.

Any subsystem absent from the evidence section has no measurement in this release at
all.
CLOSING
} >"$OUT"

echo "wrote $OUT"
