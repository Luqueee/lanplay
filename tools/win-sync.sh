#!/usr/bin/env bash
# Mirrors the workspace onto the Windows host over one connection.
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
# beside it fails to load. Writing stub sources to satisfy that would put a lie
# on disk; inert real sources are cheaper and true.
#
# One tar stream through one ssh, and this is not a micro-optimisation. The
# first version ran two scp calls per member, which is around forty-five
# handshakes in a few seconds; Windows sshd throttles new connections and
# stopped spawning shells at all, taking the host out of the lab until it
# recovered. A single stream cannot do that.
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

paths=(Cargo.toml)
count=0
for member in $(members); do
    paths+=("$member/Cargo.toml")
    [ -d "$REPO/$member/src" ] && paths+=("$member/src")
    count=$((count + 1))
done

# bsdtar ships with Windows as tar.exe and reads the stream on stdin, so the
# whole tree crosses in one direction with no per-file round trip. Extracting
# creates the directories, which is why none are made in advance.
tar -cz -C "$REPO" "${paths[@]}" |
    ssh -o BatchMode=yes "$HOST" "tar -xzf - -C $REMOTE" ||
    {
        echo "sync failed; the host may be refusing connections" >&2
        exit 1
    }

echo "synced    $count workspace members in one stream"
