#!/usr/bin/env bash
# Offline end-to-end verification of a packaged Phantom server bundle.
#
# Runs the *packaged* static binary inside the production Alpine release
# (alpine:3.18) together with a controllable origin (tests/e2e/minihttpd.rs),
# then drives real traffic through the tunnel from this machine using the
# local CLI client and byte-compares the result.
#
# Nothing external is contacted, so it works on networks where google.com is
# unreachable (the deployment host is what provides the real egress later).
#
# Usage: verify-server.sh <pkg-dir> <arch> <engine>
#   e.g. verify-server.sh dist/phantom-server-0.1.0-linux-amd64 amd64 podman

set -euo pipefail

PKG_DIR="${1:-}"
ARCH="${2:-amd64}"
ENGINE="${3:-podman}"

[[ -n "$PKG_DIR" && -d "$PKG_DIR" ]] || { echo "ERROR: package dir not found: $PKG_DIR" >&2; exit 1; }
command -v "$ENGINE" >/dev/null || { echo "ERROR: container engine '$ENGINE' not found" >&2; exit 1; }

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
WORK="$REPO_ROOT/dist/verify"
NET="phantom-verify"
SERVER_CT="phantom-verify-server"
ORIGIN_CT="phantom-verify-origin"
SERVER_IMG="phantom-verify-server:local"
ORIGIN_IMG="phantom-verify-origin:local"
HOST_PORT="${PHANTOM_VERIFY_PORT:-18443}"
SOCKS_PORT="${PHANTOM_VERIFY_SOCKS:-1080}"
CLIENT="$REPO_ROOT/target/release/phantom"

cleanup() {
    set +e
    "$ENGINE" rm -f "$SERVER_CT" "$ORIGIN_CT" >/dev/null 2>&1
    [[ -n "${CLIENT_PID:-}" ]] && kill "$CLIENT_PID" >/dev/null 2>&1
    "$ENGINE" network rm "$NET" >/dev/null 2>&1
}
trap cleanup EXIT

echo "==> Preparing verification images (target: linux/$ARCH)"
mkdir -p "$WORK"

# Test origin: the same static-file server the e2e suite uses, served from an
# Alpine 3.18 userland so the data path is production-shaped.
"$ENGINE" build -q -f deploy/Containerfile --target test-origin \
    --build-arg "TARGETARCH=$ARCH" -o "type=local,dest=$WORK/origin" "$REPO_ROOT"

cat > "$WORK/origin.Containerfile" <<'EOF'
FROM alpine:3.18
COPY minihttpd /usr/local/bin/minihttpd
RUN set -eux; \
    mkdir -p /www; \
    head -c 10000000 /dev/urandom > /www/10mb.bin; \
    head -c 102400  /dev/urandom > /www/100kb.bin; \
    head -c 1024    /dev/urandom > /www/1kb.bin
CMD ["/usr/local/bin/minihttpd", "/www"]
EOF

cat > "$WORK/server.Containerfile" <<'EOF'
FROM alpine:3.18
COPY phantom /usr/local/bin/phantom
ENTRYPOINT ["/usr/local/bin/phantom"]
EOF

"$ENGINE" build -q -f "$WORK/origin.Containerfile" -t "$ORIGIN_IMG" "$WORK/origin"
"$ENGINE" build -q -f "$WORK/server.Containerfile" -t "$SERVER_IMG" "$PKG_DIR"

echo "==> Starting containers (engine=$ENGINE, platform=linux/$ARCH)"
"$ENGINE" network create "$NET" >/dev/null 2>&1 || true
"$ENGINE" run -d --name "$ORIGIN_CT" --network "$NET" "$ORIGIN_IMG" >/dev/null
"$ENGINE" volume create phantom-verify-data >/dev/null 2>&1 || true
"$ENGINE" run -d --name "$SERVER_CT" --network "$NET" \
    -p "127.0.0.1:$HOST_PORT:443" -v phantom-verify-data:/var/lib/phantom \
    --platform "linux/$ARCH" "$SERVER_IMG" server --port 443 --public-host 127.0.0.1 >/dev/null

# Wait for the server to bootstrap (writes server.toml with the URI).
URI=""
for _ in $(seq 1 30); do
    URI="$("$ENGINE" exec "$SERVER_CT" sh -c \
        "sed -n 's|^#[[:space:]]*\(phantom://.*\)\$|\1|p' /var/lib/phantom/server.toml 2>/dev/null | head -1" \
        2>/dev/null || true)"
    [[ -n "$URI" ]] && break
    sleep 1
done
[[ -n "$URI" ]] || { echo "ERROR: server never wrote a URI"; "$ENGINE" logs "$SERVER_CT" | tail -20; exit 1; }

# The container listens on 443 internally; from the host it is $HOST_PORT.
URI="${URI/@127.0.0.1:443/@127.0.0.1:$HOST_PORT}"
echo "    URI: $URI"

ORIGIN_IP="$("$ENGINE" inspect -f '{{(index .NetworkSettings.Networks "'"$NET"'").IPAddress}}' "$ORIGIN_CT" 2>/dev/null || true)"
[[ -n "$ORIGIN_IP" ]] || { echo "ERROR: could not determine origin container IP"; exit 1; }
echo "    origin: $ORIGIN_IP:8080"

echo "==> Driving traffic through the tunnel"
RUST_LOG=warn "$CLIENT" client --server "$URI" >"$WORK/client.log" 2>&1 &
CLIENT_PID=$!
for _ in $(seq 1 30); do
    nc -z 127.0.0.1 "$SOCKS_PORT" 2>/dev/null && break
    sleep 0.5
done
nc -z 127.0.0.1 "$SOCKS_PORT" || { echo "ERROR: client SOCKS5 never came up"; tail -20 "$WORK/client.log"; exit 1; }

PROXY="socks5h://127.0.0.1:$SOCKS_PORT"
BODY="$WORK/downloaded.bin"
SPEED="$(curl -s --max-time 120 -x "$PROXY" -o "$BODY" \
    -w '%{speed_download} %{size_download} %{time_total}' \
    "http://$ORIGIN_IP:8080/10mb.bin")"

LOCAL_SUM="$("$ENGINE" exec "$ORIGIN_CT" sha256sum /www/10mb.bin | awk '{print $1}')"
FETCHED_SUM="$(shasum -a 256 "$BODY" 2>/dev/null | awk '{print $1}' || sha256sum "$BODY" | awk '{print $1}')"

echo "    10 MiB through tunnel: $(awk '{printf "%.2f MB/s (%.1f Mbps) in %ss", $1/1048576, $1*8/1000000, $3}' <<<"$SPEED")"
if [[ "$LOCAL_SUM" == "$FETCHED_SUM" ]]; then
    echo "    byte-for-byte check:   OK ($FETCHED_SUM)"
else
    echo "    byte-for-byte check:   MISMATCH ($LOCAL_SUM != $FETCHED_SUM)" >&2
    exit 1
fi

echo "==> Latency samples (/100kb.bin)"
for i in 1 2 3; do
    curl -s --max-time 30 -x "$PROXY" -o /dev/null \
        -w "    sample $i: %{time_total}s\n" "http://$ORIGIN_IP:8080/100kb.bin"
done

echo "==> Real-internet relay check (domestic target, reachable from here)"
CODE="$(curl -s --max-time 30 -x "$PROXY" -o /dev/null -w '%{http_code}' http://www.baidu.com || echo 000)"
echo "    www.baidu.com -> HTTP $CODE"
[[ "$CODE" != "000" ]] || { echo "ERROR: domestic relay check failed"; exit 1; }

echo
echo "OK: packaged binary runs on alpine:3.18 and relays real traffic end to end."
