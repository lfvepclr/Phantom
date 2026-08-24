# Phantom Tunnel 项目执行计划

## 1. 项目概述

Phantom（幽灵）是一个高性能加密代理隧道，基于 Rust 构建，使用 Noise IK 协议进行认证和密钥交换，支持自适应加密套件选择。支持 SOCKS5 代理和 TUN 透明代理两种模式，提供 macOS / Android / HarmonyOS / 路由器 / CLI 多平台客户端。

**核心目标**: 配置简单，性能极高，容易维护，代码模块化，现代化。

---

## 2. 已完成功能清单

### 2.1 核心协议层 ✅

| 功能 | 状态 | 说明 |
|------|------|------|
| Noise IK 握手 | ✅ | 双向公钥认证，零额外 RTT |
| PSK 抗主动探测 | ✅ | `Noise_IKpsk1`：PSK 绑定首条消息，无 PSK 的探测得不到任何回应 |
| 密码协商 | ✅ | CipherOffer/CipherAccept 嵌入握手载荷 |
| 四层密码套件 | ✅ | AES-256-GCM / AES-128-GCM / ASCON-128 / ChaCha20-Poly1305 |
| 自适应选择 | ✅ | auto 检测 CPU 能力 |
| 密钥派生 | ✅ | HKDF-SHA256 双向独立会话密钥 |
| 帧协议 | ✅ | 8 字节头，SYN/FIN/RST/ACK/DATA/PING/PONG/UDP |
| QUIC 多路复用 | ✅ | Noise-over-QUIC（quinn-hyphae，100% Rust）：连接级握手一次，stream 复用 QUIC 原生多路复用 |

### 2.2 服务端 ✅

| 功能 | 状态 | 说明 |
|------|------|------|
| TCP relay | ✅ | Noise 握手 → SYN → TCP connect → 双向 relay |
| UDP relay | ✅ | SYN\|UDP 帧 → UdpSocket → recv_from → UDP\|DATA 帧 |
| 客户端白名单 | ✅ | 空白名单 = 开放模式，非空 = 公钥验证 |
| io_uring | ✅ | Linux 5.1+ 可选 |
| 多 worker | ✅ | performance.workers 自定义 tokio runtime |

### 2.3 客户端 ✅

| 功能 | 状态 | 说明 |
|------|------|------|
| SOCKS5 代理 | ✅ | negotiate → CONNECT → tunnel → relay |
| TUN 透明代理 | ✅ | macOS utun7 / Linux phantom0 / Android VpnService fd |
| Linux 网关模式 | ✅ | `--gateway`：`ip rule iif` 策略路由 + iptables，退出自动回滚 |
| 代理模式 | ✅ | Global(Auto) / Smart(规则) / Direct |
| 规则引擎 | ✅ | 7 种规则：domain-full/suffix/keyword/regex / ip-cidr / port / geoip |
| DNS 劫持 | ✅ | UDP:53 拦截 → 上游 DNS → IP→域名缓存 |
| UDP Direct relay | ✅ | 本地 UdpSocket 直连 |
| UDP Proxy relay | ✅ | Noise 隧道 + UDP SYN 帧转发 |
| Failover | ✅ | 多服务器自动切换，TCP 探测健康检查，服务器池可热重载 |
| 系统代理自启 | ✅ | macOS networksetup 自动设置/恢复 SOCKS5 |
| URI 单串配置 | ✅ | phantom://base64key@host:port?cipher=&proto=#name |
| 配置热重载 | ✅ | mtime 轮询 5s：rules / mode / DNS 上游 / servers / failover 调优 |
| 流量统计 | ✅ | AtomicU64 计数器 + Prometheus :9150/metrics |

### 2.4 平台客户端 ✅

| 平台 | 状态 | 说明 |
|------|------|------|
| CLI | ✅ | phantom client / server；client 支持 SOCKS5 / `--tun` / `--tun --gateway` 三种形态；server 支持 auto / interactive / load 三种模式 |
| macOS | ✅ | SwiftUI 菜单栏，Global/Auto/Direct 模式切换，系统代理自启 |
| Android | ✅ | Kotlin VpnService + Jetpack Compose，TUN fd 传入 |
| HarmonyOS NEXT | ✅ | ArkUI + VpnExtensionAbility，TUN fd 传入 |
| 路由器（华硕 AX86U Pro） | ✅ | aarch64-unknown-linux-musl 静态二进制 + Asuswrt-Merlin 部署脚本 |

---

## 3. 测试覆盖

### 3.1 测试分层

| 层级 | 测试数 | 文件 | 覆盖范围 |
|------|--------|------|---------|
| L0 单元测试 | 109 | 各 crate #[cfg(test)] | 帧协议边界、URI 构建/解析、规则引擎、DNS（上游解析/热切换）、failover（池热重载/探测切换）、stats、handler、bootstrap |
| L0 CLI 单测 | 6 | client/cli 内联 | `--tun-addr` CIDR 解析、clap 参数依赖 |
| L0 网关单测 | 12 | client/src/gateway.rs（Linux） | 指令集生成、优先级排序、回滚对称性、保留路由表校验 |
| L1 配置生效 | 9 | config_effect, cipher_matrix | 白名单、密码协商、echo 模式 |
| L1 模块交互 | 15 | rule_engine, dns_hijack, stats_metrics | DNS→规则、规则→路由、stats→Prometheus |
| L1 全链路 | 7 | full_link_tcp, full_link_udp, http_tunnel | TCP echo/大数据/并发、UDP relay |
| L1 真实场景 | 4 | real_world | Mock 百度 + 真百度 (2 ignored) |
| L1 性能 | 17 | performance, throughput | 吞吐量/延迟/并发 (10 ignored) |
| L2 系统 | 11 | cli_system | CLI 自举、端口递增 fallback、keygen 已删除、version、密钥复用、TUN/网关参数校验与平台限制 |

### 3.2 已知不稳定 / 阻塞的测试

以下两个先存问题已于阶段 6 定位根因并修复。二者均为测试基建问题——产品的 relay
路径（`handler::relay`）本就是 `try_join!` 全双工，不存在缺陷。

| 测试 | 现象 | 根因与修复 |
|---|---|---|
| `correctness::tcp_*_echo_large` | 永久挂起（>20 min 不返回） | `echo_data` 先写满 10MB 再读：relay 环路缓冲耗尽后写读双方互等，半双工死锁。已改为 `tokio::join!` 全双工泵送（`measure_echo_throughput` 同改），并加 180s 超时兜底 |
| `http_tunnel::http_get_ip_through_tunnel` 等 | 约 25%~75% 概率返回空响应体 | 请求后立刻发 FIN（半关闭）：hyper 将 EOF 视为连接拆除，丢弃未发出的响应（最小复现：axum + 立即 FIN，空响应 ~60-70%）。已新增无 FIN 的 `exchange_data`，HTTP 用例改由服务端 `Connection: close` 主导收尾 |

### 3.3 关键测试场景

- **密码协商矩阵**: 每种 cipher 全链路 echo，验证协商 + 数据完整性
- **白名单配置生效**: 空=开放、非空=拒绝未知密钥、匹配=接受
- **规则引擎交互**: domain→Proxy、ip-cidr→Direct、域名优先级 > IP
- **DNS→规则**: DNS 查询缓存 IP→域名，后续 TCP SYN 查到域名走规则
- **UDP 全链路**: UDP SYN 帧 → 服务端 UdpSocket → 响应回传
- **Mock 百度**: 通过隧道访问 mock 百度页面，验证 HTML 内容
- **热重载**: 运行中修改配置文件，5s 内规则/模式/DNS 上游/服务器池生效
- **网关指令集**: `ip rule` / `iptables` 指令按优先级排序、安装与回滚严格对称、保留路由表被拒绝
- **TUN/网关参数**: `--gateway` 依赖 `--tun`、LAN 参数依赖 `--gateway`、非 Linux 平台明确拒绝
- **失败保留语义**: 规则集非法时保留旧引擎，`client.dns` 非法时保留旧上游（均不退化）
- **端口探测确定性**: 测试持有真实监听器占位，不再依赖「探测后立即释放」的竞态假设

---

## 4. 待实现功能

| 功能 | 优先级 | 说明 |
|------|--------|------|
| SOCKS5 UDP ASSOCIATE | Deferred | TUN UDP proxy 已覆盖主场景 |
| 多用户/限速 | P3 | 需协议扩展 |
| ACME 自动证书 | P3 | 需 CA 集成 |
| 后量子密钥交换 | P3 | X25519 → ML-KEM 路线图 |
| IPv6 网关路由 | P3 | `gateway.rs` 目前只下发 IPv4 策略路由（`ip -6 rule` 未实现） |
| 网关防火墙自修复 | P3 | Asuswrt 重建 NAT 时现依赖 `nat-start` 钩子重启进程补回规则 |

---

## 5. 加密协议设计

### 5.1 三层自适应密码套件

```
┌─────────────────────────────────────────────────────────┐
│  第一梯队: AES-256-GCM (硬件加速)                         │
│  条件: x86_64 AES-NI / aarch64 ARM CE                   │
│  吞吐量: 5-12 GB/s                                      │
├─────────────────────────────────────────────────────────┤
│  第二梯队: AES-128-GCM (平衡)                            │
│  条件: 中端 ARM 设备，AES CE 可用但功耗敏感               │
│  吞吐量: 3-8 GB/s                                       │
├─────────────────────────────────────────────────────────┤
│  第三梯队: ASCON-128 (NIST SP 800-232)                   │
│  条件: 无 AES 硬件加速                                   │
│  吞吐量: ~1-2 GB/s (比软件 AES 快 5-10 倍)              │
├─────────────────────────────────────────────────────────┤
│  第四梯队: ChaCha20-Poly1305 (最后备选)                   │
│  条件: 兼容性场景                                       │
│  吞吐量: ~1-2 GB/s                                      │
└─────────────────────────────────────────────────────────┘
```

### 5.2 自动检测逻辑

```rust
pub fn auto_detect() -> CipherSuite {
    // x86_64 + AES-NI → Aes256Gcm
    // aarch64 + ARM CE → Aes256Gcm
    // 其他 → Ascon128 (无硬加速时比软件 AES 快 5-10x)
}
```

### 5.3 密码协商协议

协商嵌入 Noise IK 握手消息载荷，零额外往返：

```
客户端 → 服务端:  Noise IK 第一条消息 + CipherOffer
                  [version:1][count:1][cipher_ids:count]

服务端 → 客户端:  Noise IK 响应消息 + CipherAccept
                  [version:1][cipher_id:1]
```

### 5.4 密钥派生管线

```
Noise IK 握手完成
       ↓
dangerously_get_raw_split() → (k1, k2)
       ↓                         ↓
  k1 = C→S 方向              k2 = S→C 方向
       ↓                         ↓
  HKDF-SHA256                 HKDF-SHA256
  info: "phantom-v2-{cipher}-c2s"    info: "phantom-v2-{cipher}-s2c"
       ↓                         ↓
  c2s_key + c2s_nonce_prefix    s2c_key + s2c_nonce_prefix
       ↓                         ↓
  Initiator: write=... read=...    Responder: write=... read=...
```

### 5.5 会话加密层

握手完成后，切换到自定义 SessionReader/SessionWriter：
- 每个方向独立 AeadState（密钥 + nonce 计数器）
- 无 Arc<Mutex> 共享，读写完全并发
- 线路格式不变：[2字节长度BE][密文+16字节认证标签]
- nonce 构造：[4字节前缀][0填充][8字节计数器]

---

## 6. 各平台性能预期

| 平台 | 推荐密码 | 预期吞吐量 |
|------|---------|-----------|
| Ubuntu x86_64 (AES-NI) | AES-256-GCM | 5-10 GB/s |
| macOS Apple Silicon (M1-M4) | AES-256-GCM | 5-8 GB/s |
| Android 旗舰 (AES CE) | AES-256-GCM | 3-6 GB/s |
| Android 旧设备 (无 AES CE) | ASCON-128 | ~1-2 GB/s |
| 华硕路由器 (新 ARM) | AES-128-GCM | 2-4 GB/s |
| 华硕路由器 (旧 MIPS) | ASCON-128 | ~0.5-1 GB/s |

---

## 7. 配置参考

### 7.1 客户端配置 (client.toml)

```toml
[[servers]]
name = "primary"
address = "example.com:443"
public_key = "<base64 公钥>"
# cipher = "auto"           # 覆盖全局 cipher
# protocol = "tcp"          # tcp (默认) 或 quic

[client]
listen = "127.0.0.1:1080"
dns = "tls://8.8.8.8:853"
mode = "smart"
cipher = "auto"

[failover]
health_check_interval = 30
health_check_timeout = 5
failover_threshold = 3
graceful_migration = true

[[rules]]
type = "domain-suffix"
value = "google.com"
action = "proxy"

[[rules]]
type = "ip-cidr"
value = "192.168.0.0/16"
action = "direct"

[rules]
final_action = "proxy"
```

### 7.2 URI 配置

```
phantom://<base64公钥>@<host>:<port>[?cipher=&proto=][#name]
```

```bash
phantom client --server "phantom://KEY@host:443?cipher=auto&proto=tcp#primary"
# URI 内已包含服务端公钥 + 端点，--config 可选；如需多 server 池，写 client.toml
phantom client --config client.toml --server "phantom://KEY@host:443"
```

### 7.3 服务端配置

#### 7.3.1 自举模式（默认）

`phantom server` 无参数时进入自举模式，CWD 下生成：

| 文件 | 权限 | 含义 |
|------|------|------|
| `./server.key` | 600 | 第 1 行 base64 公钥，第 2 行 base64 私钥 |
| `./server.toml` | 644 | bind / cipher / protocol + 顶部 URI 注释（`#   phantom://...`）+ `[[allowed_clients]]` 白名单 |

```bash
# 默认 0.0.0.0:443，端口被占 +1 递增（最多 10 次）
phantom server

# 覆盖参数
phantom server --port 8443 --public-host vpn.example.com --cipher ascon-128 --proto quic

# 交互式向导
phantom server -i
```

#### 7.3.2 TOML 加载模式（兼容传统部署）

保留 TOML 配置供 systemd 单元、CI 脚本、复杂多实例场景使用：

```toml
bind = "0.0.0.0:443"
private_key = "/var/lib/phantom/server.key"   # load 模式必填
cipher = "auto"

# 可选：内联白名单（auto 模式与 load 模式通用）
# [[allowed_clients]]
# public_key = "<base64 客户端公钥>"
# name = "alice-laptop"

[quic]
max_streams = 100
keep_alive_interval = 45
congestion = "cubic"

[performance]
io_uring = false
workers = 0
```

```bash
phantom server -c /etc/phantom/server.toml
```

---

## 8. 基准测试

```bash
cargo bench -p phantom-bench
```

| 基准 | 内容 | 指标 |
|------|------|------|
| aead_throughput | 4 种密码 × 4 种负载 | GB/s |
| handshake | Noise IK 握手往返 | μs |
| key_derivation | HKDF-SHA256 | μs |
| frame_codec | Frame 编解码 | GB/s |
| pipeline | 完整数据路径 | GB/s |

---

## 9. 安全模型

| 属性 | 机制 |
|------|------|
| 双向认证 | Noise IK：客户端预知服务端静态公钥（来自 URI），服务端校验客户端公钥白名单（空白名单=开放） |
| 抗主动探测 | `psk1` 把 PSK 绑定在**第一条**握手消息；无 PSK 无法构造可解密的首条消息，服务端静默断开且零回应 |
| 前向保密 | 每次握手生成新的临时密钥；PSK 与静态私钥同时泄露仍不能解已录制的历史流量 |
| 密钥分离 | C→S 和 S→C 方向使用独立的 HKDF 派生密钥 |
| 认证加密 | 所有 AEAD 方案提供机密性 + 完整性 + 认证 |
| 黑洞行为 | 服务端所有失败均静默丢弃，不泄露信息 |
| 抗量子 | 对称加密（AES/ASCON）与对称 PSK 本身抗量子；未来可升级 X25519 → ML-KEM |

**握手 pattern**：`Noise_IKpsk1_25519_ChaChaPoly_SHA256`

`psk1` 而非 `psk2` 是刻意选择：`psk2` 把 PSK 混入第二条消息，服务端会正常回应一个
「公钥正确但无 PSK」的探测，只有客户端侧能发现不匹配 —— 这泄露了「此处有服务」的信号。
回归测试见 `core/src/crypto/noise.rs::handshake_fails_when_psk_differs` 与
`pattern_binds_the_psk_to_the_first_message`，端到端见
`tests/tests/config_effect.rs::wrong_psk_is_rejected`。

**凭据边界**：`phantom://` URI 同时携带服务端公钥与 PSK，是一份完整凭据，需通过安全渠道
传递；服务端侧三者均存于 `server.key`（三行，权限 600）。
