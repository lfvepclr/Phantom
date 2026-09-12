#!/usr/bin/env bash
# Turn the macOS system SOCKS5 proxy on/off for the Phantom client.
#
# The app does this itself without any privileges (networksetup works for a
# normal user — verified). This script is for doing it by hand, e.g. from a
# terminal client that does not set the proxy.
#
# Usage:
#   bash scripts/mac-sysproxy.sh on  [port]      # default port 11080
#   bash scripts/mac-sysproxy.sh off
#   bash scripts/mac-sysproxy.sh status
#
# Remember to run `off` before quitting a terminal client, otherwise browsers
# will keep pointing at a dead listener. The menu-bar app restores the previous
# state itself on Stop.

set -euo pipefail

ACTION="${1:-status}"
PORT="${2:-11080}"
HOST="127.0.0.1"

# Active network service = the interface holding the default route.
active_service() {
    iface="$(route -n get default 2>/dev/null | awk '/interface:/{print $2}')"
    if [[ -z "$iface" ]]; then echo "Wi-Fi"; return; fi
    networksetup -listnetworkserviceorder \
        | awk -v dev="$iface" '
            /^\([0-9]+\)/ { svc = substr($0, index($0, ")") + 2) }
            /Device: / { if (index($0, dev ":") > 0) { print svc; exit } }' \
        | sed 's/[[:space:]]*$//'
}

SERVICE="$(active_service)"
[[ -n "$SERVICE" ]] || SERVICE="Wi-Fi"

case "$ACTION" in
    on)
        networksetup -setsocksfirewallproxy "$SERVICE" "$HOST" "$PORT"
        networksetup -setsocksfirewallproxystate "$SERVICE" on
        echo "System SOCKS5 proxy enabled on '$SERVICE' -> $HOST:$PORT"
        networksetup -getsocksfirewallproxy "$SERVICE" | head -3
        ;;
    off)
        networksetup -setsocksfirewallproxystate "$SERVICE" off
        echo "System SOCKS5 proxy disabled on '$SERVICE'"
        networksetup -getsocksfirewallproxy "$SERVICE" | head -3
        ;;
    status)
        echo "service: $SERVICE"
        networksetup -getsocksfirewallproxy "$SERVICE" | head -4
        ;;
    *)
        sed -n '2,16p' "$0"
        exit 2
        ;;
esac
