#!/usr/bin/env bash
set -euo pipefail

REPO="$(cd "$(dirname "$0")/.." && pwd)"
HOST_IP="$(ssh -G windows | awk '/^hostname /{print $2}')"
OUT="${OUT:-/tmp/gamepad-fault}"
PORT=5006
LOCAL_IP="$(ipconfig getifaddr en0)"
SECONDS="${1:-10}"
RELAY="$LOCAL_IP:5106"
mkdir -p "$OUT"
cargo build --release -q -p lanplay-udp-fault -p lanplay-input-inject
"$REPO/tools/win-sync.sh" >/dev/null
"$REPO/tools/win-ssh.sh" 'cd C:\Users\luque\lanplay-rs && cargo build --release -q -p lanplay-input-inject && dotnet build -c Release windows\hidmaestro-bridge\HidMaestroBridge.csproj -p:HidMaestroRoot=C:\Users\luque\HIDMaestro' >/dev/null
"$REPO/tools/win-ssh.sh" 'del /q C:\Users\luque\gamepad-fault.log 2>nul || exit /b 0'
WIN_TIMEOUT=120 "$REPO/tools/win-session.sh" 'C:\Users\luque\gamepad-fault.log' "target\\release\\gamepad_inject_probe.exe --seconds $((SECONDS + 30)) --bridge C:\\Users\\luque\\lanplay-rs\\windows\\hidmaestro-bridge\\bin\\Release\\net10.0-windows10.0.26100.0\\win-x64\\HidMaestroBridge.exe" >"$OUT/host.out" 2>&1 &
host=$!
"$REPO/target/release/udp-fault" --forward "$HOST_IP:$PORT" --listen "$RELAY" --loss 5 --duplicate 2 --reorder 3 --stall-ms 50 --stall-every-ms 1000 --seed 42 >"$OUT/net.out" 2>&1 &
relay=$!
for _ in $(seq 1 80); do
    "$REPO/tools/win-ssh.sh" 'type C:\Users\luque\gamepad-fault.log' 2>/dev/null | grep -q '^ready$' && break
    sleep 0.5
done
"$REPO/tools/win-ssh.sh" 'type C:\Users\luque\gamepad-fault.log' | grep -qx 'ready'
trap 'kill "$relay" 2>/dev/null || true' EXIT
sleep 1
python3 - "$LOCAL_IP" <<'PY'
import socket, struct, sys
host=sys.argv[1]; s=socket.socket(socket.AF_INET,socket.SOCK_DGRAM)
def p(kind, body, seq): return bytes([2,kind,0,0])+struct.pack('>IIQ',1,seq,0)+body
for seq in range(1,121):
    if seq == 1: body=struct.pack('>QBI',1,0,1); kind=9
    elif seq == 120: body=struct.pack('>QBI',2,0,1); kind=10
    else: body=struct.pack('>IBIHBhhhhHH',1,0,seq-1,1,0,32767 if seq%2 else -32767,0,0,0,0,0); kind=11
    s.sendto(p(kind,body,seq),(host,5106))
PY
wait "$host"
cat "$OUT/host.out"
grep -Eq 'udp [1-9][0-9]* decode 0 wrong-session 0 attach [1-9][0-9]* state [1-9][0-9]* detach [1-9][0-9]* stale [0-9]+ neutral [1-9][0-9]*' "$OUT/host.out"
