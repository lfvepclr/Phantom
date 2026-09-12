#!/usr/bin/env bash
# Phantom throughput / unblock speed test.
#
# Three vantage points:
#   --loopback   client + server + origin all local  -> client software ceiling
#   --uri ...    through a deployed server           -> real tunnel throughput
#   --vps-host   bare ssh link to that host          -> physical link baseline
#
# The unblock checks only run with --check-unblock (they need a working tunnel):
#   google/gstatic generate_204, cloudflare cdn-cgi/trace (asserts the exit IP
#   and country) and youtube.
#
# Usage:
#   speedtest.sh --uri "phantom://..." [--rounds N] [--origin cloudflare|vps]
#                [--check-unblock] [--socks-port 1080] [--vps-host root@HOST]
#   speedtest.sh --loopback [--rounds N]

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLIENT="$REPO_ROOT/target/release/phantom"
ROUNDS=3
ORIGIN="cloudflare"
SOCKS_PORT=1080
URI=""
LOOPBACK=0
CHECK_UNBLOCK=0
VPS_HOST=""
WORK="$(mktemp -d "${TMPDIR:-/tmp}/phantom-speed.XXXXXX")"

while [[ $# -gt 0 ]]; do
    case "$1" in
        --uri) URI="${2:?}"; shift 2 ;;
        --rounds) ROUNDS="${2:?}"; shift 2 ;;
        --origin) ORIGIN="${2:?}"; shift 2 ;;
        --socks-port) SOCKS_PORT="${2:?}"; shift 2 ;;
        --loopback) LOOPBACK=1; shift ;;
        --check-unblock) CHECK_UNBLOCK=1; shift ;;
        --vps-host) VPS_HOST="${2:?}"; shift 2 ;;
        -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
        *) echo "unknown flag: $1" >&2; exit 2 ;;
    esac
done

[[ -x "$CLIENT" ]] || { echo "ERROR: $CLIENT not found — run: cargo build --release -p phantom-cli" >&2; exit 1; }
PROXY="socks5h://127.0.0.1:$SOCKS_PORT"
CLIENT_PID=""
SERVER_PID=""
ORIGIN_PID=""
REMOTE_ORIGIN=""

cleanup() {
    set +e
    [[ -n "$CLIENT_PID" ]] && kill "$CLIENT_PID" >/dev/null 2>&1
    [[ -n "$SERVER_PID" ]] && kill "$SERVER_PID" >/dev/null 2>&1
    [[ -n "$ORIGIN_PID" ]] && kill "$ORIGIN_PID" >/dev/null 2>&1
    if [[ -n "$REMOTE_ORIGIN" ]]; then
        ssh -o BatchMode=yes "$VPS_HOST" "pkill -f '/tmp/phantom-origin' || true" >/dev/null 2>&1
    fi
    rm -rf "$WORK"
}
trap cleanup EXIT

wait_for_port() {
    local port="$1" tries="${2:-40}"
    for _ in $(seq 1 "$tries"); do
        nc -z 127.0.0.1 "$port" 2>/dev/null && return 0
        sleep 0.5
    done
    return 1
}

start_tunnel() {
    local uri="$1"
    RUST_LOG=warn "$CLIENT" client --server "$uri" >"$WORK/client.log" 2>&1 &
    CLIENT_PID=$!
    wait_for_port "$SOCKS_PORT" || { echo "ERROR: SOCKS5 never came up"; tail -20 "$WORK/client.log"; exit 1; }
}

median() { sort -n | awk '{a[NR]=$1} END{ if(NR%2){print a[(NR+1)/2]} else {print (a[NR/2]+a[NR/2+1])/2} }'; }
fmt() { awk '{printf "%.2f MB/s (%.1f Mbps)", $1/1048576, $1*8/1000000}'; }

# ── Loopback: no remote server involved ──────────────────────────────────────
run_loopback() {
    echo "=== Loopback: client + server + origin on this machine ==="
    local dir="$WORK/loopback"
    mkdir -p "$dir/www"
    # 50 MiB deterministic payload served by python3 on 127.0.0.1:18080.
    head -c 52428800 /dev/urandom > "$dir/www/50mb.bin"
    ( cd "$dir/www" && python3 -m http.server 18080 --bind 127.0.0.1 >"$dir/origin.log" 2>&1 ) &
    ORIGIN_PID=$!
    wait_for_port 18080 || { echo "ERROR: local origin failed to start"; exit 1; }

    ( cd "$dir" && RUST_LOG=warn "$CLIENT" server --port 18443 --public-host 127.0.0.1 \
        >"$dir/server.log" 2>&1 ) &
    SERVER_PID=$!
    local uri=""
    for _ in $(seq 1 30); do
        uri="$(sed -n 's|^#[[:space:]]*\(phantom://.*\)$|\1|p' "$dir/server.toml" 2>/dev/null | head -1)"
        [[ -n "$uri" ]] && break
        sleep 1
    done
    [[ -n "$uri" ]] || { echo "ERROR: local server never bootstrapped"; tail -20 "$dir/server.log"; exit 1; }
    echo "    loopback URI: $uri"

    start_tunnel "$uri"
    echo "--- download ${ROUNDS}x 50 MiB (client software ceiling) ---"
    rm -f "$WORK/loop-dl.txt"
    for i in $(seq 1 "$ROUNDS"); do
        curl -s --max-time 300 -x "$PROXY" -o /dev/null \
            -w '%{speed_download}\n' http://127.0.0.1:18080/50mb.bin | tee -a "$WORK/loop-dl.txt" | fmt
        echo "  (round $i)"
    done
    echo "--- median ---"
    echo "    $(median < "$WORK/loop-dl.txt" | fmt)"
}

# ── Through a deployed server ────────────────────────────────────────────────
run_remote() {
    echo "=== Tunnel through deployed server ==="
    echo "    URI: ${URI%%#*}#..."
    start_tunnel "$URI"

    if [[ "$CHECK_UNBLOCK" == "1" ]]; then
        echo
        echo "--- unblock evidence (through the tunnel) ---"
        local g
        g="$(curl -s --max-time 20 -x "$PROXY" -o /dev/null -w '%{http_code}' https://www.google.com/generate_204 || echo 000)"
        echo "    google/generate_204     -> HTTP $g (expect 204)"
        g="$(curl -s --max-time 20 -x "$PROXY" -o /dev/null -w '%{http_code}' https://www.gstatic.com/generate_204 || echo 000)"
        echo "    gstatic/generate_204    -> HTTP $g (expect 204)"
        g="$(curl -s --max-time 25 -x "$PROXY" -o /dev/null -w '%{http_code}' https://www.youtube.com/ || echo 000)"
        echo "    youtube.com             -> HTTP $g (expect 200)"
        echo -n "    cloudflare trace        -> "
        curl -s --max-time 20 -x "$PROXY" https://www.cloudflare.com/cdn-cgi/trace 2>/dev/null \
            | awk -F= '/^(ip|loc|colo)=/{printf "%s=%s ", $1, $2} END{print ""}' || echo "(unreachable)"
        echo
    fi

    if [[ "$ORIGIN" == "vps" && -n "$VPS_HOST" ]]; then
        echo "--- origin: VPS-side static file (no third party) ---"
        local origin_bin="$REPO_ROOT/target/x86_64-unknown-linux-musl/release/phantom-origin"
        if [[ ! -x "$origin_bin" ]]; then
            rustc --edition 2021 -O --target x86_64-unknown-linux-musl \
                "$REPO_ROOT/tests/e2e/minihttpd.rs" -o "$origin_bin"
        fi
        scp -q -o BatchMode=yes "$origin_bin" "$VPS_HOST:/tmp/phantom-origin"
        ssh -o BatchMode=yes "$VPS_HOST" \
            "mkdir -p /tmp/phantom-www && head -c 10000000 /dev/urandom > /tmp/phantom-www/10mb.bin; \
             pkill -f /tmp/phantom-origin 2>/dev/null; \
             setsid /tmp/phantom-origin /tmp/phantom-www >/tmp/phantom-origin.log 2>&1 < /dev/null & \
             sleep 1; echo started"
        REMOTE_ORIGIN=1
        rm -f "$WORK/vps-dl.txt"
        for i in $(seq 1 "$ROUNDS"); do
            curl -s --max-time 180 -x "$PROXY" -o /dev/null \
                -w '%{speed_download}\n' http://127.0.0.1:8080/10mb.bin | tee -a "$WORK/vps-dl.txt" | fmt
            echo "  (round $i)"
        done
        echo "    median: $(median < "$WORK/vps-dl.txt" | fmt)"
        echo "    VPS-side origin is stopped automatically on exit."
    else
        echo "--- origin: Cloudflare speed endpoint (through the tunnel) ---"
        rm -f "$WORK/cf-dl.txt" "$WORK/cf-ul.txt"
        for i in $(seq 1 "$ROUNDS"); do
            curl -s --max-time 180 -x "$PROXY" -o /dev/null \
                -w '%{speed_download}\n' "https://speed.cloudflare.com/__down?bytes=20000000" \
                | tee -a "$WORK/cf-dl.txt" | fmt
            echo "  (download round $i)"
        done
        echo "    download median: $(median < "$WORK/cf-dl.txt" | fmt)"

        head -c 5000000 /dev/urandom > "$WORK/up.bin"
        for i in $(seq 1 "$ROUNDS"); do
            curl -s --max-time 180 -x "$PROXY" -o /dev/null \
                -w '%{speed_upload}\n' --data-binary @"$WORK/up.bin" https://speed.cloudflare.com/__up \
                | tee -a "$WORK/cf-ul.txt" | fmt
            echo "  (upload round $i)"
        done
        echo "    upload median:   $(median < "$WORK/cf-ul.txt" | fmt)"
    fi
}

# ── Bare link baseline (ssh only, no tunnel) ─────────────────────────────────
run_bare_link() {
    [[ -n "$VPS_HOST" ]] || return 0
    echo
    echo "=== Bare link baseline (ssh, no tunnel) ==="
    local t0 t1 bytes bps
    t0=$(date +%s.%N)
    ssh -o BatchMode=yes "$VPS_HOST" 'dd if=/dev/zero bs=1M count=10 2>/dev/null' | dd of=/dev/null bs=1M 2>/dev/null
    t1=$(date +%s.%N)
    bytes=10485760
    bps=$(awk -v b="$bytes" -v s="$(awk -v a="$t0" -v b="$t1" 'BEGIN{print b-a}')" 'BEGIN{print b/s}')
    echo "    server -> this machine: $(echo "$bps" | fmt)"
}

if [[ "$LOOPBACK" == "1" ]]; then
    run_loopback
else
    [[ -n "$URI" ]] || { echo "ERROR: --uri is required (or use --loopback)" >&2; exit 2; }
    run_remote
    run_bare_link
fi

echo
echo "=== done ==="
