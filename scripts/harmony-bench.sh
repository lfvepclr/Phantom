#!/usr/bin/env bash
# Phantom HarmonyOS on-device benchmark: measure one scenario and judge it.
#
# What it does while you drive the phone:
#   1. samples `ifconfig vpn-tun` once a second (bytes + TX/RX dropped)
#   2. snapshots the client's stats JSON before and after
#   3. pulls the TUN trace and runs scripts/tun-trace-report.py on it
#   4. prints one summary block (optionally appended to a report file)
#
# Usage:
#   scripts/harmony-bench.sh --label "video 1080p" --seconds 60
#   scripts/harmony-bench.sh --label "dl.google.com" --seconds 60 --out tests/PERF_TUN_PATH_REPORT.md
#
# Run it, then do the scenario on the phone (play the video / start the
# download). Restart the tunnel right before each measurement: the trace file is
# truncated on every start, so the numbers are per-scenario.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SANDBOX="/data/app/el2/100/base/co.phantom.harmony/haps/entry/files"
HDC="${HDC:-/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/toolchains/hdc}"
LABEL="scenario"
SECONDS_TO_RUN=60
OUT=""
WORK="$(mktemp -d "${TMPDIR:-/tmp}/phantom-bench.XXXXXX")"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --label) LABEL="${2:?}"; shift 2 ;;
        --seconds) SECONDS_TO_RUN="${2:?}"; shift 2 ;;
        --out) OUT="${2:?}"; shift 2 ;;
        --hdc) HDC="${2:?}"; shift 2 ;;
        -h|--help) sed -n '2,18p' "$0"; exit 0 ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
done

[[ -x "$HDC" ]] || { echo "ERROR: hdc not found at $HDC (set HDC=…)" >&2; exit 1; }

shell() { "$HDC" shell "$1" 2>/dev/null; }

if ! "$HDC" list targets 2>/dev/null | grep -q .; then
    echo "ERROR: no device connected (hdc list targets is empty)" >&2
    exit 1
fi

# --- helpers -------------------------------------------------------------

# `ifconfig vpn-tun` → "rx_bytes tx_bytes rx_dropped tx_dropped"
read_tun_counters() {
    shell "ifconfig vpn-tun" | awk '
        /RX packets/ { for (i = 1; i <= NF; i++) if ($i ~ /^dropped:/) rx_drop = substr($i, 9) }
        /TX packets/ { for (i = 1; i <= NF; i++) if ($i ~ /^dropped:/) tx_drop = substr($i, 9) }
        /RX bytes/   { for (i = 1; i <= NF; i++) if ($i == "bytes:") rx = $(i+1) }
        /TX bytes/   { for (i = 1; i <= NF; i++) if ($i == "bytes:") tx = $(i+1) }
        END { printf "%d %d %d %d\n", rx, tx, rx_drop, tx_drop }'
}

read_stats() {
    shell "cat $SANDBOX/phantom_vpn_stats.txt" | tr -d '\r' | grep -m1 '{' || true
}

echo "▶ Phantom HarmonyOS bench — $LABEL (${SECONDS_TO_RUN}s)"
echo "  trace is truncated on every tunnel restart; restart it now if you have not."

BEFORE_TUN="$(read_tun_counters)"
BEFORE_STATS="$(read_stats)"
echo "$BEFORE_STATS" > "$WORK/stats.jsonl"

echo "  sampling vpn-tun once a second … (drive the phone now)"
SAMPLES="$WORK/tun.csv"
echo "second,rx_bytes,tx_bytes,rx_dropped,tx_dropped" > "$SAMPLES"
for ((i = 1; i <= SECONDS_TO_RUN; i++)); do
    read -r rx tx rx_drop tx_drop <<< "$(read_tun_counters)"
    echo "$i,$rx,$tx,$rx_drop,$tx_drop" >> "$SAMPLES"
    sleep 1
done

AFTER_TUN="$(read_tun_counters)"
AFTER_STATS="$(read_stats)"
echo "$AFTER_STATS" >> "$WORK/stats.jsonl"

# --- pull the trace and analyse -----------------------------------------

echo "  pulling phantom_tun_trace.log …"
"$HDC" file recv "$SANDBOX/phantom_tun_trace.log" "$WORK/phantom_tun_trace.log" >/dev/null 2>&1 || true

set +e
python3 "$REPO_ROOT/scripts/tun-trace-report.py" \
    "$WORK/phantom_tun_trace.log" --stats "$WORK/stats.jsonl" --json > "$WORK/report.json"
ANALYZER_STATUS=$?
set -e

read -r b_rx b_tx b_rx_drop b_tx_drop <<< "$BEFORE_TUN"
read -r a_rx a_tx a_rx_drop a_tx_drop <<< "$AFTER_TUN"
DOWN_BYTES=$(( (a_rx - b_rx) ))
UP_BYTES=$(( (a_tx - b_tx) ))
RX_DROP_DELTA=$(( (a_rx_drop - b_rx_drop) ))
TX_DROP_DELTA=$(( (a_tx_drop - b_tx_drop) ))
DOWN_RATE=$(( DOWN_BYTES / SECONDS_TO_RUN ))
UP_RATE=$(( UP_BYTES / SECONDS_TO_RUN ))

# Median of the per-second deltas is a better estimate than the mean when the
# first second includes connection setup.
MEDIAN_DOWN="$(python3 - "$SAMPLES" <<'PY'
import statistics, sys
rows = [line.strip().split(",") for line in open(sys.argv[1]).read().splitlines()[1:] if line.strip()]
values = []
for previous, current in zip(rows, rows[1:]):
    values.append(int(current[1]) - int(previous[1]))
print(int(statistics.median(values)) if values else 0)
PY
)"

VERDICT="$(python3 -c 'import json,sys; print("PASS" if json.load(open(sys.argv[1]))["pass"] else "FAIL")' "$WORK/report.json" 2>/dev/null || echo UNKNOWN)"
RETX_RATE="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["checks"]["retransmits_per_minute"]["value"])' "$WORK/report.json" 2>/dev/null || echo 0)"
DUP_MB="$(python3 -c 'import json,sys; print(round(json.load(open(sys.argv[1]))["duplicate_bytes"]/1e6,2))' "$WORK/report.json" 2>/dev/null || echo 0)"
UNIQUE_MB="$(python3 -c 'import json,sys; print(round(json.load(open(sys.argv[1]))["unique_down_bytes"]/1e6,2))' "$WORK/report.json" 2>/dev/null || echo 0)"

echo ""
echo "──────── $LABEL ────────"
printf '  vpn-tun down : %s bytes (%.1f KB/s avg, %.1f KB/s median)\n' \
    "$DOWN_BYTES" "$(echo "$DOWN_RATE" | awk '{print $1/1024}')" "$(echo "$MEDIAN_DOWN" | awk '{print $1/1024}')"
printf '  vpn-tun up   : %s bytes (%.1f KB/s avg)\n' "$UP_BYTES" "$(echo "$UP_RATE" | awk '{print $1/1024}')"
printf '  RX/TX dropped: +%s / +%s\n' "$RX_DROP_DELTA" "$TX_DROP_DELTA"
printf '  trace        : %s retransmits/min, %s MB duplicate vs %s MB unique\n' "$RETX_RATE" "$DUP_MB" "$UNIQUE_MB"
printf '  verdict      : %s\n' "$VERDICT"
echo "  artifacts    : $WORK (report.json, tun.csv, phantom_tun_trace.log)"

if [[ -n "$OUT" ]]; then
    {
        echo ""
        echo "| $(date '+%Y-%m-%d %H:%M') | $LABEL | ${DOWN_RATE} | ${MEDIAN_DOWN} | +${TX_DROP_DELTA} | ${RETX_RATE} | ${DUP_MB} MB / ${UNIQUE_MB} MB | ${VERDICT} |"
    } >> "$OUT"
    echo "  appended a row to $OUT"
fi

exit $ANALYZER_STATUS
