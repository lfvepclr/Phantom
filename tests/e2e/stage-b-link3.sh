#!/bin/bash
# 阶段 B 链路级 v3: 用 TOML serde 枚举名（aes256-gcm/aes128-gcm/ascon128/cha-cha20-poly1305）
set -u
cd /Users/<user>/workspace/qoder/phantom

KEY='cGhhbnRvbS1lMmUtdGVzdC1zZXJ2ZXIta2V5LTMyYiE=='
PSK='cGhhbnRvbS1lMmUtcHJlLXNoYXJlZC1rZXktMzJiISE=='

# TOML serde 名 → 展示名
run_matrix() {
  for pair in "aes256-gcm:AES-256-GCM" "aes128-gcm:AES-128-GCM" "cha-cha20-poly1305:ChaCha20" "ascon128:ASCON-128"; do
    toml_name="${pair%%:*}"
    label="${pair##*:}"
    echo "===== server cipher = ${label} ====="
    sed -i '' "s/^cipher = .*/cipher = \"${toml_name}\"/" .e2e-data/server.toml
    podman restart phantom-server > /dev/null
    sleep 3
    if [ "$(podman inspect -f '{{.State.Running}}' phantom-server)" != "true" ]; then
      echo "  [FAIL] server not running:"
      podman logs phantom-server 2>&1 | tail -3
      continue
    fi

    for round in 1 2; do
      pkill -f 'phantom client' 2>/dev/null; sleep 0.5
      RUST_LOG=warn ./target/release/phantom client -s \
        "phantom://${KEY}@127.0.0.1:443?psk=${PSK}&cipher=auto&proto=tcp#m" > /dev/null 2>&1 &
      cpid=$!
      sleep 3
      if kill -0 $cpid 2>/dev/null; then
        curl -s -m 120 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
          -w "  LINK TCP-${label} r${round} speed=%{speed_download} B/s time=%{time_total}s\n" \
          http://phantom-web:8080/10mb.bin
      else
        echo "  LINK TCP-${label} r${round} [FAIL] client died"
      fi
      kill $cpid 2>/dev/null; wait $cpid 2>/dev/null
    done
  done
}

run_matrix

# 恢复 auto
sed -i '' 's/^cipher = .*/cipher = "auto"/' .e2e-data/server.toml
podman restart phantom-server > /dev/null
sleep 2

echo '===== 服务端协商确认（本轮） ====='
podman logs --since 6m phantom-server 2>&1 | grep 'Client connected' | sort | uniq -c
