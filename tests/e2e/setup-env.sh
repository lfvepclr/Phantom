#!/bin/bash
# E2E 第2轮环境部署: web + TCP(443) + QUIC(8443) + 备用(9443) 四容器
set -eu
cd "$(dirname "$0")/../.."

echo '--- [1] 凭据就位 ---'
cp .e2e-boot/server.key .e2e-data/server.key

echo '--- [2] 编译 minihttpd (musl) ---'
rustc --edition 2021 -O --target aarch64-unknown-linux-musl -C linker=rust-lld \
  -o .e2e-ctx/minihttpd tests/e2e/minihttpd.rs 2>/dev/null || \
  { mkdir -p .e2e-ctx; rustc --edition 2021 -O --target aarch64-unknown-linux-musl -C linker=rust-lld -o .e2e-ctx/minihttpd tests/e2e/minihttpd.rs; }
printf 'FROM scratch\nCOPY minihttpd /minihttpd\nENTRYPOINT ["/minihttpd", "/www"]\n' > .e2e-ctx/Containerfile-web

echo '--- [3] 网络与镜像 ---'
podman network create phantom-net 2>/dev/null || true
podman build -q -f deploy/Containerfile -t localhost/phantom-server target/aarch64-unknown-linux-musl/release | tail -1
podman build -q -f .e2e-ctx/Containerfile-web -t localhost/phantom-web .e2e-ctx | tail -1

echo '--- [4] 启动容器 ---'
podman rm -f phantom-web phantom-server phantom-server-quic phantom-server2 2>/dev/null || true
podman run -d --name phantom-web --network phantom-net -v "$PWD/.e2e-web:/www:Z" localhost/phantom-web > /dev/null
podman run -d --name phantom-server --network phantom-net \
  -p 443:443/tcp --cpus 1 --memory 256m \
  -v "$PWD/.e2e-data:/data:Z" localhost/phantom-server > /dev/null
podman run -d --name phantom-server-quic --network phantom-net \
  -p 8443:443/tcp -p 8443:443/udp --cpus 1 --memory 256m \
  --entrypoint /usr/local/bin/phantom-server \
  -v "$PWD/.e2e-data:/data:Z" localhost/phantom-server /data/server-quic.toml > /dev/null
podman run -d --name phantom-server2 --network phantom-net \
  -p 9443:443/tcp --cpus 1 --memory 256m \
  --entrypoint /usr/local/bin/phantom-server \
  -v "$PWD/.e2e-data:/data:Z" localhost/phantom-server /data/server2.toml > /dev/null

sleep 2
echo '--- [5] 状态 ---'
podman ps --format '{{.Names}}  {{.Status}}  {{.Ports}}'
echo '--- banner check ---'
podman logs phantom-server-quic 2>&1 | grep -o 'proto=[a-z]*' | head -1
echo 'ENV-READY'
