#!/usr/bin/env bash
# Runs a command on the Windows host, recovering the local ssh-agent first if
# it has wedged.
#
# The lab host is reached with a passphrase-protected key, so only the agent
# holds it usable. The launchd ssh-agent on this Mac wedges into a spinning
# loop every few tens of minutes: it keeps its socket open, stops answering, and
# ssh then blocks on it before ever offering a key. From the outside that is
# indistinguishable from the host being down, and it cost most of an afternoon
# and three wrong diagnoses - a rate-limited sshd, a reinstalled server, a bad
# file ACL - before the agent turned out to be the common factor.
#
# The recovery is mechanical: kill the spinning agent, let launchd start a
# fresh one, and reload the key from the keychain where the passphrase lives.
# Nothing here needs a human, which is the whole point.
#
# usage:
#   tools/win-ssh.sh <command...>
#   tools/win-ssh.sh --check          just prove the host answers

set -euo pipefail

HOST="${WIN_HOST:-windows}"

# `hostname` and not `true`: the host's default shell is cmd.exe, where `true`
# does not exist, so the probe failed every time and the recovery then killed a
# perfectly healthy agent. A liveness check that cannot succeed is worse than no
# check at all.
#
# Six seconds is generous for a LAN round trip and short enough that a wedged
# agent is noticed rather than waited on.
answers() {
    timeout 6 ssh -n -o BatchMode=yes -o ConnectTimeout=4 "$HOST" hostname >/dev/null 2>&1
}

recover() {
    local agent
    agent="$(pgrep -f 'ssh-agent -l' | head -1 || true)"
    if [ -n "$agent" ]; then
        echo "win-ssh: agent $agent is not answering, restarting it" >&2
        kill -9 "$agent" 2>/dev/null || true
        sleep 2
    fi
    # The keychain is what makes this unattended. Without the passphrase stored
    # there, a fresh agent has nothing to load and only a human can fix it.
    if ! timeout 10 ssh-add --apple-load-keychain >/dev/null 2>&1; then
        echo "win-ssh: no key in the keychain; run ssh-add --apple-use-keychain ~/.ssh/id_ed25519" >&2
        return 1
    fi
}

if ! answers; then
    recover || exit 1
    if ! answers; then
        echo "win-ssh: the host still does not answer after recovering the agent" >&2
        exit 1
    fi
fi

if [ "${1:-}" = "--check" ]; then
    ssh -n -o BatchMode=yes "$HOST" hostname
    exit 0
fi

exec ssh -n -o BatchMode=yes "$HOST" "$@"
