# Phantom 架构文档

## 1. 项目概述

Phantom 是一个高性能加密代理隧道，基于 Rust 构建，采用 `crates/` + `server/` + `client/` 三级 workspace 结构。核心设计目标：
- **数据面 100% Rust**：macOS/Android 原生客户端通过 FFI 一次性传递 TUN fd，之后零跨语言开销
- **控制面-数据面契约**：配置声明的每一项必须在数据面有对应实现
- **零拷贝传输**：基于 `BytesMut` 复用的帧协议，消除每帧堆分配
- **QUIC 多路复用**：Noise IK 握手一次，后续 stream 通过 HKDF 派生子密钥

---

## 2. 系统架构

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                                客户端 (Client)                                │
│  ┌─────────────┐   SOCKS5   ┌──────────────────┐   TCP/QUIC   ┌──────────┐  │
│  │  应用程序    │ ──────────▶│  Phantom Client  │ ────────────▶│  Server  │  │
│  │ (浏览器等)   │            │  (SOCKS5 + TUN)  │  Noise+AEAD  │          │  │
│  └─────────────┘            └──────────────────┘              └──────────┘  │
│         │                            │                                      │
│         │       TUN (透明代理)         │                                      │
│         └────────────────────────────┘                                      │
│                                                                               │
│  平台层:                                                                      │
│    macOS: utun7 + SwiftUI 菜单栏 (FFI: phantom_macos_start/stop)             │
│    Android: VpnService fd + Jetpack Compose (JNI: phantom_android_start)     │
│    CLI: phantom-cli (tokio main, SOCKS5 only)                                │
└─────────────────────────────────────────────────────────────────────────────┘

                                    ↓ TCP/QUIC

┌─────────────────────────────────────────────────────────────────────────────┐
│                                服务端 (Server)                                │
│  ┌──────────────────┐     TCP relay / UDP relay (future)    ┌──────────┐   │
│  │  Phantom Server  │ ─────────────────────────────────────▶ │ 目标站点  │   │
│  │  (Noise responder)│                                        │          │   │
│  └──────────────────┘                                        └──────────┘   │
│         │                                                                     │
│    Linux io_uring (optional)                                                 │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## 3. Crate 职责矩阵

| Crate | 路径 | 职责 | 依赖 |
|-------|------|------|------|
| `phantom-core` | `crates/phantom-core` | 共享类型、配置(TOML)、错误、常量、BufferPool | `serde`, `toml`, `crossbeam-queue` |
| `phantom-crypto` | `crates/phantom-crypto` | 密码套件、AEAD 状态、Noise IK 握手、密钥管理、会话派生 | `snow`, `x25519-dalek`, `aes-gcm`, `chacha20poly1305`, `ascon-aead` |
| `phantom-protocol` | `crates/phantom-protocol` | 线路帧格式、地址编码、编解码器(`FrameReader`/`FrameWriter`) | `bytes`, `phantom-core` |
| `phantom-transport` | `crates/phantom-transport` | TCP/QUIC 传输抽象、`Transport` trait | `tokio`, `quinn` |
| `phantom-client` | `client/` | SOCKS5 代理、TUN 透明代理、故障转移、规则引擎、DNS 劫持 | `phantom-*`, `tun`, `etherparse`, `ipnet`, `regex` |
| `phantom-cli` | `client/cli` | 命令行入口 (`phantom client` / `phantom server` / `phantom keygen`) | `phantom-client`, `phantom-server` |
| `phantom-server` | `server/` | 服务端连接处理、双向中继、io_uring (Linux) | `phantom-*`, `quinn`, `tokio-uring`(opt) |
| `phantom-e2e` | `phantom-e2e/` | 端到端集成测试、echo server、fixture | `phantom-client`, `phantom-server` |
| `phantom-bench` | `phantom-bench/` | Criterion 性能基准测试 | `phantom-*`, `criterion` |

---

## 4. 数据流详细图

### 4.1 SOCKS5 路径（CLI / 浏览器模式）

```
应用 → SOCKS5 CONNECT → Phantom Client SOCKS5 listener
                              │
                              ▼
                    ┌──────────────────┐
                    │ NoiseInitiator   │ ──► TCP connect to server
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
 RuleEngine  DnsProxy    RuleEngine
 (Direct/    (upstream   (Direct →
  Proxy/     DNS)         local UDP socket
  Reject)              Proxy → drop)
    │
    ▼
Direct ──► tcp_direct_relay_task (直连目标)
Proxy  ──► tcp_relay_task ──► SOCKS5 ──► Noise tunnel ──► Server
Reject ──► send_tcp_rst()
```

---

## 5. 配置契约表

> 原则：控制面（`config.rs` + `toml`）声明的每个字段，数据面必须有实现。

| 配置字段 | 控制面 | 数据面实现 | 状态 |
|----------|--------|-----------|------|
| `client.mode = "smart"` | `ProxyMode` 枚举 | `tun.rs` SYN 时查 `RuleEngine` | **已实现** |
| `client.dns` | `String` | `dns.rs` `DnsProxy` 拦截 UDP:53 | **已实现** |
| `rules.*` | `RulesConfig` | `rules.rs` `RuleEngine` | **已实现** |
| `failover.health_check_interval` | `u64` | `failover.rs` `run_health_check_loop()` | **已实现** |
| `failover.graceful_migration` | `bool` | 切服时直接断开，**未实现迁移** | 部分实现 |
| `performance.workers` | `u32` | `server/src/bin.rs` 自定义 `tokio::runtime` | **已实现** |
| `performance.io_uring` | `bool` | `linux_ext.rs` `tokio_uring` 真实现 | **已实现** |
| `performance.zero_copy` | `bool` | `linux_ext.rs` 占位，注册 buffer pool 未接入 relay | 占位 |
| `tls.disguise` | `bool` | 服务端仍使用自签名证书，无 TLS 伪装 | 占位 |
| `quic.enable` | `bool` | `server/src/lib.rs` QUIC endpoint | **已实现** |
| `quic.congestion` | `CongestionAlgorithm` | `quinn` 传输配置 | **已实现** |

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
- 服务端提取客户端静态公钥进行白名单校验

### 6.2 密钥派生管线

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

- 每个方向独立 `AeadState`，无 `Arc<Mutex>` 共享
- Nonce 构造：`[4字节前缀][0填充][8字节计数器]`

### 6.3 QUIC Stream 子密钥派生（P2）

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

- 第一个 bi-stream：完整 Noise IK 握手
- 后续 bi-stream：隐式 stream counter（1, 2, 3…），双方按 `accept_bi()` / `open_bi()` 顺序计数

---

## 7. 平台抽象层

| 平台 | TUN 创建 | TUN 读写 | FFI 入口 | 打包方式 |
|------|---------|---------|---------|---------|
| macOS | `tun::create_as_async("utun7")` | `AsyncRead`/`AsyncWrite` | `phantom_macos_start(config_toml, len)` | `cdylib` + SwiftUI |
| Android | `VpnService.Builder.establish()` → `detachFd()` → `AsyncFd` | `libc::read/write` + `AsyncFd` | `phantom_android_start(fd, config_json, len)` | `cdylib` + Kotlin VpnService |
| Linux CLI | N/A (SOCKS5 only) | N/A | `phantom-cli` main | 统一二进制 |

---

## 8. 路由规则引擎（Smart 模式）

### 8.1 规则类型与匹配优先级

| 优先级 | 类型 | 数据结构 | 查询复杂度 |
|--------|------|---------|-----------|
| 1 | Domain full | `HashMap<String, Action>` | O(1) |
| 2 | Domain suffix | `DomainSuffixTrie` | O(域名深度) |
| 3 | Domain keyword | `Vec<(String, Action)>` | O(N) |
| 4 | Domain regex | `Vec<(Regex, Action)>` | O(N) |
| 5 | IP-CIDR | 分层 `Vec<(Net, Action)>` 按 prefix len 降序 | O(层数) |
| 6 | Port | `HashMap<u16, Action>` | O(1) |
| 7 | GEOIP | `maxminddb::Reader` (optional feature) | O(1) |
| 8 | Final | 常量 | O(1) |

### 8.2 DNS 缓存与域名关联

TUN 模式只有 IP 地址，没有域名。Smart 模式通过以下方式关联域名：
1. DNS 劫持模块拦截 UDP:53 查询，提取域名
2. DNS 响应解析 A 记录，建立 `IP → domain` 缓存
3. TCP SYN 时查缓存获取域名，再查规则引擎

---

## 9. 已知限制与 TODO

| 模块 | 限制 | 优先级 |
|------|------|--------|
| TUN UDP | Proxy 模式 UDP 丢弃，未走 Phantom 隧道 | P0 |
| SOCKS5 UDP | UDP ASSOCIATE 未实现（服务端也不支持 UDP relay） | P0 |
| IPv6 | TUN 层多处硬编码 IPv4，IPv6 包被丢弃 | P1 |
| GEOIP | `maxminddb` 已集成但规则未预索引，实际未生效 | P2 |
| TLS 伪装 | `tls.disguise` 配置 stub，未实现 HTTPS 握手伪装 | P2 |
| 配置热重载 | 启动后配置不可变 | P3 |
| 流量统计 | 仅 `tracing::debug!`，无结构化 metrics | P3 |
| 多用户/限速 | 无 | P3 |
| ACME | 无自动证书申请 | P3 |
