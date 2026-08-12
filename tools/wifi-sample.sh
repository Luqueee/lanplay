#!/usr/bin/env bash
# B4: what the radio was doing while a run was being measured.
#
# The question a sweep cannot answer on its own: when a ten-second window
# collapses, does the radio collapse with it? If RSSI, PHY rate and channel
# are flat through a bad window, the suspicion moves off signal quality and
# onto the access point's scheduling or the receiver's delivery path.
#
# Sampled from outside the measured process, once a second, because
# system_profiler is not cheap and has no business on a receive thread.
#
# usage:
#   tools/wifi-sample.sh <output.csv> [seconds]

set -euo pipefail

OUT="${1:?usage: wifi-sample.sh <output.csv> [seconds]}"
SECONDS_TO_RUN="${2:-120}"

echo "t_s,rssi_dbm,noise_dbm,tx_rate_mbps,mcs,channel" >"$OUT"
START="$(date +%s)"
END=$((START + SECONDS_TO_RUN))

while [ "$(date +%s)" -lt "$END" ]; do
    now="$(date +%s)"
    # One call, parsed once: asking system_profiler per field would cost a
    # second each and the samples would not describe the same instant.
    info="$(system_profiler SPAirPortDataType 2>/dev/null | sed -n '/Current Network Information/,/Other Local Wi-Fi Networks/p')"
    rssi="$(printf '%s' "$info" | awk -F'[:/ ]+' '/Signal . Noise/{print $(NF-3)}' | head -1)"
    noise="$(printf '%s' "$info" | awk -F'[:/ ]+' '/Signal . Noise/{print $(NF-1)}' | head -1)"
    rate="$(printf '%s' "$info" | awk -F': *' '/Transmit Rate/{print $2}' | head -1)"
    mcs="$(printf '%s' "$info" | awk -F': *' '/MCS Index/{print $2}' | head -1)"
    channel="$(printf '%s' "$info" | awk -F': *' '/Channel/{print $2}' | head -1 | tr -d ' ')"
    echo "$((now - START)),${rssi:-},${noise:-},${rate:-},${mcs:-},${channel:-}" >>"$OUT"
    sleep 1
done
