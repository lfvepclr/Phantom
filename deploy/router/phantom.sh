#!/bin/sh
# Phantom router client — start/stop wrapper for Asuswrt / Asuswrt-Merlin.
#
# Installed to /jffs/phantom/phantom.sh by deploy/router/install.sh.
# Configuration lives in /jffs/phantom/phantom.conf.
#
# POSIX sh only: the router's shell is BusyBox ash, not bash.

set -e

PHANTOM_DIR="/jffs/phantom"
BIN="$PHANTOM_DIR/phantom"
CONF="$PHANTOM_DIR/phantom.conf"
PIDFILE="$PHANTOM_DIR/phantom.pid"
LOGFILE="$PHANTOM_DIR/phantom.log"

[ -r "$CONF" ] || { echo "phantom: missing $CONF" >&2; exit 1; }
# shellcheck disable=SC1090
. "$CONF"

: "${PHANTOM_URI:?PHANTOM_URI must be set in $CONF}"
: "${TUN_NAME:=phantom0}"
: "${TUN_ADDR:=10.7.0.1/24}"
: "${LAN_IF:=br0}"
: "${TABLE_ID:=200}"
: "${LISTEN:=127.0.0.1:1080}"
: "${LAN_DNS_HIJACK:=1}"
: "${RUST_LOG:=info}"

is_running() {
    [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null
}

build_args() {
    set -- client --server "$PHANTOM_URI" \
        --tun --tun-name "$TUN_NAME" --tun-addr "$TUN_ADDR" \
        --gateway --table "$TABLE_ID"
    for iface in $LAN_IF; do
        set -- "$@" --lan-interface "$iface"
    done
    if [ "$LAN_DNS_HIJACK" != "1" ]; then
        set -- "$@" --no-lan-dns-hijack
    fi
    echo "$@"
}

start() {
    if is_running; then
        echo "phantom: already running (pid $(cat "$PIDFILE"))"
        return 0
    fi

    [ -x "$BIN" ] || { echo "phantom: $BIN is not executable" >&2; exit 1; }
    # OpenVPN ships the tun module on Asuswrt; load it in case nothing else has.
    [ -c /dev/net/tun ] || modprobe tun 2>/dev/null || true
    [ -c /dev/net/tun ] || { echo "phantom: /dev/net/tun unavailable" >&2; exit 1; }

    echo "phantom: starting ($(date))" >>"$LOGFILE"
    # shellcheck disable=SC2046
    RUST_LOG="$RUST_LOG" "$BIN" $(build_args) >>"$LOGFILE" 2>&1 &
    echo $! >"$PIDFILE"

    # Give the Hello verification a moment so a bad URI fails visibly here
    # rather than silently in the log.
    sleep 3
    if is_running; then
        echo "phantom: started (pid $(cat "$PIDFILE"))"
    else
        rm -f "$PIDFILE"
        echo "phantom: failed to start, last log lines:" >&2
        tail -n 20 "$LOGFILE" >&2
        exit 1
    fi
}

stop() {
    if ! is_running; then
        echo "phantom: not running"
        rm -f "$PIDFILE"
        return 0
    fi
    pid=$(cat "$PIDFILE")
    # Phantom traps SIGTERM and unwinds, which is what reverts the ip rule /
    # iptables changes. Give it time before escalating.
    kill "$pid" 2>/dev/null || true
    i=0
    while [ $i -lt 50 ] && kill -0 "$pid" 2>/dev/null; do
        sleep 0.1 2>/dev/null || sleep 1
        i=$((i + 1))
    done
    if kill -0 "$pid" 2>/dev/null; then
        echo "phantom: did not exit in time, sending SIGKILL" >&2
        kill -9 "$pid" 2>/dev/null || true
        # SIGKILL skips the Drop handler, so clean the rules up by hand.
        cleanup_rules
    fi
    rm -f "$PIDFILE"
    echo "phantom: stopped"
}

# Best-effort removal of rules left behind by a SIGKILL or a crash.
cleanup_rules() {
    for iface in $LAN_IF; do
        while ip rule del iif "$iface" lookup "$TABLE_ID" 2>/dev/null; do :; done
        while ip rule del iif "$iface" lookup main 2>/dev/null; do :; done
    done
    ip route flush table "$TABLE_ID" 2>/dev/null || true
}

status() {
    if is_running; then
        echo "phantom: running (pid $(cat "$PIDFILE"))"
        echo "--- ip rule ---"
        ip rule show 2>/dev/null | grep -E "lookup (main|$TABLE_ID)" || true
        echo "--- table $TABLE_ID ---"
        ip route show table "$TABLE_ID" 2>/dev/null || true
        echo "--- metrics ---"
        curl -s --max-time 2 http://127.0.0.1:9150/metrics 2>/dev/null | head -12 || \
            echo "(metrics endpoint unavailable)"
    else
        echo "phantom: not running"
        return 1
    fi
}

case "$1" in
    start)   start ;;
    stop)    stop ;;
    restart) stop; sleep 1; start ;;
    status)  status ;;
    log)     tail -n "${2:-50}" "$LOGFILE" ;;
    *)       echo "Usage: $0 {start|stop|restart|status|log [lines]}" >&2; exit 1 ;;
esac
