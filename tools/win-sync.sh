#!/usr/bin/env bash
# Mirrors the workspace onto the Windows host.
#
# The host only ever builds a few crates, but Cargo loads the manifest of
# every workspace member before it builds anything, so a member that exists
# here and not there fails the build with a path error that names the wrong
# problem. Adding a crate on this machine has broken the host build three
# times that way.
#
# Every member is mirrored whole, including the macOS ones the host will never
# compile. Sending only their manifests was tried and does not work: Cargo
# requires each member to declare a target, so a manifest with no sources
# beside it fails to load. Writing stub sources to satisfy that would put a
# lie on disk; inert real sources are cheaper and true.
#
# usage:
#   tools/win-sync.sh

set -euo pipefail

HOST="${WIN_HOST:-windows}"
REPO="$(cd "$(dirname "$0")/.." && pwd)"
REMOTE='C:/Users/luque/lanplay-rs'

members() {
    python3 - "$REPO/Cargo.toml" <<'PY'
import re, sys
text = open(sys.argv[1]).read()
block = re.search(r"^members\s*=\s*\[(.*?)\]", text, re.S | re.M).group(1)
for line in re.findall(r'"([^"]+)"', block):
    print(line)
PY
}

# One connection for the whole directory tree. Doing this per crate spends
# more time in SSH handshakes than in copying.
dirs=""
for member in $(members); do
    dirs="$dirs,'$REMOTE/$member'"
done
ssh -n -o BatchMode=yes "$HOST" \
    "powershell -NoProfile -Command \"New-Item -ItemType Directory -Force ${dirs#,} | Out-Null\"" \
    >/dev/null

scp -q "$REPO/Cargo.toml" "$HOST:$REMOTE/Cargo.toml"
copied=0
for member in $(members); do
    scp -q "$REPO/$member/Cargo.toml" "$HOST:$REMOTE/$member/Cargo.toml"
    scp -q -r "$REPO/$member/src" "$HOST:$REMOTE/$member/"
    copied=$((copied + 1))
done

echo "synced    $copied workspace members"
