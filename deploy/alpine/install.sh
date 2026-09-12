#!/bin/sh
# Phantom server installer for Alpine Linux / OpenRC — no compiler, no network.
#
# Usage (as root, inside an extracted phantom-server-<version>-linux-<arch>):
#   PHANTOM_PORT=443 PHANTOM_PROTO=tcp PHANTOM_PUBLIC_HOST=203.0.113.7 sh install.sh
#
# Idempotent: ./server.key is never deleted, so re-installing keeps the same
# public key and the same phantom:// URI already distributed to clients.

set -eu

PKG_BIN="phantom"
PKG_INITD="phantom.initd"
PKG_CONFD="phantom.confd"

PREFIX="/usr/local/bin"
STATE_DIR="/var/lib/phantom"
CONF_D="/etc/conf.d/phantom"
INIT_D="/etc/init.d/phantom"
LOG_FILE="/var/log/phantom.log"
SERVICE="phantom"

PHANTOM_PORT="${PHANTOM_PORT:-443}"
PHANTOM_PROTO="${PHANTOM_PROTO:-tcp}"
PHANTOM_CIPHER="${PHANTOM_CIPHER:-auto}"
PHANTOM_PUBLIC_HOST="${PHANTOM_PUBLIC_HOST:-}"
PHANTOM_USER="${PHANTOM_USER:-phantom}"

log() { printf '==> %s\n' "$*"; }
warn() { printf 'WARN: %s\n' "$*" >&2; }
die() { printf 'ERROR: %s\n' "$*" >&2; exit 1; }

# ── Preconditions ────────────────────────────────────────────────────────────
[ "$(id -u)" = "0" ] || die "must run as root"
[ -f "./$PKG_BIN" ] || die "./$PKG_BIN not found — extract the bundle and run install.sh from inside it"
[ -f "./$PKG_INITD" ] || die "./$PKG_INITD not found"
[ -f "./$PKG_CONFD" ] || die "./$PKG_CONFD not found"
command -v rc-service >/dev/null 2>&1 || die "OpenRC not found (rc-service missing) — this installer targets Alpine/OpenRC hosts"

ARCH="$(uname -m)"
case "$ARCH" in
	x86_64|aarch64|armv7l) ;;
	*) die "unsupported architecture: $ARCH" ;;
esac
log "Installing Phantom server ($ARCH, port $PHANTOM_PORT/$PHANTOM_PROTO)"

# Dynamic-linking check: a static musl binary is the only thing that reliably
# runs across Alpine versions, so refuse anything else loudly.
if command -v ldd >/dev/null 2>&1; then
	if ldd "./$PKG_BIN" 2>/dev/null | grep -q "=>"; then
		die "./$PKG_BIN looks dynamically linked; expected a static musl build"
	fi
fi

# ── Service account ──────────────────────────────────────────────────────────
if ! getent group "$PHANTOM_USER" >/dev/null 2>&1; then
	log "Creating group $PHANTOM_USER"
	addgroup -S "$PHANTOM_USER" || die "addgroup failed"
fi
if ! id -u "$PHANTOM_USER" >/dev/null 2>&1; then
	log "Creating system user $PHANTOM_USER"
	# busybox adduser; -H skips creating the home dir (we create it ourselves).
	adduser -S -D -H -h "$STATE_DIR" -s /sbin/nologin -G "$PHANTOM_USER" "$PHANTOM_USER" \
		|| adduser -S -D -H -h "$STATE_DIR" -s /sbin/nologin "$PHANTOM_USER" \
		|| die "adduser failed"
fi

# ── State directory (server.key / server.toml live here) ─────────────────────
mkdir -p "$STATE_DIR"
chown "$PHANTOM_USER:$PHANTOM_USER" "$STATE_DIR"
chmod 750 "$STATE_DIR"
if [ -f "$STATE_DIR/server.key" ]; then
	log "Existing server.key preserved (public key and URI stay unchanged)"
fi

# ── Swap the binary and (re)write the service definition ─────────────────────
if [ -x "$INIT_D" ] && rc-service "$SERVICE" status >/dev/null 2>&1; then
	log "Stopping running service before replacing the binary"
	rc-service "$SERVICE" stop >/dev/null 2>&1 || true
fi

install -m 0755 "./$PKG_BIN" "$PREFIX/$SERVICE.new" || die "install failed"
mv -f "$PREFIX/$SERVICE.new" "$PREFIX/$SERVICE"
log "Installed $PREFIX/$SERVICE"

cat > "$CONF_D" <<EOF
# Managed by deploy/alpine/install.sh — edit then: rc-service phantom restart
PHANTOM_PORT="$PHANTOM_PORT"
PHANTOM_PROTO="$PHANTOM_PROTO"
PHANTOM_CIPHER="$PHANTOM_CIPHER"
PHANTOM_PUBLIC_HOST="$PHANTOM_PUBLIC_HOST"
PHANTOM_USER="$PHANTOM_USER"
EOF
chmod 0644 "$CONF_D"

install -m 0755 "./$PKG_INITD" "$INIT_D"

# ── Start + readiness probe ──────────────────────────────────────────────────
start_service() {
	rc-service "$SERVICE" restart >/dev/null 2>&1 \
		|| rc-service "$SERVICE" start >/dev/null 2>&1 \
		|| true
}

# The server binds $PHANTOM_PORT, or walks upward when it is busy; poll the
# port it actually recorded in server.toml.
bound_port() {
	sed -n 's/^bind *= *"0\.0\.0\.0:\([0-9]*\)".*/\1/p' "$STATE_DIR/server.toml" 2>/dev/null | head -1
}

wait_for_listen() {
	_i=0
	while [ "$_i" -lt 30 ]; do
		_actual="$(bound_port)"
		if [ -n "$_actual" ] && netstat -tln 2>/dev/null | grep -q ":$_actual "; then
			return 0
		fi
		_i=$((_i + 1))
		sleep 1
	done
	return 1
}

log "Starting $SERVICE (user=$PHANTOM_USER)"
start_service

if ! wait_for_listen; then
	if [ "$PHANTOM_USER" != "root" ]; then
		warn "Port never came up as user '$PHANTOM_USER' — retrying as root"
		warn "(verified: this OpenRC build cannot hand CAP_NET_BIND_SERVICE to a"
		warn " non-root user, so the service falls back to root — see README.md)"
		sed -i 's/^PHANTOM_USER=.*/PHANTOM_USER="root"/' "$CONF_D"
		PHANTOM_USER="root"
		# The state dir must be owned by whoever runs the server: server.key is
		# written 0600 and server.toml is rewritten on every start.
		chown -R "$PHANTOM_USER:$PHANTOM_USER" "$STATE_DIR"
		start_service
		if ! wait_for_listen; then
			tail -20 "$LOG_FILE" 2>/dev/null || true
			die "service did not start; see $LOG_FILE"
		fi
	else
		tail -20 "$LOG_FILE" 2>/dev/null || true
		die "service did not start; see $LOG_FILE"
	fi
fi

rc-update add "$SERVICE" default >/dev/null 2>&1 || true
_actual="$(bound_port)"

# ── Report ───────────────────────────────────────────────────────────────────
URI="$(sed -n 's|^#[[:space:]]*\(phantom://.*\)$|\1|p' "$STATE_DIR/server.toml" | head -1)"

echo
log "Status: $(rc-service "$SERVICE" status 2>&1 | head -1)"
log "Listening on port ${_actual:-$PHANTOM_PORT} as user $PHANTOM_USER"
echo
if [ -n "$URI" ]; then
	echo "Client URI (distribute to clients):"
	echo "  $URI"
else
	warn "No phantom:// URI found in $STATE_DIR/server.toml — check $LOG_FILE"
fi
echo
echo "Useful commands:"
echo "  rc-service phantom status"
echo "  tail -f $LOG_FILE"
echo "  rc-service phantom restart"
