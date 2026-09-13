#!/usr/bin/env bash
#
# Four-dimension measurement for the Android client: CPU, memory, network, power.
#
# Usage:
#   scripts/measure-android.sh <scenario> [duration-seconds]
#
# Scenarios (identical to measure-harmony.sh, so results can be compared):
#   A_idle_screen_off       not connected, screen off          (wake-up check)
#   B_connected_screen_off  connected, screen off, no traffic  (idle cost)
#   C_foreground            connected, foreground, screen on   (UI cost)
#
# Output: one JSON document under scripts/measurements/, plus a copy on stdout.
# The schema is shared with measure-harmony.sh (see scripts/README.md), so the
# two platforms produce directly comparable rows.
#
# Every tool is probed before it is used. A tool that is missing degrades the
# fields it feeds to `null` and is recorded in `probe.missing` -- a partial
# measurement that is honest beats a run that never happens. A *device* is the
# one hard prerequisite: no device means no measurement at all.
#
# Power figures are only meaningful on a real device. An emulator reports
# synthesised mAh, has no Doze and never sees a radio: use one for before/after
# CPU, memory, network and rendering, never for a battery conclusion.
#
set -euo pipefail

SCENARIO="${1:-}"
DURATION="${2:-60}"
INTERVAL="${INTERVAL:-10}"
PACKAGE="${PACKAGE:-co.phantom.android}"
OUT_DIR="$(cd "$(dirname "$0")" && pwd)/measurements"
STAMP="$(date +%Y%m%d-%H%M%S)"

usage() { sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }
case "$SCENARIO" in
  A_idle_screen_off|B_connected_screen_off|C_foreground) ;;
  *) usage ;;
esac

command -v adb >/dev/null 2>&1 || { echo "ERROR: adb not found in PATH." >&2; exit 1; }
if ! adb devices | tail -n +2 | grep -q 'device$'; then
  echo "ERROR: no Android device/emulator connected (adb devices)." >&2
  exit 1
fi

mkdir -p "$OUT_DIR"

# --- tool probing -------------------------------------------------------------
# `probe` records what this device/toolchain offered, so a null in the samples
# reads as "not measurable here" instead of "measured as zero". The lists are
# kept as pre-rendered JSON fragments: bash 3.2 (the system bash on macOS) has
# no nameref or `${arr[*]@Q}`, and an empty array must still render as `[]`.
AVAILABLE_JSON=""
MISSING_JSON=""
csv() { local s="${1%,}"; echo "$s"; }

probe() {
  local name="$1" check="$2"
  if adb shell "$check" >/dev/null 2>&1; then
    AVAILABLE_JSON="${AVAILABLE_JSON}\"${name}\","
  else
    MISSING_JSON="${MISSING_JSON}\"${name}\","
  fi
}

probe cpuinfo      "dumpsys cpuinfo"
probe top_threads  "top -b -n 1 -p 1"
probe meminfo      "dumpsys meminfo"
probe netstats     "dumpsys netstats detail"
probe batterystats "dumpsys batterystats"
probe battery      "dumpsys battery"
probe gfxinfo      "dumpsys gfxinfo"

PID="$(adb shell pidof "$PACKAGE" 2>/dev/null | tr -d '\r[:space:]')"
if [[ -z "$PID" ]]; then
  echo "ERROR: $PACKAGE is not running. Start the app and reach the scenario first." >&2
  exit 1
fi

UID_APP="$(adb shell dumpsys package "$PACKAGE" 2>/dev/null \
  | sed -n 's/.*userId=\([0-9]\{1,\}\).*/\1/p' | head -1)"

# --- helpers ------------------------------------------------------------------

num() { [[ "${1:-}" =~ ^-?[0-9]+([.][0-9]+)?$ ]] && echo "$1" || echo null; }
available() { [[ ",${AVAILABLE_JSON}" == *",\"$1\","* ]]; }

# --- samplers -----------------------------------------------------------------

sample_cpu() {
  available cpuinfo || { echo null; return; }
  local raw
  raw="$(adb shell dumpsys cpuinfo 2>/dev/null \
    | awk -v p="$PACKAGE" '$0 ~ p {print $1; exit}' | tr -d '%')"
  num "${raw:-}"
}

sample_cpu_breakdown() {
  # Per-thread rows are what expose a polling thread. `null` means the device's
  # `top` has no `-H`; `[]` would claim the process has no threads.
  available top_threads || { echo null; return; }
  local out body
  out="$(adb shell top -H -b -n 1 -p "$PID" 2>/dev/null | tail -n +2 || true)"
  body="$(awk '
    NF >= 9 && $1 ~ /^[0-9]+$/ {
      printf "%s{\"name\":\"%s\",\"cpu_percent\":%s}", (n++ ? "," : ""), $NF, $9 + 0
    }
  ' <<<"$out")"
  echo "[$body]"
}

sample_pss() {
  available meminfo || { echo null; return; }
  local pss
  pss="$(adb shell dumpsys meminfo "$PACKAGE" 2>/dev/null \
    | awk '/TOTAL PSS:/ {print $3 + 0; exit} /^ *TOTAL / {print $2 + 0; exit}')"
  num "${pss:-}"
}

sample_net() {
  # Per-uid byte counters. `dumpsys netstats detail` spells the same counters
  # `rb=`/`tb=` on some releases and `rxBytes=`/`txBytes=` on others, so both
  # spellings are summed; if neither appears the field is null, not zero.
  if ! available netstats || [[ -z "$UID_APP" ]]; then
    echo '{"rx":null,"tx":null}'
    return
  fi
  local raw rx tx
  raw="$(adb shell dumpsys netstats detail 2>/dev/null | grep -F "uid=$UID_APP" || true)"
  rx="$(awk '{for (i = 1; i <= NF; i++) {
        if ($i ~ /^rb=/)      {v = $i; sub(/^rb=/, "", v);      s += v}
        if ($i ~ /^rxBytes=/) {v = $i; sub(/^rxBytes=/, "", v); s += v}
      }} END {print (NR ? s + 0 : "null")}' <<<"$raw")"
  tx="$(awk '{for (i = 1; i <= NF; i++) {
        if ($i ~ /^tb=/)      {v = $i; sub(/^tb=/, "", v);      s += v}
        if ($i ~ /^txBytes=/) {v = $i; sub(/^txBytes=/, "", v); s += v}
      }} END {print (NR ? s + 0 : "null")}' <<<"$raw")"
  echo "{\"rx\":$rx,\"tx\":$tx}"
}

sample_power() {
  # `wakeups` counts the wake-lock acquisitions BatteryStats attributed to the
  # package: the number that has to fall for scenarios A and B. mAh is only
  # trusted from a real battery.
  local wakeups=null mah=null cur=null temp=null
  if available batterystats; then
    local dump
    dump="$(adb shell dumpsys batterystats --charged "$PACKAGE" 2>/dev/null || true)"
    if [[ -n "$dump" ]]; then
      wakeups="$(num "$(grep -c 'Wake lock' <<<"$dump" || true)")"
      mah="$(num "$(sed -n 's/.*Estimated power use.*: *\([0-9.]\{1,\}\).*/\1/p' <<<"$dump" | head -1)")"
    fi
  fi
  if available battery; then
    local b
    b="$(adb shell dumpsys battery 2>/dev/null || true)"
    cur="$(num "$(sed -n 's/.*current now: *\(-\{0,1\}[0-9]\{1,\}\).*/\1/p' <<<"$b" | head -1)")"
    temp="$(sed -n 's/.*temperature: *\([0-9]\{1,\}\).*/\1/p' <<<"$b" | head -1)"
    # `dumpsys battery` reports tenths of a degree.
    if [[ "${temp:-}" =~ ^[0-9]+$ ]]; then
      temp="$(awk -v t="$temp" 'BEGIN {printf "%.1f", t / 10}')"
    else
      temp=null
    fi
  fi
  echo "{\"wakeups\":$wakeups,\"mah\":$mah,\"current_ma\":$cur,\"temp_c\":$temp}"
}

sample_render() {
  # Scenario C only. `gfxinfo` counts frames/jank; an FPS figure would need
  # `perfetto`, so it stays null here.
  available gfxinfo || { echo '{"fps":null,"jank_frames":null}'; return; }
  local dump jank
  dump="$(adb shell dumpsys gfxinfo "$PACKAGE" 2>/dev/null || true)"
  jank="$(sed -n 's/.*Janky frames: *\([0-9]\{1,\}\).*/\1/p' <<<"$dump" | head -1)"
  echo "{\"fps\":null,\"jank_frames\":$(num "${jank:-}")}"
}

sample_idle_objects() {
  # Live datapath object counts. Android keeps them in-process and its bridge
  # exposes cumulative totals only, so this is null unless the caller hands
  # over a snapshot: `PHANTOM_IDLE_JSON=flow.json scripts/measure-android.sh …`.
  if [[ -n "${PHANTOM_IDLE_JSON:-}" && -r "${PHANTOM_IDLE_JSON}" ]]; then
    cat "$PHANTOM_IDLE_JSON"
  else
    echo '{"flows":null,"pool_idle":null,"udp_flows":null}'
  fi
}

# --- run ----------------------------------------------------------------------

SAMPLES=$(( DURATION / INTERVAL ))
[[ "$SAMPLES" -lt 1 ]] && SAMPLES=1

# Reset the battery window so the first sample describes this run rather than
# the weeks before it. A failed reset is not fatal: the counters stay cumulative.
adb shell dumpsys batterystats --reset >/dev/null 2>&1 || true

OUT_FILE="$OUT_DIR/android-$SCENARIO-$STAMP.json"
{
  echo "{"
  echo "  \"scenario\": \"$SCENARIO\","
  echo "  \"platform\": \"android\","
  echo "  \"duration_s\": $DURATION,"
  echo "  \"interval_s\": $INTERVAL,"
  echo "  \"pids\": [$PID],"
  echo "  \"probe\": {"
  echo "    \"available\": [$(csv "$AVAILABLE_JSON")],"
  echo "    \"missing\": [$(csv "$MISSING_JSON")]"
  echo "  },"
  echo "  \"samples\": ["
  for i in $(seq 1 "$SAMPLES"); do
    PID="$(adb shell pidof "$PACKAGE" 2>/dev/null | tr -d '\r[:space:]')"
    if [[ -z "$PID" ]]; then
      echo "ERROR: $PACKAGE disappeared mid-run (crashed or was killed)." >&2
      exit 1
    fi
    COMMA=","
    [[ "$i" == "$SAMPLES" ]] && COMMA=""
    echo "    {"
    echo "      \"t\": $(date +%s),"
    echo "      \"cpu_percent\": $(sample_cpu),"
    echo "      \"cpu_breakdown\": $(sample_cpu_breakdown),"
    echo "      \"pss_kb\": $(sample_pss),"
    echo "      \"mem_breakdown\": null,"
    echo "      \"net_bytes\": $(sample_net),"
    echo "      \"net_conns\": null,"
    echo "      \"power\": $(sample_power),"
    echo "      \"render\": $(sample_render),"
    echo "      \"idle_objects\": $(sample_idle_objects)"
    echo "    }${COMMA}"
    [[ "$i" == "$SAMPLES" ]] || sleep "$INTERVAL"
  done
  echo "  ]"
  echo "}"
} > "$OUT_FILE"

echo "Wrote $OUT_FILE (uid=$UID_APP pid=$PID samples=$SAMPLES)" >&2
cat "$OUT_FILE"
