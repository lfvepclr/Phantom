# Phantom 架构文档

## 1. 项目概述

Phantom 是一个高性能加密代理隧道，基于 Rust 构建，采用 `core/` + `server/` + `client/` 三级 workspace 结构。核心设计目标：
- **数据面 100% Rust**：macOS/Android 原生客户端通过 FFI 一次性传递 TUN fd，之后零跨语言开销
- **控制面-数据面契约**：配置声明的每一项必须在数据面有对应实现
- **零拷贝传输**：基于 `BytesMut` 复用的帧协议，消除每帧堆分配
- **QUIC 多路复用**：Noise IK 握手一次，后续 stream 通过 HKDF 派生子密钥
- **智能分流**：DNS 劫持 + 规则引擎 + 热重载，配置即行为

---

## 2. 系统架构

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                                客户端 (Client)                              │
│                                                                             │
│  ┌─────────────┐   SOCKS5   ┌──────────────────┐   TCP/QUIC   ┌──────────┐ │
│  │  应用程序    │ ──────────▶│  Phantom Client  │ ────────────▶│  Server  │ │
│  │ (浏览器等)   │            │  (SOCKS5 + TUN)  │  Noise+AEAD  │          │ │
│  └─────────────┘            └──────────────────┘              └──────────┘ │
│         │                            │                                     │
│         │       TUN (透明代理)         │                                     │
│         └────────────────────────────┘                                     │
│                                        │                                   │
│  ┌─────────────────────────────────────┴──────────────────────────────┐    │
│  │  TUN 数据面                                                        │    │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌───────────────────┐  │    │
│  │  │ HotReload│  │RuleEngine│  │ DnsProxy │  │  SystemProxy      │  │    │
│  │  │ (Arc<Mux>)│  │ (rules)  │  │ (UDP:53) │  │  (networksetup)  │  │    │
│  │  └──────────┘  └──────────┘  └──────────┘  └───────────────────┘  │    │
│  │  ┌──────────┐  ┌──────────┐  ┌──────────────────────────────┐    │    │
│  │  │TrafficSt │  │UdpProxy  │  │  Metrics HTTP :9150         │    │    │
│  │  │  ats     │  │ FlowTable│  │  (Prometheus /metrics)      │    │    │
│  │  └──────────┘  └──────────┘  └──────────────────────────────┘    │    │
│  └────────────────────────────────────────────────────────────────────┘    │
│                                                                             │
│  平台层:                                                                    │
│    macOS: utun7 + SwiftUI 菜单栏 (FFI: phantom_macos_start/stop)           │
│    Android: VpnService fd + Jetpack Compose (JNI: phantom_android_start)   │
│    CLI: phantom-cli (tokio main, SOCKS5 only)                              │
└─────────────────────────────────────────────────────────────────────────────┘

                                    ↓ TCP/QUIC

┌─────────────────────────────────────────────────────────────────────────────┐
│                                服务端 (Server)                              │
│  ┌──────────────────┐     TCP relay / UDP relay      ┌──────────┐         │
│  │  Phantom Server  │ ──────────────────────────────▶│ 目标站点  │         │
│  │  (Noise responder)│                               │          │         │
│  └──────────────────┘                               └──────────┘         │
│         │                                                                   │
│    Linux io_uring (optional)                                               │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 3. Crate 职责矩阵

| Crate | 路径 | 职责 | 依赖 |
|-------|------|------|------|
| `phantom-core` | `core/` | 共享类型、配置、密码套件、帧协议、传输抽象、URI 解析、错误、常量、BufferPool | `serde`, `toml`, `snow`, `quinn` |
| `phantom-client` | `client/` | SOCKS5 代理、TUN 透明代理、规则引擎、DNS 劫持、UDP relay、流量统计、热重载 | `phantom-*`, `tun`, `etherparse`, `ipnet`, `regex` |
| `phantom-cli` | `client/cli` | CLI 入口 (`phantom client` / `server`)，`server` 支持 auto（自举）/ interactive（向导）/ load（`-c` TOML）三种模式，支持 `--server` URI | `phantom-client`, `phantom-server`, `phantom-core` |
| `phantom-server` | `server/` | 服务端连接处理、TCP relay、UDP relay、io_uring、`bootstrap` 模块（自举 / 交互 / URI 反向解析） | `phantom-*`, `quinn`, `tokio-uring`(opt) |
| `phantom-e2e` | `tests/` | E2E 测试、HTTP/TCP/UDP echo、Mock 百度、网络模拟 | `phantom-*`, `axum` |
| `phantom-bench` | `tests/bench/` | 性能基准测试 | `phantom-*`, `divan` |

---

## 4. 数据流详细图

### 4.1 SOCKS5 路径（CLI / 浏览器模式）

```
应用 → SOCKS5 CONNECT → Phantom Client SOCKS5 listener
                              │
                              ▼
                    ┌──────────────────┐
                    │ NoiseInitiator   │ ──► TCP/QUIC connect
                    │ IK handshake     │ ◄── Cipher negotiation
                    │ split_after_     │
                    │ handshake()      │
                    └──────────────────┘
                              │
                    ┌─────────┴──────────┐
                    │ FrameWriter::syn() │ ──► encrypted SYN frame
                    │ FrameReader read   │ ◄── ACK / RST
                    └────────────────────┘
                              │
                    ┌─────────┴──────────┐
                    │ relay_socks5_tunnel│ 双向 relay
                    └────────────────────┘
```

### 4.2 TUN 路径（macOS / Android Native 模式）

```
系统流量 → TUN device (utun7 / VpnService fd)
                │
                ▼
        ┌───────────────┐
        │ TunProxy::run │
        └───────────────┘
                │
    ┌───────────┼───────────┐
    ▼           ▼           ▼
  TCP SYN    UDP:53      其他 UDP
    │           │           │
    ▼           ▼           ▼
 HotReload   DnsProxy    HotReload
 (proxy_mode) (upstream   (proxy_mode)
    │         DNS)         │
    ▼         │            ▼
 RuleEngine  │         RuleEngine
 (Smart)     │         (Smart → Direct/Proxy/Reject)
    │         │            │
    ▼         ▼            ▼
Direct → direct TCP    Direct → local UdpSocket
Proxy  → SOCKS5 → Noise tunnel → Server
Reject → RST packet   Proxy  → Noise tunnel (UDP SYN frame) → Server
```

### 4.3 UDP Relay 帧协议

```
客户端                                     服务端
  │                                          │
  │──── SYN|UDP|DATA ─────────────────────▶  │  payload = [TargetAddr][datagram]
  │◀─── ACK ──────────────────────────────  │
  │                                          │  UdpSocket::send_to(datagram, target)
  │                                          │◀─ recv_from() ── 目标
  │◀─── UDP|DATA ──────────────────────────  │  payload = response datagram
  │──── UDP|DATA ────────────────────────▶  │  send_to(datagram, target)
  │──── UDP|DATA ────────────────────────▶  │  ...
  │──── FIN ──────────────────────────────▶  │  关闭 UdpSocket
```

---

## 5. 配置契约表

> 原则：控制面（`config.rs` + `toml`）声明的每个字段，数据面必须有实现。

| 配置字段 | 控制面 | 数据面实现 | 状态 |
|----------|--------|-----------|------|
| `client.mode` | `ProxyMode` 枚举 (proxy/direct/smart/auto) | `tun.rs` HotReloadState.proxy_mode | **已实现** |
| `client.dns` | `String` | `dns.rs` DnsProxy 拦截 UDP:53 | **已实现** |
| `client.cipher` | `CipherPreference` | Noise 握手 CipherOffer | **已实现** |
| `servers[].cipher` | `CipherPreference` | 服务器级密码覆盖 | **已实现** |
| `servers[].protocol` | `TransportProtocol` | socks5.rs 选择 TCP/QUIC 传输 | **已实现** |
| `rules.*` | `RulesConfig` | `rules.rs` RuleEngine (7种规则+GeoIP) | **已实现** |
| `rules.geoip` | `HashMap<String, RuleAction>` | maxminddb 查询 + 国家码匹配 | **已实现** |
| `failover.health_check_interval` | `u64` | `failover.rs` run_health_check_loop() | **已实现** |
| `failover.failover_threshold` | `u32` | 连续失败 N 次后切换服务器 | **已实现** |
| `failover.graceful_migration` | `bool` | 切服时不断已有连接 | 部分实现 |
| `performance.workers` | `u32` | server/bin.rs 自定义 tokio runtime | **已实现** |
| `performance.io_uring` | `bool` | linux_ext.rs tokio_uring | **已实现** |
| `performance.zero_copy` | `bool` | 占位 | 占位 |
| `tls.disguise` | `bool` | 配置 stub，未实现 TLS 伪装 | 占位 |
| `quic.congestion` | `CongestionAlgorithm` | quinn 传输配置 | **已实现** |

---

## 6. 加密协议设计

### 6.1 Noise IK 握手 + 密码协商

```
客户端 ──► 服务端:  Noise IK 第一条消息 + CipherOffer
                       [version:1][count:1][cipher_ids:count]

客户端 ◄── 服务端:  Noise IK 响应消息 + CipherAccept
                       [version:1][cipher_id:1]
```

- Pattern: `Noise_IK_25519_ChaCha20Poly1305_SHA256`
- 密码套件协商嵌入握手载荷，**零额外 RTT**
- 服务端提取客户端静态公钥进行白名单校验（空白名单=开放）

### 6.2 帧协议

```
Wire format: [ver:1][stream_id:4 BE][flags:1][payload_len:2 BE][payload]

Flags:
  SYN  = 0x01  SYN + DATA = TCP/UDP 连接打开
  FIN  = 0x02  优雅关闭
  RST  = 0x04  中断
  ACK  = 0x08  确认
  DATA = 0x10  数据帧
  PING = 0x20  保活探测
  PONG = 0x40  保活响应
  UDP  = 0x80  UDP 模式（与 SYN/DATA 组合）

UDP SYN payload: [TargetAddr encoded][datagram bytes]
```

### 6.3 密钥派生管线

```
Noise IK 握手完成
       │
       ▼
dangerously_get_raw_split() → (k1, k2)
       │                         │
       ▼                         ▼
  HKDF-SHA256              HKDF-SHA256
  info: "phantom-v2-{cipher}-c2s"   info: "phantom-v2-{cipher}-s2c"
       │                         │
       ▼                         ▼
  c2s_key + nonce_prefix      s2c_key + nonce_prefix
       │                         │
       ▼                         ▼
SessionWriter (加密)      SessionReader (解密)
```

### 6.4 QUIC Stream 子密钥派生

```
Connection Noise 握手完成 → conn_keys (k1, k2)
       │
       ▼
split_for_stream(stream, &conn_keys, cipher, is_initiator, stream_counter)
       │
       ▼
HKDF-SHA256
info: "phantom-v3-stream-{stream_id}-{cipher}"
       │
       ▼
per-stream SessionReader / SessionWriter
```

---

## 7. 平台抽象层

| 平台 | TUN 创建 | FFI 入口 | 系统代理 | 打包方式 |
|------|---------|---------|---------|---------|
| macOS | `tun::create_as_async("utun7")` | `phantom_macos_start/stop` | networksetup SOCKS5 自动设置/恢复 | cdylib + SwiftUI |
| Android | VpnService fd → AsyncFd | `phantom_android_start/stop` | VpnService 路由规则 | cdylib + Kotlin |
| CLI | N/A (SOCKS5 only) | `phantom-cli` main | 无 | 统一二进制 |

### 7.1 macOS 系统代理

启动隧道后自动执行：
```bash
networksetup -setsocksfirewallproxy "Wi-Fi" 127.0.0.1 11080
networksetup -setsocksfirewallproxystate "Wi-Fi" on
```

停止时恢复之前的代理设置（保存/恢复机制）。

### 7.2 macOS 代理模式切换

菜单栏分段选择器：
- **Global** → `client.mode = "proxy"`（所有流量走隧道）
- **Auto** → `client.mode = "smart"`（规则引擎分流）
- **Direct** → `client.mode = "direct"`（全局直连）

### 7.3 phantom:// URI 格式

```
phantom://<base64_public_key>@<host>:<port>[?<query>][#<name>]
```

Query 参数：`cipher=`, `proto=`, `congestion=`

CLI 支持 `--server` URI 与 `--config` TOML 组合使用。

---

## 8. 热重载机制

TunProxy 持有 `Arc<Mutex<HotReloadState>>` 共享状态：

```rust
struct HotReloadState {
    proxy_mode: ProxyMode,
    rule_engine: Option<Arc<RuleEngine>>,
}
```

后台任务每 5 秒轮询配置文件 mtime，变更时重新加载 `proxy_mode` 和 `rule_engine`。
`handle_tcp` / `handle_udp` 每次查询时克隆 `Arc` 快照，不持有锁跨 await。

---

## 9. 流量统计

`TrafficStats` 使用 `AtomicU64` 计数器，零锁开销：

| 计数器 | 说明 |
|--------|------|
| `tcp_bytes_up/down` | TCP 上下行字节数 |
| `udp_bytes_up/down` | UDP 上下行字节数 |
| `tcp_connections` | TCP 连接总数 |
| `udp_datagrams_up/down` | UDP 数据报总数 |

暴露为 `127.0.0.1:9150/metrics` Prometheus 端点。

---

## 10. 已知限制与 TODO

| 模块 | 限制 | 优先级 |
|------|------|--------|
| SOCKS5 UDP | UDP ASSOCIATE 未实现（TUN UDP proxy 已覆盖主场景） | Deferred |
| TLS 伪装 | `tls.disguise` 配置 stub，未实现 HTTPS 握手伪装 | Deferred |
| 配置热重载 | 仅 rules + mode，DNS/服务器列表不支持热更新 | P2 |
| 多用户/限速 | 无 | P3 |
| ACME | 无自动证书申请 | P3 |

---

## 11. 测试覆盖

| 层级 | 测试数 | 覆盖范围 |
|------|--------|---------|
| L0 单元测试 | ~80 | 帧协议边界、URI 构建/解析、规则引擎查询、DNS 解析、stats、decode_udp_syn、bootstrap (密钥/白名单/端口探测) |
| L1 配置生效 | 9 | 白名单开放/限制、密码协商矩阵 |
| L1 模块交互 | 15 | DNS→规则、规则→路由、stats→Prometheus |
| L1 全链路 | 7 | TCP echo/大数据/并发、UDP relay、HTTP 隧道 |
| L1 真实场景 | 4 | Mock 百度、真百度（ignored） |
| L1 性能 | 17 | 吞吐量、延迟、并发（10 ignored） |
| L2 系统 | 5 | CLI 自举（auto 生成 key/URI）、端口递增 fallback、keygen 子命令已删除、version、复用已有 key |
