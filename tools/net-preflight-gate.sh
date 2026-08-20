#!/usr/bin/env bash
# N2: measure the link with the product's own traffic before a session asks it
# for anything, and prove the measurement can come out both ways.
#
# The probe is five seconds of `net-bench send --pacer burst` over the real
# radio, which is a real H.264 fixture at 120 fps in real datagrams handed to
# the kernel one access unit at a time. That last part is why the generator is
# borrowed rather than written: this product presents the air with some forty
# datagrams back to back, forty times a second, and a smooth stream of the same
# bitrate would come back optimistic about a link nobody is going to use.
#
# What the probe writes is a description. It grades nothing, and a five-second
# description read as a ten-minute prediction is the most confident wrong number
# a system like this can hold - `results/audio/e2e-clean/radio-trace-full.csv`
# spread 4 dB in its first thirty seconds and 11 dB across the session it
# started, with the negotiated rate running 103 to 816 Mbps. So N2 picks a
# starting point and the monitor does the watching.
#
# ---------------------------------------------------------------------------
# Six judgements over three arms
# ---------------------------------------------------------------------------
#
#   refusal          no sender at all             must be REFUSED
#   faults           the radio through udp-fault  must PASS its own criteria
#   faults-as-clean  the same numbers, judged     must FAIL
#                    by the clean arm's criteria
#   clean            the radio as it is           must PASS its own criteria
#   clean-as-faults  the same numbers, judged     must FAIL
#                    by the fault arm's criteria
#   separation       the two arms' crossing rates must differ by more than this
#                                                link's own between-arm variance
#
# The crossings are what make this a gate rather than a report. The fault arm's
# own criteria are must-not-be-zeros - the injected loss reached the path, the
# bunching was seen - and an arm that passes those has not failed anything, so
# it cannot be the control `tools/gates.toml` asks every gate for. Judged by the
# clean arm's criteria, the same numbers must be refused: that is the probe shown
# detecting a link it should not call good. Judged the other way, the clean arm
# must fail to show an injected loss it never had: that is the probe shown not
# crying wolf. `tools/audio-rtp-gate.sh` reached the same arrangement for the
# same reason, and the arm it judged against a threshold of its own had already
# passed once on the harness being broken.
#
# The refusal arm is first and needs no hardware. A probe that received nothing
# must refuse rather than report a clean link, because zero datagrams lost out of
# zero sent is the most common way an instrument in this project has lied, and an
# instrument whose refusal has never been seen to fire cannot be trusted to
# refuse the arms that cost radio time.
#
# And the sixth, which is not about either arm. This link produced 162 threshold
# crossings a minute of variation between N1's nine 90 s arms with nothing
# running at all, and its delivery p99 spanned 11.93 to 17.79 ms across them. A
# probe of five seconds is therefore one draw from a wide distribution, so two
# arms that differ by less than that spread are two draws and not a comparison.
# The gate refuses in that case rather than failing: nobody was in a position to
# ask the question, which is a different answer from having asked it and been
# told no. It is also why the report this probe writes carries no adjective.
#
# ---------------------------------------------------------------------------
# What the relay is told to do, and why those numbers
# ---------------------------------------------------------------------------
#
# Two per cent loss and a 60 ms hold every 150 ms, seeded so the run repeats.
# The hold rather than more loss is the point: udp-fault holds datagrams and then
# releases them together, which is what an access point going off channel does,
# and `crates/link-metrics` counts the result as a stall followed by units
# arriving early - bunching, which is a different fault from loss and needs a
# different answer.
#
# The size follows from the number above rather than from taste. At 120 fps a
# 60 ms hold is seven source periods, so it yields one crossing of two periods
# and cannot be mistaken for jitter; a 150 ms cycle fits some thirty of them into
# a five-second arm, which is about 400 crossings a minute against the 162 this
# link produces on its own. An earlier draft held 120 ms every 1500 ms - three
# holds, some 36 crossings a minute - which is well inside that variance and
# would have made the control indistinguishable from doing nothing.
#
# It cannot grow without limit either. A6's own broken-link control holds 400 ms
# every 2 s and was rejected here: at 50 Mbps that is two thousand datagrams and
# 2.4 MB in one queue, so what reaches the far side is the relay's release burst
# rather than anything the radio did. A 60 ms hold queues some 310 datagrams,
# 370 kB. Two per cent loss was chosen over a larger figure for the same reason
# in reverse: every committed arm on this channel and all nine of N1's lost
# nothing at all, so any loss at all is separation, and a control that destroys
# the population cannot show an instrument reading it.
#
# ---------------------------------------------------------------------------
# usage
# ---------------------------------------------------------------------------
#
#   tools/net-preflight-gate.sh [seconds]
#
# exit 0  every judgement came out the way it had to
# exit 1  one did not, and the block above it names the criterion and the numbers
# exit 2  refused: an arm that had to be decided could not be, so nothing here
#         says whether the probe reads this link correctly either way

set -euo pipefail

SECONDS_TO_RUN="${1:-5}"
FPS="${FPS:-120}"
MTU="${MTU:-1200}"
PORT="${PORT:-5004}"
# Not 5106. That port belongs to udp-fault by convention across this repository
# and a relay left running by another harness in another worktree owned it while
# this gate first ran, which cost two arms and looked like a hang. A gate that
# shares a port with the tool it starts is a gate that measures whichever
# instance won the bind.
RELAY_PORT="${RELAY_PORT:-5116}"
REFUSAL_PORT="${REFUSAL_PORT:-5117}"
IFACE="${IFACE:-en0}"
FIXTURE="${FIXTURE:-motion-1920x1080@120-10s-50M.h264}"
WIN_REPO="${WIN_REPO:-C:\\Users\\luque\\lanplay-rs}"
# Fault injection is deterministic and this is the seed every control arm in this
# repository uses, so an arm that behaves oddly can be re-run exactly.
SEED="${SEED:-20250815}"

# Threshold crossings a minute that this link produces on its own, between arms,
# with nothing running and nothing configured differently: N1's nine 90 s arms
# ranged 20.62 to 182.66, a spread of 162 on the very quantity a probe measures,
# while their delivery p99 spanned 11.93 to 17.79 ms. Any injected fault has to
# separate the two arms by more than this or the control has no power - which is
# the result N1's own comparison got and reported rather than dressing up. It is
# a floor on the separation and never a threshold on either arm alone.
NATURAL_CROSSINGS_PER_MIN="${NATURAL_CROSSINGS_PER_MIN:-162}"

REPO="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${OUT:-/tmp/net-preflight-gate/$(date +%Y%m%d-%H%M%S)}"
PROBE="$REPO/target/release/net-preflight"
RELAY="$REPO/target/release/udp-fault"
XTASK="$REPO/target/release/xtask"

mkdir -p "$OUT"

# Every session keeps its own log, because the per-arm documents beside it cannot
# say what a reader of a whole session wants: which arms ran, in which order, and
# what the two came out as side by side. Re-executed through `tee` rather than
# redirected with `exec`, so that the pipeline is one this shell waits on and the
# verdict at the bottom cannot be the line that was still in a buffer. The first
# session committed under results/ has no such file, which is what suggested it.
if [ -z "${NET_PREFLIGHT_SESSION_LOG:-}" ]; then
    export NET_PREFLIGHT_SESSION_LOG=1 OUT
    "$0" "$@" 2>&1 | tee "$OUT/gate.out"
    exit "${PIPESTATUS[0]}"
fi

echo "results   $OUT"

fail() {
    echo "net-preflight-gate: $1" >&2
    exit 1
}

cargo build --release -q -p lanplay-net-preflight -p lanplay-udp-fault -p xtask

COMMIT_ARGS=()
if COMMIT="$(git -C "$REPO" rev-parse --short HEAD 2>/dev/null)"; then
    COMMIT_ARGS=(--commit "$COMMIT")
fi

# ---- the first arm, which needs nothing -----------------------------------
# Run before the link is checked, on the loopback interface and with no sender,
# because it is a statement about the probe and not about the radio: a harness
# whose refusal is broken would report the two arms below as clean links.

echo
echo "arm       refusal: nothing is sending, and the probe must say so"
refusal_status=0
"$PROBE" --bind "127.0.0.1:$REFUSAL_PORT" --seconds "$SECONDS_TO_RUN" --wait-seconds 3 \
    --arm refusal --expect clean \
    --report "$OUT/refusal.report.json" --envelope "$OUT/refusal.envelope.json" \
    "${COMMIT_ARGS[@]}" >"$OUT/refusal.out" 2>&1 || refusal_status=$?
grep -E "^(radio|REFUSED)" "$OUT/refusal.out" || true
# Exit 1 is the probe saying it could not run at all, which is a different thing
# from the refusal this arm exists to produce and must not be swallowed: the
# first run of this gate had its port taken by a relay from another worktree,
# the arm never listened, and the reason sat in a log nobody was reading.
[ "$refusal_status" -ne 1 ] || {
    sed 's/^/          /' "$OUT/refusal.out" >&2
    fail "the refusal arm could not run, so the probe's refusal was never exercised"
}

# ---- the link the other two arms cross ------------------------------------
# An interface that is down, or up with no address, would send the run somewhere
# else and label the result with this one.

status="$(ifconfig "$IFACE" 2>/dev/null | awk '/status:/{print $2}')"
[ "$status" = "active" ] || fail "$IFACE is ${status:-missing}"
LOCAL_IP="$(ipconfig getifaddr "$IFACE" || true)"
[ -n "$LOCAL_IP" ] || fail "$IFACE is up but has no IPv4 address"
WIN_IP="$(ssh -G windows 2>/dev/null | awk '/^hostname /{print $2}')"
[ -n "$WIN_IP" ] || fail "no host named windows in the ssh configuration"
echo
echo "link      $IFACE $LOCAL_IP -> $WIN_IP"

# The host's firewall answers no ICMP, so tcp/22 stands in: it is the port this
# script already depends on, and cannot pass where the run would fail.
if ping -c 2 -t 2 -S "$LOCAL_IP" "$WIN_IP" >/dev/null 2>&1; then
    echo "route     ICMP answered"
elif nc -z -w 3 -s "$LOCAL_IP" "$WIN_IP" 22 >/dev/null 2>&1; then
    echo "route     tcp/22 answered; ICMP is filtered"
else
    fail "no route to $WIN_IP from $LOCAL_IP"
fi

"$REPO/tools/win-ssh.sh" --check >/dev/null || fail "the host does not answer over ssh"

# One measurement's worth of traffic that never left the host is a whole arm
# wasted, so the sender is confirmed to exist before the first one starts.
"$REPO/tools/win-ssh.sh" "if exist $WIN_REPO\\target\\release\\net-bench.exe (echo present) else (echo missing)" |
    tr -d '\r' | grep -qx present ||
    fail "net-bench.exe is not built on the host: cargo build --release -p lanplay-net-bench there"

# ---- one arm over the air -------------------------------------------------
# The probe starts first and prints when it is listening; the sender runs two
# seconds longer than the probe measures for, so the probe's window is covered
# at both ends rather than trimmed by a sender that started late.

# Waits for a line to appear in a log, or gives up saying what it saw instead.
#
# Bounded because the first run of this gate hung: it waited forever for a relay
# banner that never came, because the relay had failed to bind and said so on the
# line above the one being waited for. An unattended harness that can hang has a
# failure mode worse than any verdict it can reach.
await_line() {
    local pattern=$1 path=$2 limit=${3:-15} waited=0
    until grep -q "$pattern" "$path" 2>/dev/null; do
        sleep 0.2
        waited=$((waited + 1))
        if [ "$waited" -gt $((limit * 5)) ]; then
            sed 's/^/          /' "$path" >&2 2>/dev/null || true
            fail "waited ${limit}s for $pattern in $(basename "$path") and it never arrived"
        fi
    done
}

# Which criteria the crossed document states: the other arm's.
crossed() {
    case "$1" in
    clean) echo faults ;;
    *) echo clean ;;
    esac
}

arm() {
    local name=$1 expect=$2 target=$3
    shift 3

    "$PROBE" --bind "0.0.0.0:$PORT" --seconds "$SECONDS_TO_RUN" --fps "$FPS" --mtu "$MTU" \
        --arm "$name" --expect "$expect" --pacer burst --wait-seconds 30 \
        --report "$OUT/$name.report.json" \
        --envelope "$OUT/$name.envelope.json" \
        --cross-envelope "$OUT/$name-as-$(crossed "$expect").envelope.json" \
        "${COMMIT_ARGS[@]}" "$@" >"$OUT/$name.out" 2>&1 &
    local probe=$!
    await_line "^listening" "$OUT/$name.out"

    "$REPO/tools/win-ssh.sh" \
        "cd $WIN_REPO && .\\target\\release\\net-bench.exe send --to $target \
         --fixture fixtures\\$FIXTURE --fps $FPS --mtu $MTU --pacer burst \
         --seconds $((SECONDS_TO_RUN + 2))" >"$OUT/$name.tx.out" 2>&1 &
    local sender=$!

    local probe_status=0
    wait $probe || probe_status=$?
    wait $sender || true
    grep -E "^(radio|shape|stream|cadence|tail|error|REFUSED)" "$OUT/$name.out" || true

    # An arm that received nothing was almost certainly failed by the sender, and
    # the sender's own words are two lines away in a file nobody thinks to open.
    # The first run of this gate over the air refused both arms because a stale
    # net-bench.exe on the host was blocked by Device Guard, and that sentence was
    # sitting in this log while the summary said only that no datagram arrived.
    if [ "$probe_status" -ne 0 ] && [ -s "$OUT/$name.tx.out" ]; then
        echo "sender    the host said:"
        sed 's/^/          /' "$OUT/$name.tx.out" | tail -6
    fi
}

# The control first. A deciding comparison that cannot come out negative makes
# everything above it worthless, and finding that out after the clean arm has
# spent its radio time is finding it out too late.
echo
echo "arm       faults: the same traffic through udp-fault, which must be seen"
# Sized against the link rather than against a feeling. The injection has to
# separate the two arms by more than the variance the link produces on its own,
# which N1 measured as 162 threshold crossings a minute between arms that had
# nothing running: at 120 fps a hold of 60 ms is seven source periods and so
# yields one crossing of two periods, and a 150 ms cycle fits some thirty of them
# into a five-second arm, which is 400 a minute against a natural 162. Loss stays
# at 2 per cent because loss needs no margin here - every committed arm on this
# channel and all nine of N1's lost nothing at all, so any loss is separation -
# and because a larger figure would destroy the population the instrument is
# being shown reading.
#
# The hold also has to stay small enough that the arm is about the link. At 50
# Mbps a 60 ms hold queues some 310 datagrams, 370 kB, inside the relay; A6's own
# 400 ms hold queues two thousand and 2.4 MB, at which point what reaches the far
# side is the relay's release burst rather than anything the radio did.
"$RELAY" --listen "0.0.0.0:$RELAY_PORT" --forward "127.0.0.1:$PORT" \
    --loss 2 --stall-ms 60 --stall-every-ms 150 --seed "$SEED" \
    >"$OUT/faults.relay.out" 2>&1 &
relay=$!
trap 'kill $relay 2>/dev/null || true' EXIT
await_line "^udp-fault:" "$OUT/faults.relay.out" 5
grep -E "^udp-fault" "$OUT/faults.relay.out"
arm faults faults "$LOCAL_IP:$RELAY_PORT" \
    --faults "loss 2%, 60 ms held every 150 ms" --fault-seed "$SEED"
kill $relay 2>/dev/null || true
trap - EXIT
wait $relay 2>/dev/null || true
grep -E "^ +[0-9]+s" "$OUT/faults.relay.out" | tail -2 || true

echo
echo "arm       clean: the link as it is"
arm clean clean "$LOCAL_IP:$PORT"

# ---- the judgements --------------------------------------------------
# Nothing here parses the probe's prose. Each document goes through `xtask
# verdict`, which is the only place in this repository where a verdict is
# decided, and every number quoted below comes back out of the same parser.

echo
echo "judgements"
echo

wrong=0
undecided=0

judge() {
    local name=$1 want=$2
    local path="$OUT/$name.envelope.json"
    local got block

    if [[ ! -s "$path" ]]; then
        printf '  %-6s %-18s no document was written, so nothing was judged\n' REFUSE "$name"
        undecided=$((undecided + 1))
        return
    fi
    if block="$("$XTASK" verdict "$path")"; then
        got=PASS
    else
        case $? in
        1) got=FAIL ;;
        *) got=REFUSED ;;
        esac
    fi
    printf '%s\n' "$block" >"$OUT/$name.verdict"

    if [[ "$got" == "$want" ]]; then
        printf '  %-6s %-18s %s, as it had to be\n' ok "$name" "$got"
        return
    fi
    printf '  %-6s %-18s %s where %s was required\n' WRONG "$name" "$got" "$want"
    # The block, and not a summary of it: the criterion and the numbers it was
    # decided on are what a reader needs a month later.
    sed 's/^/         /' "$OUT/$name.verdict"
    if [[ "$got" == REFUSED ]]; then
        undecided=$((undecided + 1))
    else
        wrong=$((wrong + 1))
    fi
}

judge refusal REFUSED
judge faults PASS
judge faults-as-clean FAIL
judge clean PASS
judge clean-as-faults FAIL

# ---- the two arms side by side --------------------------------------------
# The deliverable of the gate is not the verdict, it is that the same instrument
# reads two links differently. Printed even when a judgement went the wrong way:
# a failed criterion does not make the measurement uninteresting.

observation() {
    "$XTASK" verdict --observation "$2" "$OUT/$1.envelope.json" 2>/dev/null || echo absent
}

echo
printf '  %-34s %14s %14s\n' '' clean faults
for row in \
    "datagrams accounted:datagrams_accounted" \
    "datagrams lost:datagrams_lost" \
    "access units delivered:access_units_delivered" \
    "access units lost:access_units_lost" \
    "datagrams per access unit:datagrams_per_access_unit" \
    "megabits per second:mbps" \
    "complete interval p50 ms:au_interval_p50_ms" \
    "complete interval p99 ms:au_interval_p99_ms" \
    "worst interval ms:au_interval_max_ms" \
    "crossings of two periods:delivery_over_2t" \
    "crossings per minute:delivery_over_2t_per_min" \
    "stall clusters:delivery_stall_clusters" \
    "stall clusters per minute:delivery_stall_clusters_per_min"; do
    printf '  %-34s %14s %14s\n' "${row%%:*}" \
        "$(observation clean "${row##*:}")" "$(observation faults "${row##*:}")"
done

# ---- and whether that difference means anything ---------------------------
# Two arms that a reader cannot tell apart are not a control, however each one
# was judged on its own. This link produces 162 threshold crossings a minute of
# variation between arms with nothing running, so a separation smaller than that
# is inside the noise and the honest answer is that the comparison had no power -
# which is what N1's neutrality comparison concluded about itself rather than
# reporting the absence of a difference as an absence of an effect.
#
# Refused rather than failed. A control with no power did not disagree with
# anything; nobody was in a position to ask it.

echo
separation="$(awk -v a="$(observation faults delivery_over_2t_per_min)" \
    -v b="$(observation clean delivery_over_2t_per_min)" \
    'BEGIN { if (a + 0 == a && b + 0 == b) printf "%.1f", a - b; else print "absent" }')"
printf '  %-34s %14s %14s\n' "separation in crossings/min" "$separation" \
    "needs > $NATURAL_CROSSINGS_PER_MIN"
if [[ "$separation" == absent ]]; then
    echo "  REFUSE one of the two arms stated no crossing rate, so nothing was compared"
    undecided=$((undecided + 1))
elif awk -v s="$separation" -v n="$NATURAL_CROSSINGS_PER_MIN" 'BEGIN { exit !(s > n) }'; then
    echo "  ok     the injected fault stands clear of what this link does on its own"
else
    echo "  REFUSE the two arms are inside this link's own variance, so this comparison has"
    echo "         no power: the fault arm may have been read correctly and nothing here"
    echo "         shows it, and an instrument that cannot separate them is not evidence"
    undecided=$((undecided + 1))
fi

echo
if [[ "$wrong" -ne 0 ]]; then
    echo "FAIL $wrong of the six judgements came out the wrong way, named above"
    [[ "$undecided" -ne 0 ]] && echo "     $undecided more could not be decided at all, which does not soften one that was"
    exit 1
fi
if [[ "$undecided" -ne 0 ]]; then
    echo "REFUSED $undecided of the six judgements could not be decided, so nothing here says"
    echo "        whether the probe reads this link correctly either way"
    exit 2
fi
echo "PASS the probe refuses an empty population, reads the injected fault as a link it"
echo "     must not call good, and reads the link as it is without crying wolf; each arm"
echo "     fails the other arm's criteria, and the two stand apart by more than this"
echo "     link's own between-arm variance, which is what makes either one evidence"
