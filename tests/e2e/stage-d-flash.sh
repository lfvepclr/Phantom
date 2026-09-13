#!/bin/bash
# 阶段 D: 闪断测试 —— 服务端重启 / 进程冻结(网络黑洞近似) / 双服务器 failover
set -u
cd "$(dirname "$0")/../.."

KEY="${PHANTOM_E2E_KEY:-cGhhbnRvbS1lMmUtdGVzdC1zZXJ2ZXIta2V5LTMyYiE=}"
PSK="${PHANTOM_E2E_PSK:-cGhhbnRvbS1lMmUtcHJlLXNoYXJlZC1rZXktMzJiISE=}"

# 循环探测: 每 0.6s 一次 100KB 下载，记录成功/失败时间线
probe_loop() {
  local duration="$1" tag="$2"
  local end=$((SECONDS + duration))
  local ok=0 fail=0
  while [ $SECONDS -lt $end ]; do
    if curl -s -m 3 --socks5-hostname 127.0.0.1:1080 -o /dev/null \
         -w '%{http_code}' http://phantom-web:8080/100kb.bin 2>/dev/null | grep -q 200; then
      echo "$(date +%H:%M:%S.%N | cut -c1-12) OK" >> /tmp/probe.log
      ok=$((ok+1))
    else
      echo "$(date +%H:%M:%S.%N | cut -c1-12) FAIL" >> /tmp/probe.log
      fail=$((fail+1))
    fi
  done
  echo "PROBE [${tag}] ok=${ok} fail=${fail}"
}

start_client() {
  local uri="$1"
  pkill -f 'phantom client' 2>/dev/null; sleep 0.5
  RUST_LOG=info ./target/release/phantom client -s "$uri" > /tmp/client-d.log 2>&1 &
  sleep 3
}

echo '===== D1: 服务端重启闪断（TCP） ====='
rm -f /tmp/probe.log /tmp/client-d.log
start_client "phantom://${KEY}@127.0.0.1:443?psk=${PSK}&cipher=auto&proto=tcp#d"
( sleep 5; podman restart phantom-server > /dev/null 2>&1; echo "RESTARTED at $(date +%H:%M:%S)" >> /tmp/probe.log ) &
probe_loop 18 'restart'
wait
echo '--- 失败窗口 ---'
grep -B 2 -A 2 'FAIL\|RESTARTED' /tmp/probe.log | head -12
echo '--- 客户端日志（重连行为） ---'
grep -iE 'connect|error|reconnect|retry|fail' /tmp/client-d.log | tail -6

echo ''
echo '===== D2: 网络黑洞（pause 3s，TCP） ====='
rm -f /tmp/probe.log /tmp/client-d.log
start_client "phantom://${KEY}@127.0.0.1:443?psk=${PSK}&cipher=auto&proto=tcp#d"
( sleep 5; podman pause phantom-server > /dev/null 2>&1; echo "PAUSED at $(date +%H:%M:%S)" >> /tmp/probe.log; sleep 3; podman unpause phantom-server > /dev/null 2>&1; echo "UNPAUSED at $(date +%H:%M:%S)" >> /tmp/probe.log ) &
probe_loop 15 'blackhole'
wait
grep -B 1 -A 3 'PAUSED' /tmp/probe.log | head -8
echo '--- 客户端日志 ---'
grep -iE 'connect|error|fail' /tmp/client-d.log | tail -4

echo ''
echo '===== D3: 双服务器 failover（443 主 → 9443 备） ====='
cat > /tmp/client-failover.toml <<EOF
[[servers]]
name = "primary"
address = "127.0.0.1:443"
public_key = "${KEY}"
psk = "${PSK}"
protocol = "tcp"

[[servers]]
name = "backup"
address = "127.0.0.1:9443"
public_key = "${KEY}"
psk = "${PSK}"
protocol = "tcp"
EOF
rm -f /tmp/probe.log /tmp/client-d.log
pkill -f 'phantom client' 2>/dev/null; sleep 0.5
RUST_LOG=info ./target/release/phantom client -c /tmp/client-failover.toml > /tmp/client-d.log 2>&1 &
sleep 3
( sleep 5; podman stop phantom-server > /dev/null 2>&1; echo "PRIMARY-STOPPED at $(date +%H:%M:%S)" >> /tmp/probe.log; sleep 12; podman start phantom-server > /dev/null 2>&1 ) &
probe_loop 25 'failover'
wait
echo '--- 切换时间线 ---'
grep -B 3 -A 3 'PRIMARY-STOPPED' /tmp/probe.log | head -10
echo '--- 客户端 failover 日志 ---'
grep -iE 'failover|switch|migrat|primary|backup|connect|error' /tmp/client-d.log | head -12

pkill -f 'phantom client' 2>/dev/null
echo '=== stage D done ==='
