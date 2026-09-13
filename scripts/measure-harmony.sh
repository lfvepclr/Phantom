#!/usr/bin/env bash
#
# Four-dimension measurement for the HarmonyOS client: CPU, memory, network and
# power/temperature, via hdc (HiDumper / HiPerf / HiTrace).
#
# Usage:
#   scripts/measure-harmony.sh <scenario> [duration-seconds]
#
# Scenarios are the same three as measure-android.sh, so the numbers line up:
#   A_idle_screen_off       not connected, screen off          (wake-up check)
#   B_connected_screen_off  connected, screen off, no traffic  (idle cost)
#   C_foreground            connected, foreground, screen on   (UI cost)
#
# The VPN runs in a *separate process* (PhantomVpnExtensionAbility), so every
# PID of the bundle is sampled and reported per-PID in `cpu_breakdown` /
# `mem_breakdown`: the UI cost and the datapath cost are not the same number and
# must not be averaged into one.
#
# Output: one JSON document under scripts/measurements/, plus a copy on stdout.
# The schema is shared with measure-android.sh (see scripts/README.md).
#
# Every tool is probed before it is used; anything missing degrades its fields
# to `null` and is listed in `probe.missing`. A device is the one hard
# prerequisite. See client/harmony/docs/PERF_TOOLS_GUIDE.md for the per-tool
# background and for which of these commands were confirmed on a real device.
#
set -euo pipefail

SCENARIO="${1:-}"
DURATION="${2:-60}"
INTERVAL="${INTERVAL:-10}"
BUNDLE="${BUNDLE:-co.phantom.harmony}"
OUT_DIR="$(cd "$(dirname "$0")" && pwd)/measurements"
STAMP="$(date +%Y%m%d-%H%M%S)"

usage() { sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 1; }
case "$SCENARIO" in
  A_idle_screen_off|B_connected_screen_off|C_foreground) ;;
  *) usage ;;
esac

command -v hdc >/dev/null 2>&1 || { echo "ERROR: hdc not found in PATH." >&2; exit 1; }
if ! hdc list targets 2>/dev/null | grep -qv '\[Empty\]'; then
  echo "ERROR: no HarmonyOS device connected (hdc list targets)." >&2
  exit 1
fi

mkdir -p "$OUT_DIR"

# --- tool probing -------------------------------------------------------------
# Kept as pre-rendered JSON fragments so an empty list still renders as `[]`.
AVAILABLE_JSON=""
MISSING_JSON=""
csv() { local s="${1%,}"; echo "$s"; }

probe() {
  local name="$1" check="$2"
  if hdc shell "$check" >/dev/null 2>&1; then
    AVAILABLE_JSON="${AVAILABLE_JSON}\"${name}\","
  else
    MISSING_JSON="${MISSING_JSON}\"${name}\","
  fi
}

probe hidumper        "hidumper --help"
probe hidumper_net    "hidumper --net"
probe power_service   "hidumper -s PowerManagerService -a 'current'"
probe thermal_service "hidumper -s ThermalService -a 'temp'"
probe render_service  "hidumper -s RenderService -a 'jank'"
probe hiperf          "hiperf --help"
probe hitrace         "hitrace --help"

PIDS="$(hdc shell "pidof $BUNDLE" 2>/dev/null | tr -d '\r' | tr ' ' '\n' | grep -v '^$' | paste -sd' ' - || true)"
if [[ -z "${PIDS// /}" ]]; then
  echo "ERROR: $BUNDLE is not running. Start the app and reach the scenario first." >&2
  exit 1
fi
# `[2222, 3333]` -- a comma-joined list with no trailing separator.
PIDS_JSON="$(tr ' ' '\n' <<<"$PIDS" | paste -sd, - | sed 's/,/, /g')"
echo "Sampling PIDs: $PIDS" >&2

# --- helpers ------------------------------------------------------------------

num() { [[ "${1:-}" =~ ^-?[0-9]+([.][0-9]+)?$ ]] && echo "$1" || echo null; }
available() { [[ ",${AVAILABLE_JSON}" == *",\"$1\","* ]]; }

# First number found in the output of a shell command, or null.
number_of() {
  local raw
  raw="$(hdc shell "$1" 2>/dev/null | tr -d '\r' || true)"
  num "$(grep -oE '\-?[0-9]+([.][0-9]+)?' <<<"$raw" | head -1)"
}

# --- samplers -----------------------------------------------------------------

# Per-PID CPU and PSS. Both are emitted as arrays so the two processes stay
# distinguishable; `cpu_percent`/`pss_kb` carry the totals for the same row.
sample_cpu_pairs() {
  # Prints two lines: the per-PID array, then the total. A PID that could not
  # be read is left out rather than reported as 0, and if nothing could be read
  # both lines are `null` -- "not measurable" must never look like "zero".
  local pid out="" total=0 got=0
  available hidumper || { printf 'null\nnull\n'; return; }
  for pid in $PIDS; do
    local v
    v="$(number_of "hidumper --cpu $pid")"
    [[ "$v" == null ]] && continue
    out+="{\"name\":\"pid:$pid\",\"cpu_percent\":$v},"
    total="$(awk -v a="$total" -v b="$v" 'BEGIN{printf "%.2f", a+b}')"
    got=$(( got + 1 ))
  done
  [[ "$got" -eq 0 ]] && { printf 'null\nnull\n'; return; }
  printf '[%s]\n%s\n' "${out%,}" "$total"
}

sample_mem_pairs() {
  # Prints two lines: the per-PID array, then the total. Same null-not-zero rule
  # as sample_cpu_pairs.
  local pid out="" total=0 got=0
  available hidumper || { printf 'null\nnull\n'; return; }
  for pid in $PIDS; do
    local raw v
    raw="$(hdc shell "hidumper --mem $pid" 2>/dev/null | tr -d '\r' || true)"
    # The PSS line has been spelled several ways across releases.
    v="$(sed -n 's/.*Pss(KB)[^0-9]*\([0-9]\{1,\}\).*/\1/p
                 s/.*Total PSS[: ]*\([0-9]\{1,\}\).*/\1/p
                 s/.*PSS (summary)[^0-9]*\([0-9]\{1,\}\).*/\1/p' <<<"$raw" | head -1)"
    v="$(num "${v:-}")"
    [[ "$v" == null ]] && continue
    out+="{\"name\":\"pid:$pid\",\"pss_kb\":$v},"
    total="$(awk -v a="$total" -v b="$v" 'BEGIN{printf "%.0f", a+b}')"
    got=$(( got + 1 ))
  done
  [[ "$got" -eq 0 ]] && { printf 'null\nnull\n'; return; }
  printf '[%s]\n%s\n' "${out%,}" "$total"
}

sample_net() {
  # HiDumper reports connections, not byte counters, so `net_bytes` stays null
  # here and `net_conns` carries the number that matters for "does an idle
  # tunnel keep opening sockets".
  echo '{"rx":null,"tx":null}'
}

sample_net_conns() {
  available hidumper_net || { echo null; return; }
  local raw
  raw="$(hdc shell "hidumper --net" 2>/dev/null | tr -d '\r' || true)"
  [[ -n "$raw" ]] || { echo null; return; }
  num "$(grep -ciE '^[[:space:]]*(tcp|udp)' <<<"$raw" || true)"
}

sample_power() {
  local cur=null temp=null
  if available power_service; then
    cur="$(number_of "hidumper -s PowerManagerService -a 'current'")"
  fi
  if available thermal_service; then
    temp="$(number_of "hidumper -s ThermalService -a 'temp'")"
  fi
  # HarmonyOS reports wake-ups through the power service; the figure is not a
  # per-app count, so it stays null rather than pretending to be one.
  echo "{\"wakeups\":null,\"mah\":null,\"current_ma\":$cur,\"temp_c\":$temp}"
}

sample_render() {
  local jank=null
  if available render_service; then
    jank="$(number_of "hidumper -s RenderService -a 'jank'")"
  fi
  echo "{\"fps\":null,\"jank_frames\":$jank}"
}

sample_idle_objects() {
  # Live datapath object counts. The extension publishes its counter snapshot
  # to `phantom_vpn_stats.txt` inside the app sandbox; reading it from a shell
  # needs a debuggable build (`hdc shell` as root). Without that this is null.
  if [[ -n "${PHANTOM_IDLE_JSON:-}" && -r "${PHANTOM_IDLE_JSON}" ]]; then
    cat "$PHANTOM_IDLE_JSON"
  else
    echo '{"flows":null,"pool_idle":null,"udp_flows":null}'
  fi
}

# --- run ----------------------------------------------------------------------

SAMPLES=$(( DURATION / INTERVAL ))
[[ "$SAMPLES" -lt 1 ]] && SAMPLES=1

OUT_FILE="$OUT_DIR/harmony-$SCENARIO-$STAMP.json"
{
  echo "{"
  echo "  \"scenario\": \"$SCENARIO\","
  echo "  \"platform\": \"harmony\","
  echo "  \"duration_s\": $DURATION,"
  echo "  \"interval_s\": $INTERVAL,"
  echo "  \"pids\": [$PIDS_JSON],"
  echo "  \"probe\": {"
  echo "    \"available\": [$(csv "$AVAILABLE_JSON")],"
  echo "    \"missing\": [$(csv "$MISSING_JSON")]"
  echo "  },"
  echo "  \"samples\": ["
  for i in $(seq 1 "$SAMPLES"); do
    if [[ -z "$(hdc shell "pidof $BUNDLE" 2>/dev/null | tr -d '\r' || true)" ]]; then
      echo "ERROR: $BUNDLE disappeared mid-run (crashed or was killed)." >&2
      exit 1
    fi

    CPU_PAIRS="$(sample_cpu_pairs)"
    MEM_PAIRS="$(sample_mem_pairs)"
    CPU_ARRAY="$(sed -n '1p' <<<"$CPU_PAIRS")"
    MEM_ARRAY="$(sed -n '1p' <<<"$MEM_PAIRS")"
    # "Not measurable" is spelled `null` in the schema, never `0` or `[]`.
    [[ "$CPU_ARRAY" == "null" ]] && CPU_ARRAY_JSON=null || CPU_ARRAY_JSON="$CPU_ARRAY"
    [[ "$MEM_ARRAY" == "null" ]] && MEM_ARRAY_JSON=null || MEM_ARRAY_JSON="$MEM_ARRAY"
    CPU_TOTAL_JSON="$(num "$(sed -n '2p' <<<"$CPU_PAIRS")")"
    MEM_TOTAL_JSON="$(num "$(sed -n '2p' <<<"$MEM_PAIRS")")"

    COMMA=","
    [[ "$i" == "$SAMPLES" ]] && COMMA=""
    echo "    {"
    echo "      \"t\": $(date +%s),"
    echo "      \"cpu_percent\": $CPU_TOTAL_JSON,"
    echo "      \"cpu_breakdown\": $CPU_ARRAY_JSON,"
    echo "      \"pss_kb\": $MEM_TOTAL_JSON,"
    echo "      \"mem_breakdown\": $MEM_ARRAY_JSON,"
    echo "      \"net_bytes\": $(sample_net),"
    echo "      \"net_conns\": $(sample_net_conns),"
    echo "      \"power\": $(sample_power),"
    echo "      \"render\": $(sample_render),"
    echo "      \"idle_objects\": $(sample_idle_objects)"
    echo "    }${COMMA}"
    [[ "$i" == "$SAMPLES" ]] || sleep "$INTERVAL"
  done
  echo "  ]"
  echo "}"
} > "$OUT_FILE"

echo "Wrote $OUT_FILE (pids=[${PIDS// /, }] samples=$SAMPLES)" >&2
cat "$OUT_FILE"
