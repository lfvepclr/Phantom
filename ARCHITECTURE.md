# Phantom 架构文档

## 1. 项目概述

Phantom 是一个高性能加密代理隧道，基于 Rust 构建，采用 `core/` + `server/` + `client/` 三级 workspace 结构。核心设计目标：
- **数据面 100% Rust**：macOS/Android 原生客户端通过 FFI 一次性传递 TUN fd，之后零跨语言开销
- **控制面-数据面契约**：配置声明的每一项必须在数据面有对应实现
- **零拷贝传输**：基于 `BytesMut` 复用的帧协议，消除每帧堆分配
- **QUIC 多路复用**：Noise-over-QUIC（quinn-hyphae，100% Rust），连接级 Noise IK 握手一次，后续 stream 复用 QUIC 原生多路复用
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
│    HarmonyOS: VpnExtensionAbility fd + ArkUI (NAPI)                        │
│    Linux 路由器: phantom0 + ip rule 策略路由 + iptables (--tun --gateway)   │
│    CLI: phantom-cli (tokio main, SOCKS5 或 --tun)                          │
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
| `phantom-client` | `client/` | SOCKS5 代理、TUN 透明代理、规则引擎、DNS 劫持、UDP relay、流量统计、热重载、Linux 网关（`gateway`） | `phantom-*`, `tun`, `etherparse`, `ipnet`, `regex` |
| `phantom-cli` | `client/cli` | CLI 入口 (`phantom client` / `server`)，`client` 支持 SOCKS5 / `--tun` / `--tun --gateway`，`server` 支持 auto / interactive / load 三种模式 | `phantom-client`, `phantom-server`, `phantom-core` |
| `phantom-server` | `server/` | 服务端连接处理、TCP relay、UDP relay、io_uring、`bootstrap` 模块（自举 / 交互 / URI 反向解析） | `phantom-*`, `quinn`, `tokio-uring`(opt) |
| `phantom-e2e` | `tests/` | E2E 测试、HTTP/TCP/UDP echo、Mock 百度、网络模拟 | `phantom-*`, `axum` |
| `phantom-bench` | `tests/bench/` | 性能基准测试 | `phantom-*`, `divan` |

---

## 4. 数据流详细图

### 4.1 本地入站路径（CLI / 浏览器模式）

本地监听端口同时讲 SOCKS5 与 HTTP：`http_proxy::handle_inbound` peek 首字节，`0x05` 走 SOCKS5，否则走 HTTP。

```
应用 → SOCKS5 CONNECT / UDP ASSOCIATE  ┐
     → HTTP CONNECT / 绝对 URI GET/POST ├─► Phantom Client inbound listener
                                        │   (首字节嗅探，可选 proxy_auth 认证)
                                        ▼
                              ┌──────────────────┐
                              │ NoiseInitiator   │ ──► TCP/QUIC connect
                              │ IK handshake     │ ◄── Cipher negotiation
                              │ split_after_     │     (QUIC: 池化连接+
                              │ handshake()      │      明文分帧)
                              └──────────────────┘
                                        │
                              ┌─────────┴──────────┐
                              │ FrameWriter::syn() │ ──► encrypted SYN frame
                              │ FrameReader read   │ ◄── ACK / RST
                              └────────────────────┘
                                        │
                              ┌─────────┴──────────┐
                              │ relay_socks5_tunnel│ TCP 双向 relay
                              │ udp_relay 管道     │ UDP: per-target 流表
                              └────────────────────┘
```

- **HTTP CONNECT**：隧道建立后回 `200 Connection Established`，之后原样 relay。
- **绝对 URI GET/POST**：请求行重写为 origin form，剥除 `Proxy-*` 头（凭据不出本地），强制 `Connection: close`（避免 keep-alive 复用连接跳 host 时无法重定向）。
- **UDP ASSOCIATE**：绑定控制连接本地 IP 的 UDP socket，回复真实 BND.ADDR/PORT；FRAG≠0 丢弃，仅接受首个数据报来源地址，控制连接 EOF 即销毁关联；每个 target 一条隧道流（SYN 携带首包，免额外 RTT）。

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
| `servers[].psk` | `String` (base64) | `config.rs` `decode_psk()` → Noise `psk1`；缺失则报错 | **已实现** |
| `rules.*` | `RulesConfig` | `rules.rs` RuleEngine (7种规则+GeoIP) | **已实现** |
| `rules.geoip` | `HashMap<String, RuleAction>` | maxminddb 查询 + 国家码匹配 | **已实现** |
| `client.dns`（热更） | `String` | `dns.rs` `DnsProxy::set_upstream()` | **已实现** |
| `client.metrics_listen` | `String` | `client/src/stats.rs` `serve_metrics()`，SOCKS5/TUN 共享 | **已实现** |
| `client.listen` | `String` | 入站监听地址，`0.0.0.0` 即局域网共享；默认 `127.0.0.1:1080` | **已实现** |
| `client.proxy_auth` | `Option<ProxyAuthConfig>` | SOCKS5 RFC1929 + HTTP Basic（407 质询），恒定时间比较，凭据不转发上游 | **已实现** |
| `servers[]`（热更） | `Vec<ServerEntry>` | `failover.rs` `FailoverManager::reload()` | **已实现** |
| `failover.health_check_interval` | `u64` | `failover.rs` run_health_check_loop() | **已实现** |
| `failover.failover_threshold` | `u32` | 连续失败 N 次后切换服务器 | **已实现** |
| `failover.graceful_migration` | `bool` | `false` 时切服广播 migration epoch，在飞隧道主动断连；`true`（默认）旧服隧道自然排空 | **已实现** |
| `hello.timeout` | `u64` | `client/src/hello.rs` Hello-ACK 等待超时 | **已实现** |
| `hello.targets` | `Vec<String>` | 随 Hello 帧下发，服务端优先探测，缺省回落 `verification_url` → 内置目标 | **已实现** |
| `server.verification_url` | `Option<String>` | `server/src/handler.rs` 自定义外网探测目标 | **已实现** |
| `performance.workers` | `u32` | server/bin.rs 自定义 tokio runtime | **已实现** |
| `performance.io_uring` | `bool` | linux_ext.rs tokio_uring | **已实现** |
| `quic.congestion` | `CongestionAlgorithm` | quinn 传输配置 | **已实现** |
| `quic.max_streams` | `u32` | quinn `max_concurrent_bidi_streams` | **已实现** |
| `quic.keep_alive_interval` | `u64` | quinn `keep_alive_interval`（秒，0=禁用） | **已实现** |

---

## 6. 加密协议设计

### 6.1 Noise IKpsk1 握手 + 密码协商

```
客户端 ──► 服务端:  Noise IKpsk1 第一条消息 + CipherOffer
                       [version:1][count:1][cipher_ids:count]

客户端 ◄── 服务端:  Noise 响应消息 + CipherAccept
                       [version:1][cipher_id:1]
```

- Pattern: `Noise_IKpsk1_25519_ChaChaPoly_SHA256`
- 密码套件协商嵌入握手载荷，**零额外 RTT**
- 服务端提取客户端静态公钥进行白名单校验（空白名单=开放）

#### PSK 位置为何是 `psk1` 而不是 `psk2`

Noise 的 `psk1` 把 PSK token 放在**第一条**消息末尾（`-> e, es, s, ss, psk`），
`MixKeyAndHash(psk)` 在该消息的 payload 被封装**之前**执行。因此持有不同 PSK 的
 responder 无法解密第一条消息，**直接断开且不回应任何数据** —— 探测者看到的是
`early eof`，与端口上无服务无法区分。

`psk2` 曾被先尝试并否定：它把 PSK 混入**第二条**消息，所以 responder 会正常回应
一个“公钥正确但无 PSK”的探测，仅 initiator 侧能发现不匹配 —— 这泄露了
“此处有服务”的信号，恰好是 PSK 要阻止的事。回归测试见
`core/src/crypto/noise.rs` 的 `handshake_fails_when_psk_differs` 与
`pattern_binds_the_psk_to_the_first_message`。

PSK 是**叠加**在 ephemeral DH 之上的第二因子，不替代 DH，因此前向保密不丢；
双向静态公钥认证仍由 IK 提供。对称 PSK 本身抗量子，构成向后量子时代的过渡手段。

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

### 6.4 QUIC 传输：Noise 替代 TLS

QUIC 的加密层由 Noise（quinn-hyphae + RustCrypto 后端）取代 TLS——零证书、零 C 依赖：

```
QuicAuth { local_secret, remote_public, psk, cipher }
       │
       ▼
Noise_IK_25519_{AESGCM,ChaChaPoly}_SHA256 + prologue = PSK
       │（hyphae 不支持 pskN 修饰符；prologue 在首条消息密封前混入握手哈希，
       │  与 TCP 的 psk1 等效：PSK 错误 → 握手失败 → 服务端零回应）
       ▼
连接级认证完成（服务端经 peer_static_key() 提取客户端公钥做白名单校验）
       │
       ▼
每条 bi-stream 直接跑帧协议（长度前缀明文分帧）——QUIC 本身已加密，
不再叠加 SessionReader/SessionWriter，消除了 QUIC over QUIC 的双层加密开销
```

注意：AEAD 由 Noise pattern 字符串固定（TLS 语境的套件协商不适用于 Noise），双端必须配置一致的 `cipher=`；`ascon-128` 在 QUIC 上不可用（RustCrypto 后端无 ASCON），指定时会在 endpoint 创建期明确报错。

### 6.5 HelloWorld 连接验证协议

启动 SOCKS5 / TUN 数据面之前，客户端会先通过一条**独立的 Noise 会话**完成一次 Hello/Hello-ACK 握手，验证 `客户端 → 服务端 → 外网` 完整链路可用，避免"本地代理已启动就显示已连接"的误导。

```
客户端                                 服务端
  │                                      │
  │──── TCP/QUIC connect ─────────────▶  │
  │──── Noise 握手 ──────────────────▶  │（TCP：IKpsk1 独立握手；QUIC：Noise 内嵌于 QUIC 握手）
  │◄─── Noise 握手响应 ───────────────  │
  │                                      │
  │──── DATA stream_id=0 ─────────────▶  │  payload = "PH/HELLO" + {nonce}
  │                                      │  http_probe(captive.apple.com)
  │◄─── DATA stream_id=0 ──────────────  │  payload = "PH/HELLO_ACK" + {ok, message, ts}
  │                                      │
  │  ok=true  → 打开本地 SOCKS5 / TUN    │
  │  ok=false → 报错，不进入数据面        │
```

设计要点：
- 复用已建立的 Noise 加密会话，Hello 帧本身也是加密的。
- 使用 `stream_id = 0` 作为保留控制流，与 SOCKS5 relay 的 `stream_id >= 1` 不冲突。
- 不新增 FrameFlags，Hello 帧是普通的 `DATA` 帧，通过 payload 前缀 `PH/HELLO` / `PH/HELLO_ACK` 识别，保持与旧版客户端的最大兼容（旧版服务端会把该帧当普通 SYN 处理并关闭连接，不会误判）。
- 服务端默认探测 `http://captive.apple.com/hotspot-detect.html`，失败时回退 `http://detectportal.firefox.com/success.txt`；可通过 `server.toml` 的 `verification_url` 自定义。

---

## 7. 平台抽象层

| 平台 | TUN 创建 | FFI 入口 | 系统代理 | 打包方式 |
|------|---------|---------|---------|---------|
| macOS | `tun::create_as_async("utun7")` | `phantom_macos_start/stop` | networksetup SOCKS5 自动设置/恢复 | cdylib + SwiftUI |
| Android | VpnService fd → AsyncFd | `phantom_android_start/stop` | VpnService 路由规则 | cdylib + Kotlin |
| HarmonyOS | VpnExtensionAbility fd → AsyncFd | NAPI 模块 | VpnExtensionAbility 路由 | cdylib + ArkTS |
| Linux 路由器 | `TunDevice::create_with(TunSettings)` | N/A（直跑 CLI） | `ip rule` 策略路由 + iptables | aarch64-musl 静态二进制 |
| CLI | 同上（`--tun` 可选） | `phantom-cli` main | 无 | 统一二进制 |

### 7.0 Linux 网关数据面（路由器）

`client/src/gateway.rs` 把转发的 LAN 流量引入 TUN：

```
LAN 客户端 ──转发──▶ ip rule iif br0 ─▶ table 200 ─▶ default dev phantom0
                                          │
                        优先级 9040：to <私有网段> lookup main（放行）
                        优先级 9050：iif br0 lookup 200（入隧道）
```

关键设计：**按入向接口 `iif` 选路，而不是按源网段**。路由器自身发起的流量
（包括 Phantom 到服务端的隧道连接）没有 `iif`，因此永不匹配 table 200，
从根源上消除了回环，无需为服务端 IP 或 WAN 网关做特例路由。

指令集的构造（`GatewayConfig::plan` / `teardown_plan`）与执行分离，因此可在
无 root、无真实网卡的情况下单测；`Gateway` 的 `Drop` 负责完整回滚。

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
    server: Option<ServerEntry>,   // 隧道化 UDP 流使用的服务器
}
```

后台任务每 5 秒轮询配置文件 mtime，变更时由 `apply_reload()` 统一下发：

| 目标 | 载体 | 行为 |
|------|------|------|
| `proxy_mode` | `HotReloadState` | 直接替换 |
| `rule_engine` | `HotReloadState` | 重建；**解析失败时保留旧引擎**，避免退化为“全代理” |
| `server` | `HotReloadState` | 新 UDP 隧道流指向新首位服务器 |
| DNS 上游 | `DnsProxy.upstream`（`RwLock`） | 重定向；不重建 socket，飞行中的 pending 查询仍能回流 |
| 服务器池 | `FailoverManager.pool`（`RwLock`） | 整体替换；**当前活跃服务器存活则保持不动** |
| failover 调优 | `FailoverManager.tuning` | 健康检查间隔变更时重建 ticker |

`handle_tcp` / `handle_udp` 每次查询时克隆 `Arc` 快照，不持有锁跳 await。
`FailoverManager::select_server()` 返回**拥有权快照**而非引用，这是池可被热替换的前提。
飞行中的健康探测结果会校验槽位身份，避免重载后误伤新池。

---

## 9. 流量统计

`TrafficStats` 使用 `AtomicU64` 计数器，零锁开销：

| 计数器 | 说明 |
|--------|------|
| `tcp_bytes_up/down` | TCP 上下行字节数 |
| `udp_bytes_up/down` | UDP 上下行字节数 |
| `tcp_connections` | TCP 连接总数 |
| `udp_datagrams_up/down` | UDP 数据报总数 |

暴露为 Prometheus 端点（`client.metrics_listen` 可配，默认 `127.0.0.1:9150/metrics`）；SOCKS5 与 TUN 运行时共享同一 `TrafficStats` 实例与同一个 HTTP 端点。

---

## 10. 已知限制与 TODO

| 模块 | 限制 | 优先级 |
|------|------|--------|
| SOCKS5 UDP | UDP ASSOCIATE 未实现（TUN UDP proxy 已覆盖主场景） | Deferred |
| 网关 IPv6 | `gateway.rs` 只下发 IPv4 策略路由，`ip -6 rule` 未实现 | P3 |
| 网关规则寿命 | Asuswrt 重建 NAT 时会清掉 iptables 条目，现靠 `nat-start` 钩子重启进程补回 | P3 |
| 多用户/限速 | 无 | P3 |
| ACME | 无自动证书申请 | P3 |

---

## 11. 测试覆盖

| 层级 | 测试数 | 覆盖范围 |
|------|--------|---------|
| L0 单元测试 | 140 | 帧协议边界、URI 构建/解析、规则引擎查询、DNS 解析与上游热切换、failover 池热重载/探测切换/migration epoch 广播、stats、hello.targets 解析过滤、QUIC max_streams 行为上限、decode_udp_syn、bootstrap |
| L0 CLI 单测 | 6 | `--tun-addr` CIDR 解析、clap 参数依赖关系 |
| L0 网关单测（Linux） | 12 | ip rule / iptables 指令集、优先级排序、回滚对称性、保留路由表校验 |
| L1 配置生效 | 9 | 白名单开放/限制、密码协商矩阵 |
| L1 模块交互 | 15 | DNS→规则、规则→路由、stats→Prometheus |
| L1 全链路 | 7 | TCP echo/大数据/并发、UDP relay、HTTP 隧道 |
| L1 QUIC 复用 | 2 | 8 隧道共享 1 连接无串流、错误 PSK 零握手（quic_mux） |
| L1 本地入站 | 9 | UDP ASSOCIATE echo（TCP/QUIC）、多 target 流表、FRAG/陌生源丢弃、HTTP CONNECT、绝对 URI 重写、同端口嗅探分流、RFC1929/Basic 认证 |
| L1 真实场景 | 4 | Mock 百度、真百度（ignored） |
| L1 性能 | 17 | 吞吐量、延迟、并发（10 ignored） |
| L2 系统 | 11 | CLI 自举、端口递增 fallback、keygen 已删除、version、密钥复用、TUN/网关参数校验与平台限制 |
