# Phantom 端到端测试计划

> 目标：以 podman 部署 Linux 服务端，配合「隧道专属静态网页」完成服务端 + 客户端的端到端功能验证，
> 覆盖 UI 点击、日志观察（握手 / 加密 / 转发）、传输速度与流量路径证明。
> 编写日期：2026-08-23。环境：macOS（Apple Silicon）+ podman machine（aarch64 Linux）+ DevEco 模拟器。

---

## 1. 测试拓扑

```
macOS 宿主机
├── podman machine（aarch64 Linux VM）
│   ├── phantom-server 容器   [phantom-net]  发布 443/tcp + 443/udp → 宿主机
│   │     musl 静态二进制，I/O 型低配：--cpus 1 --memory 256m
│   └── phantom-web 容器      [phantom-net]  busybox httpd :8080，不发布端口
│         隧道专属网页 + 测速文件（宿主机/局域网均不可直连）
│
├── CLI 客户端（phantom client -s <URI>）
│     SOCKS5/HTTP 双协议入站 127.0.0.1:1080，Prometheus 127.0.0.1:9150
│
└── 鸿蒙模拟器（Pura 90, HarmonyOS 6.1.1 API 24）
      UI 点击：填 URI → Start Tunnel → Connection Logs 观察握手
```

核心验证思想：**phantom-web 只存在于 podman 内部网络**。任何能打开它的 HTTP 请求，
必然经过了 Phantom 隧道转发（服务端日志会同步出现 `SYN → phantom-web:8080` 与字节统计），
因此「是否通过代理」不依赖外网服务，完全本地、可重复、可断言。

资源规格说明（按项目定位：I/O 型工作负载，消耗网络而非 CPU）：服务端容器固定
`--cpus 1 --memory 256m`。转发路径瓶颈在 AEAD 加密与网络，1 vCPU 足以跑通全功能；
性能阶段（§8）在该限制下测得的吞吐即为「单核加密上限」数据，同时提供 `--cpus 2` 对照。

---

## 2. 阶段 0：podman 服务端部署

### 2.1 构建静态二进制

```bash
cd /Users/<user>/workspace/qoder/phantom
cargo xtask build server-arm64
# 产物：target/aarch64-unknown-linux-musl/release/phantom-server
file target/aarch64-unknown-linux-musl/release/phantom-server
# 断言：ELF 64-bit LSB ... ARM aarch64 ... statically linked
```

说明：musl 静态链接 → 镜像可用 `FROM scratch`，零系统依赖；podman machine 为
aarch64 Linux，与该产物架构一致。musl 下 io-uring 特性为 no-op（epoll 运行时），
容器默认 seccomp 亦不支持 io_uring，两者一致，无需开启。

### 2.2 Containerfile

新建 `deploy/Containerfile`：

```dockerfile
FROM scratch
COPY target/aarch64-unknown-linux-musl/release/phantom-server /usr/local/bin/phantom-server
WORKDIR /data
ENTRYPOINT ["/usr/local/bin/phantom-server", "/data/server.toml"]
```

scratch 无 shell；所有状态（server.key / server.toml / clients）经 `/data` 卷持久化。

### 2.3 容器编配（含资源限制）

```bash
cd /Users/<user>/workspace/qoder/phantom

# 内部网络（服务端与 web 容器互通；web 不发布端口）
podman network create phantom-net

# 服务端容器：I/O 型低配 —— 1 vCPU / 256MB，网络优先
podman build -f deploy/Containerfile -t localhost/phantom-server .
podman run -d --name phantom-server \
  --network phantom-net \
  -p 443:443/tcp -p 443:443/udp \
  --cpus 1 --memory 256m \
  -v "$PWD/.e2e-data:/data:Z" \
  localhost/phantom-server
```

`/data/server.toml` 内容（挂载卷中手工创建，见 §4.2 生成 server.key 后）：

```toml
bind = "0.0.0.0:443"
private_key = "/data/server.key"   # 三行文件：公钥 / 私钥 / PSK，须整体复用
clients = ""                       # 空 = 开放模式（白名单用例见 §9）
cipher = "auto"

[quic]
max_streams = 100
keep_alive_interval = 45
congestion = "bbr"

[performance]
io_uring = false
workers = 2
```

### 2.4 健康检查

```bash
podman ps --filter name=phantom-server        # STATUS 应为 Up
podman logs phantom-server | head -20         # 应有 bind/白名单相关日志，无 panic
podman stats --no-stream phantom-server       # 记录空闲 CPU/内存基线
```

---

## 3. 阶段 1：隧道专属静态网页

### 3.1 内容准备（宿主机）

```bash
mkdir -p .e2e-web
cat > .e2e-web/index.html <<'EOF'
<!doctype html><html><body>
<h1>PHANTOM-E2E-OK</h1>
<p>This page is only reachable through the Phantom tunnel.</p>
</body></html>
EOF
dd if=/dev/urandom of=.e2e-web/10mb.bin bs=1m count=10   # 测速文件
dd if=/dev/urandom of=.e2e-web/100kb.bin bs=1k count=100 # 小文件（延迟测试）
```

### 3.2 Web 容器（不发布任何端口）

```bash
podman run -d --name phantom-web \
  --network phantom-net \
  -v "$PWD/.e2e-web:/www:Z" \
  docker.io/library/busybox:latest \
  httpd -f -p 8080 -h /www
```

### 3.3 隔离性预断言（负向用例）

```bash
# 宿主机直连容器名 / 容器 IP 均不可达（无端口发布 + 容器网络隔离）
curl -m 3 -s http://phantom-web:8080/ ; echo "exit=$?"    # 期望 exit=6（DNS 解析失败）
WEB_IP=$(podman inspect -f '{{.NetworkSettings.Networks.phantom-net.IPAddress}}' phantom-web)
curl -m 3 -s "http://${WEB_IP}:8080/" ; echo "exit=$?"    # 期望 exit=7/28（连接拒绝/超时）
```

预期结论：**任何宿主机进程都无法绕过代理访问该网页**——这是后续正向用例的对照组。

---

## 4. 阶段 2：服务端自举与 URI 获取

### 4.1 宿主机自举（同时验证自举功能本身）

```bash
cargo xtask build cli          # 产物 target/release/phantom
mkdir -p .e2e-boot && cd .e2e-boot
../target/release/phantom server --port 443 --public-host 127.0.0.1
```

自举标准输出断言（对照记忆《Phantom服务端自举标准输出规范》）：
- 打印 `server.key` 路径、`server.toml` 路径、白名单状态（OPEN 模式）、bind 地址；
- 打印完整 `phantom://<base64公钥>@127.0.0.1:443?cipher=auto&proto=quic#...` URI（含 psk 参数）；
- 记录该 URI（下称 `$URI`），Ctrl-C 停止进程。

### 4.2 凭据复用进容器

```bash
cd .. && cp .e2e-boot/server.key .e2e-data/server.key
# 按 §2.3 创建 .e2e-data/server.toml 后：
podman restart phantom-server
podman logs phantom-server | tail -5      # 应看到 "Loaded N allowed client keys" 或开放模式日志
```

URI 中的 host 部分按客户端所在网络替换：
- CLI（宿主机本机）：`127.0.0.1`
- 鸿蒙模拟器：宿主机局域网 IP（`ipconfig getifaddr en0`，下称 `$HOST_IP`）

仅替换 host，`psk=`/`cipher=`/`proto=` 参数保持不变。

---

## 5. 阶段 3：CLI 端到端数据面（核心路径）

### 5.1 启动客户端（TCP 与 QUIC 各一轮）

```bash
# 第 1 轮：QUIC（URI 原样）
RUST_LOG=info ./target/release/phantom client -s "$URI" &
# 第 2 轮：TCP（URI 的 proto 参数改为 proto=tcp）
RUST_LOG=info ./target/release/phantom client -s "${URI/proto=quic/proto=tcp}" &
```

观察客户端日志：应出现服务端连接、握手成功、SOCKS5 监听 `127.0.0.1:1080`、
metrics 监听 `127.0.0.1:9150`。

### 5.2 流量路径验证（是否通过代理）

```bash
# 经 SOCKS5（域名由代理侧解析）
curl -s --socks5-hostname 127.0.0.1:1080 http://phantom-web:8080/
# 断言：输出含 PHANTOM-E2E-OK

# 经 HTTP 代理（同端口协议嗅探）
curl -s -x http://127.0.0.1:1080 http://phantom-web:8080/
# 断言：输出含 PHANTOM-E2E-OK
```

服务端日志同步断言（另一终端 `podman logs -f phantom-server`）：

| 日志行 | 含义 | 出现时机 |
|---|---|---|
| `Client connected (cipher=Aes256Gcm)` 或 `QUIC client connected` | Noise IKpsk2 握手成功 + 协商加密套件 | 客户端启动 / 首次请求 |
| `SYN → phantom-web:8080 (stream=N)` | 经隧道发起了对内网网页的转发 | 每次 curl |
| `Relay done ↓ phantom-web:8080 (N bytes down)` | 下行字节统计 | 请求结束 |
| `Relay done ↑ phantom-web:8080 (N bytes up)` | 上行字节统计 | 请求结束 |

客户端 metrics 断言：

```bash
curl -s http://127.0.0.1:9150/metrics | grep -E 'phantom.*(bytes|connections)'
# 断言：tcp_bytes_down 增长，增量 ≈ index.html 大小 + 帧协议开销
```

### 5.3 加密套件矩阵（可选加强）

URI 追加/替换 `cipher=` 参数为 `aes-128-gcm` / `chacha20-poly1305` / `ascon-128`，
重复 §5.2，断言服务端日志 `Client connected (cipher=...)` 与所选套件一致。

---

## 6. 阶段 4：UI 端到端（鸿蒙模拟器点击验证）

### 6.1 前置

```bash
# 启动模拟器（已配置的实例）
/Applications/DevEco-Studio.app/Contents/tools/emulator/Emulator -start "Pura 90"
export PATH="/Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/toolchains:$PATH"
hdc list targets                                  # 断言：127.0.0.1:5555
hdc install client/harmony/entry-default-signed.hap
hdc shell aa start -a EntryAbility -b co.phantom.harmony
```

### 6.2 UI 操作流（Midscene 视觉自动化或手动）

使用已安装的 `harmonyos-device-automation` skill（Midscene，纯截图驱动）执行：

1. **截图基线**：确认 VPN Tab、URI 输入框、Global/Auto/Direct、Start Tunnel、Connection Logs 可见；
2. **填入 URI**：将 `$URI` 的 host 替换为 `$HOST_IP` 后输入服务地址框；
3. **点击 Start Tunnel**；
4. **轮询截图断言**（10s 内）：
   - 状态文本由 `Idle`（红）变为运行态（`isRunning`，status=2）；
   - Connection Logs 区域出现连接/握手相关行；
5. **点击 Stop Tunnel**：状态回到 Idle。

### 6.3 日志双通道断言

```bash
# 通道 1：应用 NAPI 日志（UI Connection Logs 的数据源）
hdc shell "hilog -x 2>/dev/null" | grep -i phantom | tail -30
# 通道 2：服务端容器日志——UI 触发的握手必须在此留痕
podman logs phantom-server | tail -10
# 断言：出现 Client connected / QUIC client connected（证明 UI 路径真实完成了 IKpsk2 握手）
```

### 6.4 可选探索：模拟器浏览器全链路

模拟器内置浏览器直访 `http://phantom-web:8080/`（不可达）→ VPN 模式下授权 VpnExtension
（UI 自动化处理系统授权弹窗）→ 再次访问（应返回 PHANTOM-E2E-OK）。若模拟器 VPN 权限
受限，本项标记 SKIP，不影响通过标准。

---

## 7. 阶段 5：安全边界

| 用例 | 操作 | 预期 |
|---|---|---|
| 裸扫描 | `curl -m 5 -sk https://127.0.0.1:443/` 或裸 TCP 连 443 | 服务端无握手日志（无 PSK 无法构造首包，静默丢弃） |
| 篡改 PSK（CLI） | URI 中 psk 参数改 1 字节后启动客户端 | 客户端报握手失败；服务端无 `Client connected` |
| 篡改 PSK（UI） | 模拟器填入篡改后的 URI → Start Tunnel | UI 状态转 Error，Connection Logs / `phantomHarmonyGetLastError()` 显示握手失败 |
| 篡改公钥 | URI 中 base64 公钥改 1 字符 | 同上：握手失败 |

自动化对照：`cargo test -p phantom-e2e --test config_effect`（已含 PSK 不匹配必须失败、
缺失 PSK 必须报错用例）。

---

## 8. 阶段 6：传输性能观测

### 8.1 吞吐（经隧道下载测速文件）

```bash
# 经 SOCKS5 隧道
curl -s --socks5-hostname 127.0.0.1:1080 -o /dev/null \
  -w 'speed=%{speed_download} B/s  size=%{size_download}  time=%{time_total}s\n' \
  http://phantom-web:8080/10mb.bin

# 上限基准：phantom-net 内直连（不经隧道、不加密）
podman run --rm --network phantom-net -v "$PWD/.e2e-web:/www:Z" \
  docker.io/library/busybox:latest \
  sh -c 'time wget -q -O /dev/null http://phantom-web:8080/10mb.bin'
```

记录两轮（QUIC / TCP）、各 cipher 的隧道速度与直连基准比值。1 vCPU 限制下的吞吐即
「单核 AEAD 加密上限」；如需分辨 CPU 与 IO 瓶颈，临时将服务端容器调整为 `--cpus 2` 复测。

### 8.2 延迟（小对象请求）

```bash
for i in 1 2 3 4 5; do
  curl -s --socks5-hostname 127.0.0.1:1080 -o /dev/null \
    -w '%{time_total}\n' http://phantom-web:8080/100kb.bin
done
```

### 8.3 三方字节对账

一次 10MB 下载后核对：curl `size_download` ≈ 服务端 `Relay done ↓` 字节 ≈ 客户端
metrics `phantom_tcp_bytes_down` 增量（差异为帧协议/握手开销，应在小几个百分点内）。

### 8.4 资源占用

```bash
podman stats --no-stream phantom-server   # 压测时观察：CPU 接近 1 核上限属预期（crypto 密集）
```

---

## 9. 阶段 7：既有自动化回归（每轮 UI 手测后必跑）

```bash
cargo test --lib
cargo test -p phantom-e2e            # 含 full_link_tcp / full_link_udp / quic_mux /
                                     # http_proxy / socks5_udp / throughput / weak_network /
                                     # config_effect / cipher_matrix / stats_metrics / cli_system
cargo bench -p phantom-bench         # aead_throughput / handshake / frame_codec / pipeline 微基准
```

重点断言：`quic_mux`（N 条 SOCKS5 连接仅 1 次握手、stream 不串流）、`config_effect`
（PSK 契约）、`throughput`（回归阈值）。

---

## 10. 通过标准

| # | 验证项 | 通过条件 |
|---|---|---|
| 1 | 部署 | scratch 容器启动成功，无 panic；空闲内存 < 50MB |
| 2 | 隔离 | 宿主机直连 phantom-web 全部失败（exit≠0） |
| 3 | 自举 | stdout 含完整 URI（公钥 + psk），server.key 三行完整 |
| 4 | SOCKS5 转发 | 经 1080 端口 curl 返回 PHANTOM-E2E-OK（TCP 与 QUIC 双协议） |
| 5 | HTTP 代理 | 同端口嗅探分流正常，CONNECT 与明文 GET 均通 |
| 6 | 握手/加密日志 | 服务端出现 `Client connected (cipher=...)`，cipher 与 URI 一致 |
| 7 | 转发日志 | `SYN →` 与 `Relay done ↑/↓` 字节与实际流量一致 |
| 8 | metrics | 9150 端点 bytes/connections 指标与日志对账一致 |
| 9 | UI 握手 | 模拟器点击 Start Tunnel 后状态转运行、Connection Logs 出现握手行、服务端留痕 |
| 10 | UI 安全 | 篡改 PSK 后 Start Tunnel 必须失败并显示错误 |
| 11 | 性能 | 10MB 下载完成并记录速度；三方字节对账误差 < 5% |
| 12 | 回归 | `cargo test -p phantom-e2e` 与 `--lib` 全绿 |

---

## 附录：清理

```bash
podman rm -f phantom-server phantom-web
podman network rm phantom-net
podman rmi localhost/phantom-server docker.io/library/busybox:latest
rm -rf .e2e-web .e2e-data .e2e-boot
```
