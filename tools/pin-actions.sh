#!/usr/bin/env bash
# Rewrite every `uses:` in the workflows from a version tag to a commit SHA.
#
# A tag is mutable. `actions/checkout@v4` runs whatever that tag points at when the
# workflow runs, which means the code executing in a job holding an id-token and write
# access to this repository can change without anything here changing. Pinning to a SHA
# is the difference between depending on a version and depending on a promise.
#
# This is a script rather than a note in a document because the workflows were written
# on a machine with no access to the GitHub API, so the SHAs could not be resolved
# there. Inventing them was not an option, and leaving a note asking somebody to
# remember is the same thing as not doing it. `xtask actions --check` fails while any
# `uses:` remains unpinned, so the enforcement is mechanical and this only has to be run
# once per action bump.
#
# Needs `gh` authenticated. Comments recording the original tag are added beside each
# SHA, because a bare forty-character hash tells a reader nothing about what it is or
# whether it is behind.
#
# usage:
#   tools/pin-actions.sh [--dry-run]

set -euo pipefail

DRY_RUN=no
[ "${1:-}" = "--dry-run" ] && DRY_RUN=yes

command -v gh >/dev/null 2>&1 || {
    echo "gh is not installed; it is what resolves a tag to a commit" >&2
    exit 127
}
gh auth status >/dev/null 2>&1 || {
    echo "gh is not authenticated, so no tag can be resolved. Run: gh auth login" >&2
    exit 1
}

changed=0
for workflow in .github/workflows/*.yml; do
    # Only unpinned entries: a forty-hex SHA is already done, and rewriting it would
    # resolve a tag this file no longer mentions.
    while IFS= read -r line; do
        action="${line#*uses: }"
        action="${action%% *}"
        repo="${action%@*}"
        ref="${action#*@}"

        case "$ref" in
        [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]*)
            [ ${#ref} -eq 40 ] && continue
            ;;
        esac
        # A local or docker action has no commit to resolve.
        case "$repo" in ./* | docker://*) continue ;; esac

        sha="$(gh api "repos/${repo}/commits/${ref}" --jq .sha 2>/dev/null || true)"
        if [ -z "$sha" ]; then
            echo "could not resolve ${repo}@${ref}" >&2
            exit 1
        fi

        echo "  ${repo}@${ref} -> ${sha}"
        if [ "$DRY_RUN" = no ]; then
            # The tag is kept as a trailing comment: a bare hash says nothing about what
            # it is, and a reader deciding whether to bump needs to know where it came
            # from.
            python3 - "$workflow" "$repo" "$ref" "$sha" <<'PY'
import re
import sys

path, repo, ref, sha = sys.argv[1:5]
with open(path) as handle:
    body = handle.read()
pattern = re.compile(rf"(uses: {re.escape(repo)}@){re.escape(ref)}(?![\w.-])")
body = pattern.sub(rf"\g<1>{sha} # {ref}", body)
with open(path, "w") as handle:
    handle.write(body)
PY
        fi
        changed=$((changed + 1))
    done < <(grep -E '^\s*(- )?uses: ' "$workflow" || true)
done

echo
if [ "$changed" -eq 0 ]; then
    echo "every action is already pinned to a commit"
elif [ "$DRY_RUN" = yes ]; then
    echo "$changed would be pinned; rerun without --dry-run"
else
    echo "$changed pinned"
fi
