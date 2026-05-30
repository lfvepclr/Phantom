# Phantom Tunnel 项目计划书

## 1. 项目概述

Phantom（幽灵）是一个高性能 SOCKS5 加密代理隧道，基于 Rust 构建，使用 Noise IK 协议进行认证和密钥交换，支持自适应加密套件选择。

**核心目标**: 配置简单，性能极高，容易维护，代码模块化，现代化。

---

## 2. 架构设计

### 2.1 系统架构

```
┌──────────────┐     SOCKS5      ┌──────────────────┐     TCP/QUIC     ┌──────────────────┐     TCP      ┌──────────┐
│  应用程序     │ ──────────────▶ │  Phantom Client  │ ────────────────▶ │  Phantom Server  │ ───────────▶ │  目标服务器│
│  (浏览器等)   │                 │  (SOCKS5 代理)    │                  │  (隧道服务端)     │              │          │
└──────────────┘                 └──────────────────┘                  └──────────────────┘              └──────────┘
                                        │                                      │
                                        │       Noise IK 握手 + 加密数据传输    │
                                        └──────────────────────────────────────┘
```

### 2.2 数据流

```
应用数据 → SOCKS5 解析 → Frame 编码 → AEAD 加密 → 长度前缀 → TCP/QUIC 传输
                                                                        ↓
目标数据 ← TCP 写入 ← Frame 解码 ← AEAD 解密 ← 长度前缀 ← TCP/QUIC 接收
```

### 2.3 Crate 结构

| Crate | 职责 |
|-------|------|
| `phantom-core` | 共享类型、配置、错误、常量 |
| `phantom-crypto` | 密码套件、AEAD 状态、Noise 握手、密钥管理、会话管理 |
| `phantom-protocol` | 线路帧格式、地址编码、编解码器 |
| `phantom-transport` | TCP/QUIC 传输抽象 |
| `phantom-server` | 服务端连接处理、双向中继 |
| `phantom-client` | SOCKS5 代理、隧道建立、故障转移 |
| `phantom-cli` | 命令行入口 |
| `phantom-bench` | 性能基准测试 |

---

## 3. 加密协议设计

### 3.1 三层自适应密码套件

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

### 3.2 自动检测逻辑

```rust
pub fn auto_detect() -> CipherSuite {
    // x86_64 + AES-NI → Aes256Gcm
    // aarch64 + ARM CE → Aes256Gcm
    // 其他 → Ascon128 (无硬加速时比软件 AES 快 5-10x)
}
```

### 3.3 密码协商协议

协商嵌入 Noise IK 握手消息载荷，零额外往返：

```
客户端 → 服务端:  Noise IK 第一条消息 + CipherOffer
                  [version:1][count:1][cipher_ids:count]

服务端 → 客户端:  Noise IK 响应消息 + CipherAccept
                  [version:1][cipher_id:1]
```

选择逻辑：按客户端优先级顺序，选择第一个服务端也支持的密码套件。

### 3.4 密钥派生管线

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

### 3.5 会话加密层

握手完成后，不再使用 snow 的 TransportState，而是切换到自定义 `SessionReader`/`SessionWriter`：

- 每个方向拥有独立的 `AeadState`（密钥 + nonce 计数器）
- 无 `Arc<Mutex>` 共享，读写完全并发
- 线路格式不变：`[2字节长度BE][密文+16字节认证标签]`
- nonce 构造：`[4字节前缀][0填充][8字节计数器]`

---

## 4. 各平台性能预期

| 平台 | 推荐密码 | 预期吞吐量 |
|------|---------|-----------|
| Ubuntu x86_64 (AES-NI) | AES-256-GCM | 5-10 GB/s |
| macOS Apple Silicon (M1-M4) | AES-256-GCM | 5-8 GB/s |
| Android 旗舰 (AES CE) | AES-256-GCM | 3-6 GB/s |
| Android 旧设备 (无 AES CE) | ASCON-128 | ~1-2 GB/s |
| 华硕路由器 (新 ARM) | AES-128-GCM | 2-4 GB/s |
| 华硕路由器 (旧 MIPS) | ASCON-128 | ~0.5-1 GB/s |

---

## 5. 配置参考

### 5.1 客户端配置 (client.toml)

```toml
[[servers]]
name = "primary"
address = "example.com:443"
public_key = "<base64 公钥>"

[client]
listen = "127.0.0.1:1080"
dns = "tls://8.8.8.8:853"
mode = "smart"
cipher = "auto"          # auto / aes-256-gcm / aes-128-gcm / ascon-128 / chacha20-poly1305

[failover]
health_check_interval = 30
health_check_timeout = 5
failover_threshold = 3
graceful_migration = true
```

### 5.2 服务端配置 (server.toml)

```toml
bind = "0.0.0.0:443"
private_key = "/etc/phantom/keys/server_private"
clients = "/etc/phantom/keys/clients_allowed"
cipher = "auto"          # auto / aes-256-gcm / aes-128-gcm / ascon-128 / chacha20-poly1305

[quic]
max_streams = 100
keep_alive_interval = 45

[performance]
io_uring = false
zero_copy = false
workers = 0
```

---

## 6. 基准测试

```bash
# 运行所有基准测试
cargo bench -p phantom-bench

# 运行单项
cargo bench -p phantom-bench --bench aead_throughput
cargo bench -p phantom-bench --bench handshake
cargo bench -p phantom-bench --bench key_derivation
cargo bench -p phantom-bench --bench frame_codec
cargo bench -p phantom-bench --bench pipeline
```

测试项目:

| 基准 | 内容 | 指标 |
|------|------|------|
| `aead_throughput` | 4 种密码 × 4 种负载 (1K/4K/16K/64K) | GB/s |
| `handshake` | Noise IK 完整握手往返 | μs |
| `key_derivation` | HKDF-SHA256 密钥派生 | μs |
| `frame_codec` | Frame 编码/解码 | GB/s |
| `pipeline` | 完整数据路径 | GB/s |

---

## 7. v1 → v2 迁移说明

| 项目 | v1 | v2 |
|------|----|----|
| 协议版本 | 1 | 2 |
| 加密算法 | ChaCha20-Poly1305 固定 | AES-256-GCM / AES-128-GCM / ASCON-128 / ChaCha20 自适应 |
| 密码协商 | 无 | 嵌入 Noise IK 握手载荷 |
| 数据路径 | snow TransportState | 自定义 SessionReader/SessionWriter |
| 读写并发 | Arc\<Mutex\> 共享状态 | 独立 AeadState，无锁 |
| 密钥生成 | snow::Builder | x25519-dalek 直接 |
| 非ce构造 | snow 内部管理 | prefix(4) + counter(8) |

**不兼容**: v1 客户端无法与 v2 服务端通信。个人专用产品，可接受。

---

## 8. 安全模型

- **认证**: Noise IK 模式，客户端和服务端互相验证静态公钥
- **前向保密**: 每次握手生成新的临时密钥，会话密钥独立
- **密钥分离**: C→S 和 S→C 方向使用独立的 HKDF 派生密钥
- **认证加密**: 所有 AEAD 方案提供机密性 + 完整性 + 认证
- **黑洞行为**: 服务端所有失败均静默丢弃，不泄露信息
- **后量子**: 对称加密（AES/ASCON）本身抗量子；未来可升级 X25519 → ML-KEM
