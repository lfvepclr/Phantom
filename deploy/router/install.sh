#!/usr/bin/env bash
# Push the Phantom router client to an ASUS router over SSH.
#
# Usage:
#   bash deploy/router/install.sh <router-host> "<phantom:// URI>" [ssh-user]
#
# Example:
#   bash deploy/router/install.sh <路由器IP> "phantom://KEY@vpn.example.com:443?cipher=auto"
#
# Prerequisites on the router (Asuswrt-Merlin):
#   - SSH enabled           (Administration -> System -> Enable SSH)
#   - JFFS enabled + custom scripts enabled (same page)
# The script verifies both before copying anything.

set -euo pipefail

ROUTER_HOST="${1:-}"
PHANTOM_URI="${2:-}"
SSH_USER="${3:-admin}"

if [[ -z "$ROUTER_HOST" || -z "$PHANTOM_URI" ]]; then
    echo "Usage: $0 <router-host> \"<phantom:// URI>\" [ssh-user]" >&2
    exit 1
fi
if [[ "$PHANTOM_URI" != phantom://* ]]; then
    echo "Error: URI must start with phantom:// (got: ${PHANTOM_URI:0:16}...)" >&2
    exit 1
fi

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
TARGET="aarch64-unknown-linux-musl"
BINARY="$REPO_ROOT/target/$TARGET/release/phantom"
WRAPPER="$REPO_ROOT/deploy/router/phantom.sh"
REMOTE_DIR="/jffs/phantom"
SSH_TARGET="$SSH_USER@$ROUTER_HOST"

if [[ ! -f "$BINARY" ]]; then
    echo "Error: router binary not found at" >&2
    echo "  $BINARY" >&2
    echo "Build it first:  cargo xtask build router" >&2
    exit 1
fi

echo "==> Checking the router environment"
ssh "$SSH_TARGET" 'sh -s' <<'REMOTE_CHECK'
set -e
[ -d /jffs ] || { echo "ERROR: /jffs is not mounted. Enable JFFS partition in the web UI." >&2; exit 1; }
[ -w /jffs ] || { echo "ERROR: /jffs is not writable." >&2; exit 1; }
if [ ! -d /jffs/scripts ]; then
    echo "ERROR: /jffs/scripts missing. Enable 'Format JFFS partition' + 'Enable JFFS custom scripts and configs' in Administration -> System." >&2
    exit 1
fi
uname -m
for tool in ip iptables; do
    command -v "$tool" >/dev/null 2>&1 || { echo "ERROR: $tool not found on the router." >&2; exit 1; }
done
echo "router environment OK"
REMOTE_CHECK

REMOTE_ARCH="$(ssh "$SSH_TARGET" 'uname -m')"
if [[ "$REMOTE_ARCH" != "aarch64" && "$REMOTE_ARCH" != "arm64" ]]; then
    echo "Error: router reports arch '$REMOTE_ARCH' but the binary is $TARGET." >&2
    echo "For 32-bit ARM models build with --target armv7-unknown-linux-musleabihf instead." >&2
    exit 1
fi

echo "==> Copying files to $REMOTE_DIR"
ssh "$SSH_TARGET" "mkdir -p '$REMOTE_DIR'"
scp -q "$BINARY" "$SSH_TARGET:$REMOTE_DIR/phantom.new"
scp -q "$WRAPPER" "$SSH_TARGET:$REMOTE_DIR/phantom.sh"

echo "==> Writing configuration and startup hooks"
# PHANTOM_URI is passed through the environment so the URI never lands in the
# router's shell history or in a process argument list.
PHANTOM_URI="$PHANTOM_URI" ssh "$SSH_TARGET" "PHANTOM_URI='$PHANTOM_URI' sh -s" <<'REMOTE_INSTALL'
set -e
REMOTE_DIR="/jffs/phantom"

# Stop any previous instance before swapping the binary out from under it.
if [ -x "$REMOTE_DIR/phantom.sh" ] && [ -f "$REMOTE_DIR/phantom.conf" ]; then
    "$REMOTE_DIR/phantom.sh" stop 2>/dev/null || true
fi
mv "$REMOTE_DIR/phantom.new" "$REMOTE_DIR/phantom"
chmod 755 "$REMOTE_DIR/phantom" "$REMOTE_DIR/phantom.sh"

if [ -f "$REMOTE_DIR/phantom.conf" ]; then
    # Preserve local tuning; only refresh the server URI.
    sed -i "s|^PHANTOM_URI=.*|PHANTOM_URI='$PHANTOM_URI'|" "$REMOTE_DIR/phantom.conf"
    echo "  updated PHANTOM_URI in existing phantom.conf"
else
    cat >"$REMOTE_DIR/phantom.conf" <<CONF
# Phantom router client configuration.
PHANTOM_URI='$PHANTOM_URI'

# TUN device. The address must not overlap the LAN subnet.
TUN_NAME='phantom0'
TUN_ADDR='10.7.0.1/24'

# LAN interface(s) whose forwarded traffic is tunnelled. Space-separated.
# br0 is the default LAN bridge on Asuswrt; add br1 etc. for guest networks.
LAN_IF='br0'

# Routing table holding the tunnel default route.
TABLE_ID='200'

# Redirect LAN port-53 traffic into the tunnel so domain rules work.
# Set to 0 to keep resolving through the router's dnsmasq (local hostnames
# keep working, but domain-based routing rules stop matching LAN clients).
LAN_DNS_HIJACK='1'

RUST_LOG='info'
CONF
    chmod 600 "$REMOTE_DIR/phantom.conf"
    echo "  created $REMOTE_DIR/phantom.conf"
fi

# services-start runs late in boot, after the LAN bridge exists.
HOOK="/jffs/scripts/services-start"
[ -f "$HOOK" ] || printf '#!/bin/sh\n' >"$HOOK"
if ! grep -q '/jffs/phantom/phantom.sh' "$HOOK"; then
    printf '%s\n' '/jffs/phantom/phantom.sh start &   # phantom' >>"$HOOK"
    echo "  added phantom to $HOOK"
fi
chmod 755 "$HOOK"

# Asuswrt rebuilds the iptables rule set on nat-start, which drops the
# FORWARD/DNAT entries the gateway installed. Restarting reinstalls them.
NAT_HOOK="/jffs/scripts/nat-start"
[ -f "$NAT_HOOK" ] || printf '#!/bin/sh\n' >"$NAT_HOOK"
if ! grep -q '/jffs/phantom/phantom.sh' "$NAT_HOOK"; then
    printf '%s\n' '/jffs/phantom/phantom.sh restart &   # phantom: nat-start flushes iptables' >>"$NAT_HOOK"
    echo "  added phantom to $NAT_HOOK"
fi
chmod 755 "$NAT_HOOK"
REMOTE_INSTALL

echo "==> Starting phantom"
ssh "$SSH_TARGET" "$REMOTE_DIR/phantom.sh start"

echo
echo "==> Status"
ssh "$SSH_TARGET" "$REMOTE_DIR/phantom.sh status" || true

cat <<EOF

Installed. Useful commands:

  ssh $SSH_TARGET $REMOTE_DIR/phantom.sh status
  ssh $SSH_TARGET $REMOTE_DIR/phantom.sh log 100
  ssh $SSH_TARGET $REMOTE_DIR/phantom.sh restart
  ssh $SSH_TARGET $REMOTE_DIR/phantom.sh stop

Tune $REMOTE_DIR/phantom.conf on the router, then restart.
EOF
