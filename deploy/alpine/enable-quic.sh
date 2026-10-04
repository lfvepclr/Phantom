#!/bin/sh
# Run a second Phantom server instance on the same port, over QUIC.
#
# Why a second instance instead of one process speaking both: the server picks
# a single transport from `[quic] enable`, and TCP:443 / UDP:443 do not
# conflict. Two instances on the same key material mean one phantom:// URI can
# be measured twice — `proto=tcp` against one, `proto=quic` against the other —
# which is what the weak-network A/B needs.
#
# Usage (on the server, as root):
#   sh enable-quic.sh
#   rc-service phantom-quic status
#   sh enable-quic.sh --disable     # stop it and remove it from the runlevel
#
# The QUIC instance reuses /etc/conf.d/phantom for everything except the
# protocol, so port, public host, cipher and user stay in one place.

set -e

SRC_INIT="/etc/init.d/phantom"
DST_INIT="/etc/init.d/phantom-quic"
SRC_CONF="/etc/conf.d/phantom"
DST_CONF="/etc/conf.d/phantom-quic"

if [ "$(id -u)" -ne 0 ]; then
    echo "ERROR: run this as root on the server." >&2
    exit 1
fi

if [ "$1" = "--disable" ]; then
    rc-service phantom-quic stop 2>/dev/null || true
    rc-update del phantom-quic default 2>/dev/null || true
    rm -f "$DST_INIT" "$DST_CONF"
    echo "phantom-quic disabled and removed."
    exit 0
fi

command -v phantom >/dev/null 2>&1 || {
    echo "ERROR: phantom is not installed (/usr/local/bin/phantom)." >&2
    exit 1
}
[ -f "$SRC_INIT" ] || { echo "ERROR: $SRC_INIT missing (install the server first)." >&2; exit 1; }
[ -f "$SRC_CONF" ] || { echo "ERROR: $SRC_CONF missing." >&2; exit 1; }

# Same settings as the TCP instance, protocol overridden. `sed` keeps the
# "user edited this file" comments and any non-default values intact.
sed 's/^PHANTOM_PROTO=.*/PHANTOM_PROTO="quic"/' "$SRC_CONF" > "$DST_CONF"
grep -q '^PHANTOM_PROTO="quic"' "$DST_CONF" || echo 'PHANTOM_PROTO="quic"' >> "$DST_CONF"

ln -sf "$SRC_INIT" "$DST_INIT"
chmod +x "$DST_INIT"

rc-update add phantom-quic default
rc-service phantom-quic start
sleep 2
rc-service phantom-quic status || true

echo
echo "phantom-quic is listening on udp/<port> with the same key material."
echo "The client picks it with proto=quic in the quick link (or the"
echo "HarmonyOS settings switch); no URI change is needed because the host"
echo "and port are the same."
