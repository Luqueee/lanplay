#!/usr/bin/env bash
# Make a CI failure readable from a machine with no token.
#
# The run logs need authentication. This project is developed on two machines that have
# none, so the last two red runs were diagnosed by reading source and guessing, and one of
# those guesses was wrong for four hours. Annotations and the job summary are served by the
# public API, so what this prints can be read from anywhere:
#
#   curl -s .../check-runs/<id>/annotations
#
# It prints, it does not judge. The exit code stays the one cargo gave, because a reporter
# that decided anything would be a second opinion about whether the run passed.

set -uo pipefail

log=${1:?usage: ci-annotate.sh <logfile> <exit-code> <label>}
code=${2:?}
label=${3:?}

if [ "$code" = 0 ]; then
  exit 0
fi

# Cargo says the same thing twice: once as it happens and once in the summary at the end.
# The summary is the useful half - it names every failing test in one place - and the panic
# is what says why, so both are kept and nothing between them is.
#
# Compile errors are matched too, because exit 1 without a panic is a build failure and that
# is a different diagnosis from a test that ran and disagreed. Distinguishing them from the
# outside was the whole of one diagnosis here, and it took an exit code and a guess.
#
# Every pattern below was fixed against a real cargo failure rather than written from memory,
# after a first version emitted a test name and no reason at all. Three things it got wrong,
# each of which silenced the line that mattered: rust puts the thread id between the name and
# `panicked`, so anchoring on `^thread '...' panicked` matches nothing; the panic message is
# the line after that one and carries no prefix to match on, so it is taken by position; and
# `left:`/`right:` are indented under the assertion they belong to.
# And the log is coloured. The workflow sets CARGO_TERM_COLOR=always, so every line clippy
# reports begins with an escape sequence and every pattern anchored at `^` matches nothing:
# the windows job's real lint arrived as "printed nothing this reporter recognises" with the
# lint visible in the tail underneath. Colour is stripped once, here, rather than each
# pattern being taught to skip it.
plain() {
  sed -E $'s/\033\\[[0-9;]*[A-Za-z]//g; s/\r$//' "$log"
}

extract() {
  plain | grep -E -A1 \
    -e 'panicked at' \
    | grep -vE '^(--|note: run with)'
  plain | grep -E \
    -e '^error(\[E[0-9]+\])?:' \
    -e '^error: could not compile' \
    -e '^ *--> ' \
    -e '^test .* \.\.\. FAILED$' \
    -e '^failures:$' \
    -e '^ +[a-z_]+::[a-z_:]+$' \
    -e '^ *(assertion|left:|right:)' \
    -e '^test result: FAILED'
}

# Cargo prints each failure twice, and the runner shows only the first ten annotations per
# step, so a duplicate costs a line that would have said something. Order is kept: the panic
# and its reason come first, because that is what a reader needs before the roll call.
lines=$(extract | awk '!seen[$0]++' | head -20)

if [ -z "$lines" ]; then
  # An unrecognised failure is worse than a recognised one, so say so with the tail rather
  # than printing nothing and leaving the reader where the last one left them.
  lines=$(tail -20 "$log")
  echo "::error title=$label::exited $code and printed nothing this reporter recognises; last 20 lines follow"
fi

while IFS= read -r line; do
  # A literal newline or carriage return would end the workflow command early and swallow
  # the rest, and %-escapes are how the runner takes them.
  printf '::error title=%s::%s\n' "$label" "${line//$'\r'/}"
done <<<"$lines"

if [ -n "${GITHUB_STEP_SUMMARY:-}" ]; then
  {
    printf '## %s failed, exit %s\n\n```\n' "$label" "$code"
    printf '%s\n' "$lines"
    printf '```\n'
  } >>"$GITHUB_STEP_SUMMARY"
fi

exit "$code"
