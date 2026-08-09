#!/usr/bin/env bash
# Runs a command in the Windows interactive session and brings its output back.
#
# SSH into Windows lands in session 0, which has no display devices: user32
# enumeration returns nothing there, so anything that captures, presents or
# changes a display mode finds nothing to work with. The only bridge from an
# SSH session to the logged-on desktop is a scheduled task created with /IT,
# and a process launched that way has no stdout we can read. So the contract
# here is: the command must write what it has to say to a file, and this script
# waits for the task to finish and prints that file.
#
# usage:
#   tools/win-session.sh <log-file-on-windows> <command...>
#
# The command runs with the repo as its working directory. Paths inside it are
# Windows paths and must be written as such. The exit code is the command's
# own, so a caller can branch on it.

set -euo pipefail

HOST="${WIN_HOST:-windows}"
REPO='C:\Users\luque\lanplay-rs'
TASK="${WIN_TASK:-lanplay-session}"
WRAPPER='C:\Users\luque\lanplay-run.cmd'

if [ $# -lt 2 ]; then
    echo "usage: $0 <log-file-on-windows> <command...>" >&2
    exit 64
fi

LOG="$1"
shift
COMMAND="$*"

# The task's own exit code is not reported by schtasks until the run finishes,
# and "finished" is only observable by polling the task state. The sentinel
# makes completion unambiguous without depending on that: the wrapper writes it
# last, so its presence means the command returned rather than the task merely
# having been launched.
SENTINEL="${LOG}.done"

# The wrapper is written here and copied, never assembled with nested `echo`
# through ssh. That route expands `%ERRORLEVEL%` while generating the file, so
# the sentinel records a literal 0 and every run looks successful whatever it
# did.
LOCAL_WRAPPER="$(mktemp -t lanplay-run)"
trap 'rm -f "$LOCAL_WRAPPER"' EXIT

{
    printf '@echo off\r\n'
    printf 'cd /d %s\r\n' "$REPO"
    printf '%s > "%s" 2>&1\r\n' "$COMMAND" "$LOG"
    printf 'echo %%ERRORLEVEL%% > "%s"\r\n' "$SENTINEL"
} > "$LOCAL_WRAPPER"

ssh -o BatchMode=yes "$HOST" "del /q \"$LOG\" \"$SENTINEL\" 2>nul" >/dev/null 2>&1 || true
scp -q "$LOCAL_WRAPPER" "$HOST:$(printf '%s' "$WRAPPER" | tr '\\' '/')"

ssh -o BatchMode=yes "$HOST" "schtasks /create /tn $TASK /tr \"$WRAPPER\" /sc once /st 23:59 /ru luque /it /f" >/dev/null
ssh -o BatchMode=yes "$HOST" "schtasks /run /tn $TASK" >/dev/null

# Poll for the sentinel rather than the task state: a task can report Ready
# while its child is still flushing, and the sentinel is written after the
# redirect closes.
DEADLINE=$(( $(date +%s) + ${WIN_TIMEOUT:-1800} ))
while :; do
    if ssh -o BatchMode=yes "$HOST" "if exist \"$SENTINEL\" (exit 0) else (exit 1)"; then
        break
    fi
    if [ "$(date +%s)" -ge "$DEADLINE" ]; then
        echo "win-session: timed out waiting for $SENTINEL" >&2
        ssh -o BatchMode=yes "$HOST" "schtasks /end /tn $TASK" >/dev/null 2>&1 || true
        ssh -o BatchMode=yes "$HOST" "type \"$LOG\"" 2>/dev/null || true
        exit 124
    fi
    sleep 2
done

ssh -o BatchMode=yes "$HOST" "type \"$LOG\"" || true
CODE=$(ssh -o BatchMode=yes "$HOST" "type \"$SENTINEL\"" | tr -d ' \r\n')
exit "${CODE:-0}"
