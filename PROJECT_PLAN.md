# Phantom Tunnel 项目执行计划

## 1. 项目概述

Phantom（幽灵）是一个高性能加密代理隧道，基于 Rust 构建，使用 Noise IK 协议进行认证和密钥交换，支持自适应加密套件选择。支持 SOCKS5 代理和 TUN 透明代理两种模式，提供 macOS / Android / CLI 多平台客户端。

**核心目标**: 配置简单，性能极高，容易维护，代码模块化，现代化。

---

## 2. 已完成功能清单

### 2.1 核心协议层 ✅

| 功能 | 状态 | 说明 |
|------|------|------|
| Noise IK 握手 | ✅ | 双向公钥认证，零额外 RTT |
| 密码协商 | ✅ | CipherOffer/CipherAccept 嵌入握手载荷 |
| 四层密码套件 | ✅ | AES-256-GCM / AES-128-GCM / ASCON-128 / ChaCha20-Poly1305 |
| 自适应选择 | ✅ | auto 检测 CPU 能力 |
| 密钥派生 | ✅ | HKDF-SHA256 双向独立会话密钥 |
| 帧协议 | ✅ | 8 字节头，SYN/FIN/RST/ACK/DATA/PING/PONG/UDP |
| QUIC 多路复用 | ✅ | 首流 Noise 握手，后续流 HKDF 子密钥 |

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
| TUN 透明代理 | ✅ | macOS utun7 / Android VpnService fd |
| 代理模式 | ✅ | Global(Auto) / Smart(规则) / Direct |
| 规则引擎 | ✅ | 7 种规则：domain-full/suffix/keyword/regex / ip-cidr / port / geoip |
| DNS 劫持 | ✅ | UDP:53 拦截 → 上游 DNS → IP→域名缓存 |
| UDP Direct relay | ✅ | 本地 UdpSocket 直连 |
| UDP Proxy relay | ✅ | Noise 隧道 + UDP SYN 帧转发 |
| Failover | ✅ | 多服务器自动切换，TCP 探测健康检查 |
| 系统代理自启 | ✅ | macOS networksetup 自动设置/恢复 SOCKS5 |
| URI 单串配置 | ✅ | phantom://base64key@host:port?cipher=&proto=#name |
| 配置热重载 | ✅ | mtime 轮询 5s，更新 proxy_mode + rule_engine |
| 流量统计 | ✅ | AtomicU64 计数器 + Prometheus :9150/metrics |

### 2.4 平台客户端 ✅

| 平台 | 状态 | 说明 |
|------|------|------|
| CLI | ✅ | phantom client / server；server 支持 auto / interactive / load 三种模式，--server URI 支持 |
| macOS | ✅ | SwiftUI 菜单栏，Global/Auto/Direct 模式切换，系统代理自启 |
| Android | ✅ | Kotlin VpnService + Jetpack Compose，TUN fd 传入 |

---

## 3. 测试覆盖

### 3.1 测试分层

| 层级 | 测试数 | 文件 | 覆盖范围 |
|------|--------|------|---------|
| L0 单元测试 | ~80 | 各 crate #[cfg(test)] | 帧协议边界、URI 构建/解析、规则引擎、DNS、stats、handler、bootstrap (密钥/白名单/端口探测) |
| L1 配置生效 | 9 | config_effect, cipher_matrix | 白名单、密码协商、echo 模式 |
| L1 模块交互 | 15 | rule_engine, dns_hijack, stats_metrics | DNS→规则、规则→路由、stats→Prometheus |
| L1 全链路 | 7 | full_link_tcp, full_link_udp, http_tunnel | TCP echo/大数据/并发、UDP relay |
| L1 真实场景 | 4 | real_world | Mock 百度 + 真百度 (2 ignored) |
| L1 性能 | 17 | performance, throughput | 吞吐量/延迟/并发 (10 ignored) |
| L2 系统 | 5 | cli_system | CLI 自举（auto 生成 key/URI）、端口递增 fallback、keygen 子命令已删除、version、复用已有 key |

### 3.2 关键测试场景

- **密码协商矩阵**: 每种 cipher 全链路 echo，验证协商 + 数据完整性
- **白名单配置生效**: 空=开放、非空=拒绝未知密钥、匹配=接受
- **规则引擎交互**: domain→Proxy、ip-cidr→Direct、域名优先级 > IP
- **DNS→规则**: DNS 查询缓存 IP→域名，后续 TCP SYN 查到域名走规则
- **UDP 全链路**: UDP SYN 帧 → 服务端 UdpSocket → 响应回传
- **Mock 百度**: 通过隧道访问 mock 百度页面，验证 HTML 内容
- **热重载**: 运行中修改配置文件，5s 内规则/模式生效

---

## 4. 待实现功能

| 功能 | 优先级 | 说明 |
|------|--------|------|
| SOCKS5 UDP ASSOCIATE | Deferred | TUN UDP proxy 已覆盖主场景 |
| TLS 伪装 | Deferred | tls.disguise stub 存在，需 TLS 层重构 |
| DNS/服务器热重载 | P2 | 当前仅 rules + mode 支持热更新 |
| 多用户/限速 | P3 | 需协议扩展 |
| ACME 自动证书 | P3 | 需 CA 集成 |
| 后量子密钥交换 | P3 | X25519 → ML-KEM 路线图 |

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

[tls]
disguise = false

[performance]
io_uring = false
zero_copy = false
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

- **认证**: Noise IK 模式，客户端和服务端互相验证静态公钥（空白名单=开放）
- **前向保密**: 每次握手生成新的临时密钥，会话密钥独立
- **密钥分离**: C→S 和 S→C 方向使用独立的 HKDF 派生密钥
- **认证加密**: 所有 AEAD 方案提供机密性 + 完整性 + 认证
- **黑洞行为**: 服务端所有失败均静默丢弃，不泄露信息
- **后量子**: 对称加密（AES/ASCON）本身抗量子；未来可升级 X25519 → ML-KEM
