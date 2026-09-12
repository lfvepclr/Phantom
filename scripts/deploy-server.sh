#!/usr/bin/env bash
# Upload a Phantom server bundle and install it on an Alpine/OpenRC host.
#
# The remote host NEVER compiles: it only unpacks a prebuilt static binary and
# (re)writes its OpenRC service files. See deploy/alpine/install.sh.
#
# Usage: deploy-server.sh <bundle.tar.gz> <host> [port] [proto] [public-host]
#   e.g. deploy-server.sh dist/phantom-server-0.1.0-linux-amd64.tar.gz \
#          root@203.0.113.10 443 tcp 203.0.113.10

set -euo pipefail

TARBALL="${1:-}"
HOST="${2:-}"
PORT="${3:-443}"
PROTO="${4:-tcp}"
PUBLIC_HOST="${5:-}"

if [[ -z "$TARBALL" || -z "$HOST" ]]; then
    sed -n '2,10p' "$0"
    exit 2
fi
[[ -f "$TARBALL" ]] || { echo "ERROR: bundle not found: $TARBALL" >&2; exit 1; }

BASE="$(basename "$TARBALL")"
NAME="${BASE%.tar.gz}"
REMOTE_DIR="/tmp/$NAME"

echo "==> Uploading $BASE to $HOST"
scp -q -o BatchMode=yes "$TARBALL" "$HOST:/tmp/$BASE"

echo "==> Installing (no compiler involved)"
# The tarball contains a single top-level directory named after the bundle.
ssh -o BatchMode=yes "$HOST" \
    "set -e; rm -rf '$REMOTE_DIR'; mkdir -p '$REMOTE_DIR'; \
     tar xzf '/tmp/$BASE' -C '$REMOTE_DIR'; cd '$REMOTE_DIR/$NAME'; \
     PHANTOM_PORT='$PORT' PHANTOM_PROTO='$PROTO' PHANTOM_PUBLIC_HOST='$PUBLIC_HOST' sh install.sh"

echo
echo "==> Host checks"
ssh -o BatchMode=yes "$HOST" "
    rc-service phantom status 2>&1 | head -2
    echo -n 'listening: '
    netstat -tln 2>/dev/null | grep ':${PORT} ' | head -1 || echo 'NOT LISTENING'
    echo -n 'linkage:   '
    ldd /usr/local/bin/phantom 2>&1 | head -1
    echo -n 'autostart: '
    rc-update show default 2>/dev/null | grep phantom || echo 'not enabled'
    echo -n 'memory:    '
    ps -o rss= -C phantom 2>/dev/null | awk '{printf \"%.1f MiB\\n\", \$1/1024}' || echo n/a
"

echo
echo "Server installed. The phantom:// URI is printed above by install.sh;"
echo "retrieve it any time with:"
echo "  ssh $HOST \"sed -n 's|^#[[:space:]]*\\(phantom://.*\\)\$|\\1|p' /var/lib/phantom/server.toml | head -1\""
