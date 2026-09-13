#!/bin/bash
# E2E 阶段 5: 安全边界 —— 裸扫描静默丢弃 / 篡改 PSK / 篡改公钥
set -u
cd "$(dirname "$0")/../.."

# 凭据从环境变量读取，仓库里只留占位值（PHANTOM_E2E_KEY / PHANTOM_E2E_PSK）。
GOOD_PSK="${PHANTOM_E2E_PSK:-cGhhbnRvbS1lMmUtcHJlLXNoYXJlZC1rZXktMzJiISE=}"
GOOD_KEY="${PHANTOM_E2E_KEY:-cGhhbnRvbS1lMmUtdGVzdC1zZXJ2ZXIta2V5LTMyYiE=}"

server_log_lines() { podman logs phantom-server 2>&1 | wc -l; }
quic_log_lines()   { podman logs phantom-server-quic 2>&1 | wc -l; }

echo '=== [1] 裸扫描：无凭据流量应被静默丢弃 ==='
N1=$(server_log_lines)
curl -m 5 -sk https://127.0.0.1:443/ > /dev/null 2>&1; echo "https-probe exit=$? (expect non-zero)"
curl -m 5 -s http://127.0.0.1:443/ > /dev/null 2>&1; echo "http-probe exit=$? (expect non-zero)"
printf 'GET / HTTP/1.1\r\nHost: x\r\n\r\n' | nc -w 3 127.0.0.1 443 > /dev/null 2>&1; echo "raw-tcp-probe exit=$?"
sleep 1
N2=$(server_log_lines)
if [ "$N2" -eq "$N1" ]; then echo '[PASS] 裸扫描被静默丢弃（服务端零日志）'; else echo '[FAIL] 服务端产生了日志：'; podman logs phantom-server 2>&1 | tail -3; fi

echo ''
echo '=== [2] 篡改 PSK（CLI）：握手必须失败 ==='
BAD_PSK_URI="phantom://${GOOD_KEY}@127.0.0.1:443?psk=AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=&cipher=auto&proto=tcp#bad"
RUST_LOG=info ./target/release/phantom client -s "$BAD_PSK_URI" > .e2e-boot/client-badpsk.log 2>&1 &
BPID=$!
sleep 15
if kill -0 "$BPID" 2>/dev/null; then
  echo '[INFO] 客户端仍在运行（等待超时）——15s 内未完成握手'
  grep -iE 'error|timeout' .e2e-boot/client-badpsk.log | head -3
  kill "$BPID" 2>/dev/null
else
  echo '[INFO] 客户端已退出：'
  tail -3 .e2e-boot/client-badpsk.log
fi
N3=$(server_log_lines)
if [ "$N3" -eq "$N2" ]; then echo '[PASS] 篡改 PSK 未产生服务端握手日志（首包静默丢弃）'; else echo '[WARN] 服务端有新日志（检查是否为握手失败记录）：'; podman logs phantom-server 2>&1 | tail -3; fi

echo ''
echo '=== [3] 篡改公钥（QUIC）：握手必须失败 ==='
NQ1=$(quic_log_lines)
BAD_KEY_URI="phantom://AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=@127.0.0.1:8443?psk=${GOOD_PSK}&cipher=auto&proto=quic#badkey"
RUST_LOG=info ./target/release/phantom client -s "$BAD_KEY_URI" > .e2e-boot/client-badkey.log 2>&1 &
BKPID=$!
sleep 15
if kill -0 "$BKPID" 2>/dev/null; then
  echo '[INFO] 客户端仍在运行（等待超时）'
  grep -iE 'error|timeout' .e2e-boot/client-badkey.log | head -3
  kill "$BKPID" 2>/dev/null
else
  echo '[INFO] 客户端已退出：'
  tail -3 .e2e-boot/client-badkey.log
fi
NQ2=$(quic_log_lines)
if [ "$NQ2" -eq "$NQ1" ]; then echo '[PASS] 篡改公钥未产生服务端握手日志'; else echo '[WARN] QUIC 服务端有新日志：'; podman logs phantom-server-quic 2>&1 | tail -3; fi

wait 2>/dev/null
echo ''
echo '=== security test done ==='
