# Phantom 幽灵 — 高性能加密代理隧道

Phantom 是一个基于 Rust 的 SOCKS5 加密代理隧道，使用 Noise IK 协议认证，支持自适应加密套件选择，在 Apple Silicon 上可达 5+ GB/s 吞吐量。

## 特性

- **自适应加密**: 自动检测 CPU 能力，选择最优加密算法
- **四层密码套件**: AES-256-GCM → AES-128-GCM → ASCON-128 → ChaCha20-Poly1305
- **零额外往返**: 密码协商嵌入 Noise IK 握手消息
- **无锁读写**: SessionReader/SessionWriter 各自独立状态，完全并发
- **客户端白名单**: 服务端基于公钥认证，静默丢弃未知连接
- **QUIC 支持**: 可选 QUIC 传输层，内置 BBR/CUBIC 拥塞控制
- **Failover**: 多服务器自动切换，支持优雅迁移
- **零拷贝**: 基于 Bytes 的零拷贝数据路径，栈分配消除每帧堆分配
- **Rust 2024**: 使用最新 Rust 特性

## 快速开始

### 构建

```bash
cargo build --release
```

构建产物位于 `target/release/phantom`，这是一个包含 `client`、`server`、`keygen` 三个子命令的统一二进制。

### 一分钟上手

```bash
# 1. 生成密钥对
phantom keygen -o ./keys
# 输出示例:
#   Public key (add to client.toml [[servers]].public_key):
#   abc123XYZ...base64...
#   Public key file: ./keys/phantom.pub
#   Secret key file: ./keys/server_private (chmod 600)

# 2. 启动服务端
phantom server -c config/server.toml

# 3. 在客户端机器上启动
phantom client -c config/client.toml
```

然后配置浏览器或系统 SOCKS5 代理为 `127.0.0.1:1080`。

---

## 部署手册

### 1. 环境准备

**系统要求**:
- Rust ≥ 1.75（Rust 2024 edition）
- Linux / macOS（服务端推荐 Linux）
- 端口开放：默认 443/TCP

**安装 Rust**（如未安装）:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

**从源码构建**:

```bash
git clone <repo-url> phantom
cd phantom
cargo build --release
```

> 国内用户：项目已配置 USTC 镜像源（`.cargo/config.toml`），无需额外设置。

**交叉编译**（可选）:

```bash
# 目标: ARM64 Linux (如 AWS Graviton / 树莓派)
rustup target add aarch64-unknown-linux-gnu
cargo build --release --target aarch64-unknown-linux-gnu

# 目标: x86_64 musl (静态链接，无 glibc 依赖)
rustup target add x86_64-unknown-linux-musl
cargo build --release --target x86_64-unknown-linux-musl
```

### 2. 服务端部署

#### 2.1 生成密钥对

```bash
phantom keygen -o /etc/phantom/keys
```

生成文件:

| 文件 | 内容 | 权限 |
|------|------|------|
| `/etc/phantom/keys/server_private` | 第1行: 公钥(Base64) <br> 第2行: 私钥(Base64) | 600 |
| `/etc/phantom/keys/phantom.pub` | 公钥(Base64) 单行 | 644 |

**记下输出的公钥字符串**，客户端配置需要使用。

#### 2.2 配置客户端白名单

每个客户端也需要生成自己的密钥对。将每个客户端的**公钥**追加到服务端白名单文件:

```bash
# /etc/phantom/keys/clients_allowed
# 每行一个客户端公钥，# 开头为注释
abc123XYZ...客户端1公钥base64...
def456UVW...客户端2公钥base64...
```

> **重要**: 白名单为空时，所有 Noise IK 握手都会失败。服务端启动时会打印已加载的密钥数量。

#### 2.3 编写服务端配置

创建 `/etc/phantom/server.toml`:

```toml
bind = "0.0.0.0:443"
private_key = "/etc/phantom/keys/server_private"
clients = "/etc/phantom/keys/clients_allowed"
cipher = "auto"

[quic]
max_streams = 100
keep_alive_interval = 45
congestion = "cubic"    # cubic / bbr / new-reno

[tls]
# 如果需要 TLS 伪装，取消注释:
# cert = "/etc/phantom/acme/cert.pem"
# key = "/etc/phantom/acme/key.pem"
disguise = false

[performance]
io_uring = false     # Linux 5.1+ 可开启
zero_copy = false    # 实验性
workers = 0          # 0 = 自动 (CPU 核心数)
```

**配置项说明**:

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `bind` | `0.0.0.0:443` | 监听地址和端口 |
| `private_key` | — | 服务端密钥文件路径（必需） |
| `clients` | — | 客户端公钥白名单文件路径（必需） |
| `cipher` | `auto` | 加密套件选择，见下方说明 |
| `quic.max_streams` | 100 | 最大并发流数 |
| `quic.keep_alive_interval` | 45 | QUIC 保活间隔（秒） |
| `quic.congestion` | `cubic` | 拥塞控制算法: `cubic` / `bbr` / `new-reno` |
| `tls.disguise` | false | 启用 TLS 伪装，需配合 cert/key |
| `performance.io_uring` | false | 启用 io_uring（Linux 5.1+） |
| `performance.zero_copy` | false | 零拷贝传输（实验性） |
| `performance.workers` | 0 | 工作线程数，0 = CPU 核心数 |

#### 2.4 防火墙配置

```bash
# iptables
iptables -A INPUT -p tcp --dport 443 -j ACCEPT

# ufw
ufw allow 443/tcp

# firewalld
firewall-cmd --permanent --add-port=443/tcp
firewall-cmd --reload
```

> 如果启用了 QUIC 传输，还需开放 443/UDP。

#### 2.5 使用 systemd 管理（推荐）

项目提供了 systemd 服务文件，安装步骤:

```bash
# 方式一: 使用安装脚本（一键部署）
sudo bash deploy/install.sh

# 方式二: 手动安装
sudo cp target/release/phantom /usr/local/bin/
sudo mkdir -p /etc/phantom/keys
sudo cp config/server.toml /etc/phantom/
sudo cp deploy/phantom.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable phantom
```

管理命令:

```bash
sudo systemctl start phantom     # 启动
sudo systemctl stop phantom      # 停止
sudo systemctl restart phantom   # 重启
sudo systemctl status phantom    # 查看状态
journalctl -u phantom -f         # 查看实时日志
```

systemd 服务文件内置安全加固:

```ini
NoNewPrivileges=true
ProtectSystem=strict
ProtectHome=true
PrivateTmp=true
ReadWritePaths=/etc/phantom
LimitNOFILE=65535
```

#### 2.6 验证服务端运行

```bash
# 检查端口监听
ss -tlnp | grep 443

# 查看启动日志
journalctl -u phantom --no-pager -n 20
# 期望输出:
#   Phantom server listening on 0.0.0.0:443
#   Loaded N allowed client keys
```

### 3. 客户端配置

#### 3.1 生成客户端密钥

在客户端机器上运行:

```bash
phantom keygen -o ~/.phantom/keys
```

将输出的**公钥字符串**添加到服务端的 `clients_allowed` 文件中。

#### 3.2 编写客户端配置

创建 `client.toml`:

```toml
# 服务器列表（支持多个，自动 failover）
[[servers]]
name = "primary"
address = "your-server.com:443"     # 服务端地址:端口
public_key = "服务端公钥Base64"       # 服务端 phantom.pub 的内容

[[servers]]
name = "backup"
address = "backup-server.com:443"
public_key = "备用服务端公钥Base64"

[client]
listen = "127.0.0.1:1080"        # 本地 SOCKS5 监听地址
dns = "tls://8.8.8.8:853"        # DNS-over-TLS 服务器
mode = "smart"                    # 代理模式
cipher = "auto"                   # 加密套件

[failover]
health_check_interval = 30       # 健康检查间隔（秒）
health_check_timeout = 5         # 健康检查超时（秒）
failover_threshold = 3           # 连续失败 N 次后切换服务器
graceful_migration = true         # 切换时优雅迁移现有连接
```

**配置项说明**:

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `servers[].name` | — | 服务器名称标识 |
| `servers[].address` | — | 服务器地址和端口 |
| `servers[].public_key` | — | 服务端公钥（Base64） |
| `client.listen` | `127.0.0.1:1080` | 本地 SOCKS5 代理监听地址 |
| `client.dns` | `tls://8.8.8.8:853` | 远程 DNS 解析服务器 |
| `client.mode` | `smart` | 代理模式，见下方说明 |
| `client.cipher` | `auto` | 加密套件 |
| `failover.health_check_interval` | 30 | 健康检查间隔（秒） |
| `failover.health_check_timeout` | 5 | 健康检查超时（秒） |
| `failover.failover_threshold` | 3 | 连续失败多少次后切换 |
| `failover.graceful_migration` | true | 切换服务器时是否迁移已有连接 |

**代理模式**:

| 模式 | 行为 |
|------|------|
| `smart` | 自动判断是否走代理（推荐） |
| `proxy` | 所有流量都走代理 |
| `direct` | 所有流量直连（调试用） |
| `auto` | 同 smart |

#### 3.3 启动客户端

```bash
phantom client -c client.toml
```

启动后客户端在 `127.0.0.1:1080` 提供 SOCKS5 代理服务。

#### 3.4 使用代理

**浏览器** (以 Firefox 为例):
- 设置 → 网络设置 → 手动代理 → SOCKS5: `127.0.0.1:1080`

**命令行**:

```bash
# curl
curl --socks5 127.0.0.1:1080 https://example.com

# git
git config --global http.proxy socks5://127.0.0.1:1080

# 环境变量（支持大多数 CLI 工具）
export ALL_PROXY=socks5://127.0.0.1:1080
```

**macOS / iOS**: 使用 Surge / Shadowrocket 等工具配置 SOCKS5 代理指向 `127.0.0.1:1080`。

### 4. 加密套件选择

| 算法 | 条件 | 吞吐量 | 定位 |
|------|------|--------|------|
| AES-256-GCM | AES-NI / ARM CE | 5-12 GB/s | 主力：现代 CPU |
| AES-128-GCM | AES CE | 3-8 GB/s | 平衡：功耗敏感 |
| ASCON-128 | 无硬加速 | ~1-2 GB/s | NIST SP 800-232 轻量级新标准 |
| ChaCha20-Poly1305 | 任意 | ~1-2 GB/s | 兼容备选 |

`cipher = "auto"` 时自动检测:

- x86_64 + AES-NI → AES-256-GCM
- aarch64 + ARM CE → AES-256-GCM
- 其他 → ASCON-128

也可手动指定:

```toml
# server.toml 或 client.toml
cipher = "aes-256-gcm"    # 强制 AES-256-GCM
cipher = "ascon-128"      # IoT / 嵌入式场景
cipher = "cha-cha20-poly1305"  # 无 AES 硬加速的旧设备
```

### 5. 拥塞控制

QUIC 传输支持三种拥塞控制算法，通过 `server.toml` 的 `[quic]` 段配置:

| 算法 | 适用场景 |
|------|----------|
| `cubic` | 通用场景（默认），兼容性好 |
| `bbr` | 高带宽长肥网络（跨洲链路），带宽探测更激进 |
| `new-reno` | 简单场景，低开销 |

### 6. TLS 伪装

Phantom 可选伪装为普通 HTTPS 流量，需配置合法 TLS 证书:

```toml
[tls]
cert = "/etc/phantom/acme/cert.pem"
key = "/etc/phantom/acme/key.pem"
disguise = true
```

使用 Let's Encrypt 获取免费证书:

```bash
# 安装 certbot
apt install certbot      # Debian/Ubuntu
yum install certbot      # CentOS/RHEL

# 获取证书
certbot certonly --standalone -d your-domain.com

# 证书路径
cert = "/etc/letsencrypt/live/your-domain.com/fullchain.pem"
key = "/etc/letsencrypt/live/your-domain.com/privkey.pem"
```

### 7. 多服务器 Failover

客户端支持配置多个服务器，自动健康检查和故障切换:

```toml
[[servers]]
name = "primary"
address = "primary.example.com:443"
public_key = "..."

[[servers]]
name = "backup"
address = "backup.example.com:443"
public_key = "..."

[failover]
health_check_interval = 30    # 每 30 秒检查一次
health_check_timeout = 5      # 5 秒超时
failover_threshold = 3        # 连续 3 次失败后切换
graceful_migration = true      # 切换时迁移已有连接
```

### 8. 性能调优

#### 系统参数

```bash
# 增大文件描述符限制
ulimit -n 65535

# Linux 内核参数优化
sysctl -w net.core.somaxconn=65535
sysctl -w net.ipv4.tcp_max_syn_backlog=65535
sysctl -w net.ipv4.tcp_tw_reuse=1
```

#### 应用配置

```toml
[performance]
io_uring = true      # Linux 5.1+ 启用 io_uring 减少 syscall
zero_copy = true     # 实验性零拷贝传输
workers = 0          # 0 = CPU 核心数
```

### 9. 故障排查

| 问题 | 可能原因 | 解决方法 |
|------|----------|----------|
| 连接超时 | 防火墙/端口未开 | 检查 `ss -tlnp \| grep 443`，开放对应端口 |
| 握手失败 | 客户端公钥不在白名单 | 将客户端公钥追加到服务端 `clients_allowed` |
| 握手失败 | 服务端公钥不匹配 | 确认 `client.toml` 的 `public_key` 与服务端 `phantom.pub` 一致 |
| 连接后无数据 | DNS 解析失败 | 检查 `client.dns` 配置，确保可达 |
| 性能不佳 | 未使用 AES 硬加速 | 检查 `cipher = "auto"` 是否选择了 AES，或手动指定 |
| 服务端崩溃重启 | systemd 自动拉起 | 查看 `journalctl -u phantom` 定位原因 |

**调试模式**:

```bash
# 启用详细日志
RUST_LOG=debug phantom client -c client.toml
RUST_LOG=debug phantom server -c server.toml
```

### 10. 安全注意事项

- **私钥保护**: `server_private` 文件权限必须为 `600`，避免泄露
- **白名单最小化**: 仅添加可信客户端的公钥
- **前向保密**: 每次会话派生独立密钥，密钥泄露不影响历史会话
- **黑洞行为**: 服务端静默丢弃所有未认证连接，不暴露任何错误信息
- **systemd 加固**: 服务以最小权限运行（`NoNewPrivileges`、`ProtectSystem`）
- **未来升级**: 对称加密抗量子计算；X25519 → ML-KEM 后量子密钥交换已在路线图中

---

## 开发者参考

### 基准测试

```bash
# 运行全部
cargo bench -p phantom-bench

# 单项测试
cargo bench -p phantom-bench --bench aead_throughput    # AEAD 加解密吞吐量
cargo bench -p phantom-bench --bench handshake          # 握手延迟
cargo bench -p phantom-bench --bench key_derivation     # 密钥派生
cargo bench -p phantom-bench --bench frame_codec        # 帧编解码
cargo bench -p phantom-bench --bench pipeline           # 完整数据路径
```

### 端到端测试

```bash
# 正确性测试
cargo test -p phantom-e2e --test correctness --release

# 弱网测试
cargo test -p phantom-e2e --test weak_network --release

# 吞吐量测试
cargo test -p phantom-e2e --test throughput --release
```

### 项目结构

```
phantom-core/       共享类型、配置、错误、常量
phantom-crypto/     密码套件、AEAD 状态、Noise 握手、密钥管理
phantom-protocol/   线路帧格式、地址编码、编解码器
phantom-transport/  TCP/QUIC 传输抽象
phantom-server/     服务端连接处理
phantom-client/     SOCKS5 代理、隧道建立
phantom-cli/        命令行入口
phantom-bench/      性能基准测试
phantom-e2e/        端到端集成测试
```

### 协议设计

1. **Noise IK 握手** (ChaCha20-Poly1305): 认证 + 密钥交换 + 密码协商
2. **HKDF 密钥派生**: 从 Noise split keys 派生双向会话密钥
3. **自定义 AEAD 传输**: SessionReader/SessionWriter 使用协商的密码套件

数据路径无 `Arc<Mutex>` 共享，读写完全并发。

## License

MIT
