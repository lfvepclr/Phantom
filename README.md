# Phantom 幽灵 — 高性能加密代理隧道

Phantom 是一个基于 Rust 的加密代理隧道，使用 Noise IK 协议认证，支持自适应加密套件选择，在 Apple Silicon 上可达 5+ GB/s 吞吐量。支持 SOCKS5 代理和 TUN 透明代理两种模式，提供 macOS / Android / HarmonyOS NEXT / CLI 多平台客户端。

## 特性

- **自适应加密**: 自动检测 CPU 能力，选择最优加密算法（AES-256-GCM / AES-128-GCM / ASCON-128 / ChaCha20-Poly1305）
- **零额外往返**: 密码协商嵌入 Noise IK 握手消息
- **透明代理**: macOS / Android TUN 模式，无需手动配置应用代理
- **智能分流**: 域名/IP/端口/GeoIP 规则引擎，全局/自动/直连三种模式
- **DNS 劫持**: TUN 模式自动拦截 DNS 查询，防止 DNS 泄露
- **UDP Relay**: TUN 模式 UDP 流量通过帧协议隧道转发
- **系统代理自启**: macOS 启动后自动设置系统 SOCKS5 代理
- **单串配置**: `phantom://` URI 格式，一行配置包含服务器信息
- **配置热重载**: 运行中修改配置文件，规则和模式自动更新
- **流量统计**: Prometheus `/metrics` 端点，实时监控流量
- **Failover**: 多服务器自动切换，支持优雅迁移
- **QUIC 支持**: 可选 QUIC 传输层，内置 BBR/CUBIC 拥塞控制
- **零拷贝**: 基于 `Bytes` 的零拷贝数据路径

## 快速开始

### 构建

```bash
# 统一构建系统（推荐）
cargo xtask build          # 构建所有可用目标
cargo xtask build server   # 仅构建服务端
cargo xtask build cli      # 仅构建 CLI 客户端
cargo xtask build mac      # 仅构建 macOS 客户端
cargo xtask build android  # 仅构建 Android 客户端
cargo xtask build harmony  # 仅构建 HarmonyOS 客户端

# 检查依赖状态
cargo xtask check-deps

# 重新生成所有平台图标
cargo xtask icons

# 清理所有构建产物
cargo xtask clean

# 传统方式
cargo build --release
```

### 一分钟上手

`phantom server` 现在采用**零配置自举**：第一次运行自动生成密钥、自适应配置、探测端口、写 `server.toml`（含 URI 快速链接注释），并立即启动监听。

```bash
# 1. 一行启动服务端（CWD 下自动生成 ./server.key 与 ./server.toml）
cd /var/lib/phantom            # 或任何目录
phantom server                  # 默认 0.0.0.0:443；端口被占时自动 +1（最多 10 次）

# 1b. 启动客户端（URI 快捷链接，推荐；URI 在 server.toml 顶部的注释行）
URI=$(grep '^#   phantom://' ./server.toml | sed 's/^#   //')
phantom client --server "$URI"

# 1c. 或继续使用传统的 TOML 加载模式（systemd / CI / 高级场景；模板见 config/server.toml）
phantom server -c /etc/phantom/server.toml
```

然后配置浏览器或系统 SOCKS5 代理为 `127.0.0.1:1080`。

### macOS 客户端构建

SwiftUI 菜单栏客户端位于 `client/mac/`，采用 **Swift Package Manager + 自写 bundler** 模式，全程无需 Xcode 工程：

```bash
# 方式一：统一构建（推荐）
cargo xtask build mac

# 方式二：独立脚本
scripts/build-mac.sh              # 默认 Apple Silicon release
```

产物在 `client/mac/.build/`：
- `.build/Phantom.app` — macOS 应用包
- `.build/dist/Phantom.dmg` — DMG 安装镜像

TUN 需要 root，请用 `sudo open client/mac/.build/Phantom.app` 启动。完整说明见 `client/mac/README.md`。

macOS 原生客户端启动后，系统代理自动生效，无需手动配置。

---

## 部署手册

### 1. 环境准备

**系统要求**:
- Rust ≥ 1.85（Rust 2024 edition；如 nightly 缺失部分特性需要切换）
- Linux / macOS（服务端推荐 Linux）
- 端口开放：默认 443/TCP（auto 模式被占时自动 +1 探测，最多 10 次）

**安装 Rust**:

```bash
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source $HOME/.cargo/env
```

**从源码构建**:

```bash
git clone <repo-url> phantom
cd phantom

# 检查依赖（自动安装可安装的依赖）
cargo xtask check-deps

# 构建
cargo xtask build server   # 服务端
cargo xtask build all      # 所有可用目标
```

> 国内用户：项目已配置 USTC 镜像源（`.cargo/config.toml`），无需额外设置。

### 2. 服务端部署

#### 2.1 自举模式（零配置）

首次运行 `phantom server`（无 `-c`）即进入**自举模式**：

1. 读取 CWD 下的 `./server.key`；不存在则生成 X25519 密钥对并写入（权限 600）
2. 读取 CWD 下的 `./server.toml` 内联 `[[allowed_clients]]` 白名单；空则开放模式（info 级日志提示）
3. 默认从 0.0.0.0:443 开始探测，端口占用时自动 +1（最多 10 次）
4. 自动探测本机公网 IP（UDP socket 探测），写入 `server.toml` URI 注释的 host 段
5. 拼接 `phantom://...` URI，以 `#   phantom://...` 注释形式写入 `./server.toml` 顶部
6. 打印启动摘要（监听地址、URI、白名单条目数），然后启动服务

| 文件 | 内容 | 权限 |
|------|------|------|
| `./server.key` | 第 1 行 base64 公钥，第 2 行 base64 私钥 | 600 |
| `./server.toml` | bind / cipher / protocol + 顶部 URI 注释 + `[[allowed_clients]]` 白名单 | 644 |

**典型自举文件**（`./server.toml`，前若干行）：

```toml
# Phantom server config (auto-bootstrap generated)
# Quick link URI (distribute to clients):
#   phantom://dGVzdGtleQ==@server.example.com:443?cipher=auto&proto=tcp#default
bind = "0.0.0.0:443"
cipher = "auto"
protocol = "tcp"
```

**获取与下发 URI**：

```bash
# 拉取 URI（剥掉行首 `#   ` 注释前缀）
URI=$(grep '^#   phantom://' /var/lib/phantom/server.toml | sed 's/^#   //')

# 下发到客户端
phantom client --server "$URI"
```

**手动指定参数**（覆盖默认值）：

```bash
phantom server --port 8443 --public-host vpn.example.com --cipher ascon-128 --proto quic
```

**交互模式**（未带 `-c` 时可用 `-i` / `--interactive` 走向导）：

```bash
phantom server -i
# 依次询问：端口 → 监听 IP → 加密算法 → 传输协议
# 端口冲突时回到询问循环，不退出
```

**TOML 加载模式**（保留传统部署方式，供 systemd 单元、CI 脚本等使用）：

```bash
phantom server -c /etc/phantom/server.toml
```

> **重跑 = 复用**：`./server.key` 已存在时不会重新生成，保证公钥稳定。需轮换时手动删除 `server.key` 再启动。

#### 2.2 客户端白名单

白名单在 `server.toml` 内的 `[[allowed_clients]]` 数组中配置（auto 模式与 load 模式通用）。**留空 = 开放模式**（任何客户端可连接），如需限制访问：

```toml
# server.toml（启动目录下的文件，load 模式则与 -c 指定的路径一致）
[[allowed_clients]]
public_key = "abc123XYZ...客户端1公钥base64..."
name = "alice-laptop"          # 可选：人类可读标签

[[allowed_clients]]
public_key = "def456UVW...客户端2公钥base64..."
name = "bob-phone"

# 留空 / 没有此段则开放模式
```

#### 2.3 服务端配置（load 模式）

`phantom server -c <toml>` 时读取 TOML。模板见 [config/server.toml](file:///Users/spencer/workspace/qoder/phantom/config/server.toml)，`install.sh` 会自动拷贝到 `/etc/phantom/server.toml`。典型内容（精简版）：

```toml
bind = "0.0.0.0:443"
private_key = "/etc/phantom/server_private"   # 由 auto 模式的 server.key 复制得到；load 模式必填
cipher = "auto"                                # auto / aes-256-gcm / aes-128-gcm / ascon-128 / chacha20-poly1305

[quic]
max_streams = 100
keep_alive_interval = 45
congestion = "cubic"          # cubic / bbr / new-reno

[tls]
disguise = false              # stub，未实现

[performance]
io_uring = false              # Linux 5.1+
zero_copy = false
workers = 0                   # 0 = CPU 核心数
```

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `bind` | `0.0.0.0:443` | 监听地址和端口 |
| `private_key` | — | 服务端密钥文件路径（必需） |
| `clients` | — | 客户端公钥白名单文件路径（空=开放） |
| `cipher` | `auto` | 加密套件 |
| `quic.congestion` | `cubic` | 拥塞控制: cubic / bbr / new-reno |
| `tls.disguise` | false | TLS 伪装（stub，未完整实现） |
| `performance.workers` | 0 | 工作线程数，0 = CPU 核心数 |

#### 2.4 使用 systemd 管理

```bash
sudo bash deploy/install.sh
# 或手动：
sudo cp target/release/phantom-server /usr/local/bin/   # 服务端独立 binary（auto-bootstrap）
sudo cp deploy/phantom.service /etc/systemd/system/
sudo systemctl enable --now phantom
```

`install.sh` 还会：
- 创建 `phantom` 系统用户与 `/var/lib/phantom` 数据目录（auto 模式写入 `server.key` 与 `server.toml` 于此）
- 将 `config/server.toml` 拷贝到 `/etc/phantom/server.toml`（保留供 load 模式使用）
- `systemctl enable` 开机自启

### 3. 客户端配置

客户端支持两种配置方式，**推荐使用 URI 快捷链接**（一行配置包含所有服务器信息），TOML 配置适用于需要精细控制的场景。

#### 3.1 URI 快捷链接（推荐）

```
phantom://<base64公钥>@<host>:<port>[?<query>][#<name>]
```

| 参数 | 说明 | 示例 |
|------|------|------|
| `base64公钥` | 服务端 X25519 公钥（标准 base64，44字符） | `dGVzdA==...` |
| `host:port` | 服务器地址和端口 | `example.com:443` |
| `cipher=` | 密码套件 | `auto`, `aes-256-gcm`, `ascon-128`, `chacha20-poly1305` |
| `proto=` | 传输协议 | `tcp`（默认）, `quic` |
| `#name` | 服务器名称 | `#primary` |

**示例：**

```bash
# 最简 URI
phantom client --server "phantom://dGVzdA==@example.com:443"

# 完整 URI（指定加密套件和传输协议）
phantom client --server "phantom://dGVzdA==@example.com:443?cipher=ascon-128&proto=quic#primary"

# URI + TOML 组合（URI 提供服务器，TOML 提供全局配置；TOML 可放在任意本地路径）
phantom client --config /path/to/your/client.toml --server "phantom://key@host:port"
```

> **获取 URI**：服务端运行 `phantom server` 后，启动目录下的 `./server.toml` 顶部注释行含完整的 `phantom://...` URI，可直接分发给客户端。

#### 3.2 TOML 配置

适用于需要多服务器 Failover、自定义规则、DNS 等高级配置的场景：

```toml
[[servers]]
name = "primary"
address = "your-server.com:443"
public_key = "服务端公钥Base64"
# 可选：覆盖全局 cipher
# cipher = "aes-256-gcm"
# protocol = "tcp"      # tcp (默认) 或 quic

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

[[rules]]
type = "port"
value = 443
action = "proxy"

[rules]
final_action = "proxy"
```

> **快速上手**：如果只需连接单台服务器，推荐使用 [3.1 URI 快捷链接](#31-uri-快捷链接推荐)，无需编写 TOML 文件。

### 4. 代理模式与路由

#### 4.1 代理模式

| 模式 | 行为 |
|------|------|
| `smart` | 基于规则分流（推荐） |
| `proxy` | 全局代理，所有流量走隧道 |
| `direct` | 全局直连，所有流量本地直连（DNS 劫持保留） |
| `auto` | 同 smart |

macOS 客户端菜单栏提供 Global / Auto / Direct 三种模式切换，实时生效。

#### 4.2 路由规则

| 规则类型 | 匹配方式 | 示例 |
|----------|---------|------|
| `domain-full` | 精确域名匹配 | `google.com` |
| `domain-suffix` | 域名后缀匹配 | `google.com`（匹配 mail.google.com） |
| `domain-keyword` | 关键词匹配 | `google` |
| `domain-regex` | 正则匹配 | `.*\.google\..*` |
| `ip-cidr` | IP CIDR 匹配（最长前缀） | `192.168.0.0/16` |
| `port` | 端口匹配 | `443` |
| `geoip` | 国家代码匹配（需 feature `geoip`） | `CN` |

#### 4.3 配置热重载

客户端运行中修改 TOML 配置文件，5 秒内自动生效：
- 规则变更
- 模式切换（smart ↔ proxy ↔ direct）

已有连接不受影响，仅新连接走新规则。

### 5. 加密套件选择

| 算法 | 条件 | 吞吐量 | 定位 |
|------|------|--------|------|
| AES-256-GCM | AES-NI / ARM CE | 5-12 GB/s | 主力：现代 CPU |
| AES-128-GCM | AES CE | 3-8 GB/s | 平衡：功耗敏感 |
| ASCON-128 | 无硬加速 | ~1-2 GB/s | NIST SP 800-232 轻量级新标准 |
| ChaCha20-Poly1305 | 任意 | ~1-2 GB/s | 兼容备选 |

`cipher = "auto"` 时自动检测：
- x86_64 + AES-NI → AES-256-GCM
- aarch64 + ARM CE → AES-256-GCM
- 其他 → ASCON-128

### 6. 流量监控

客户端在 `127.0.0.1:9150` 提供 Prometheus 格式的流量统计：

```bash
curl http://127.0.0.1:9150/metrics
```

输出包含：
- `phantom_tcp_bytes_up/down` — TCP 上下行字节数
- `phantom_udp_bytes_up/down` — UDP 上下行字节数
- `phantom_tcp_connections` — TCP 连接总数
- `phantom_udp_datagrams_up/down` — UDP 数据报总数

### 7. 性能调优

```bash
# 增大文件描述符限制
ulimit -n 65535

# Linux 内核参数
sysctl -w net.core.somaxconn=65535
sysctl -w net.ipv4.tcp_max_syn_backlog=65535
```

### 8. 故障排查

| 问题 | 可能原因 | 解决方法 |
|------|----------|----------|
| 连接超时 | 防火墙/端口未开 | 检查 `ss -tlnp \| grep 443` |
| 握手失败 | 公钥不匹配 | 确认 URI 中的公钥或 client TOML 的 `public_key` 与服务端 `server.toml` URI 注释里的公钥一致 |
| 握手被拒 | 白名单限制 | 编辑 `server.toml` 的 `[[allowed_clients]]` 数组追加客户端公钥（auto 模式改 `/var/lib/phantom/server.toml`），或清空该段回退开放模式 |
| URI 解析失败 | 公钥格式错误 | 确认公钥为 32 字节标准 Base64 编码（44 字符） |
| 端口冲突 | 443/8443 被占用 | auto 模式会自动 +1 探测；load 模式改 `bind` 字段 |
| 服务端启动报 `Address already in use` | 端口耗尽或权限 | 确认 `bind` ≤ 1024 需要 root/CAP_NET_BIND_SERVICE；检查 `ss -tlnp` |
| 连接后无数据 | DNS 解析失败 | 检查 client TOML 的 `dns` 配置 |
| 系统代理未生效 | macOS 客户端未启用 | 确认菜单栏显示 Connected |

调试模式：

```bash
RUST_LOG=debug phantom client --server "$URI"
# 或 load 模式
RUST_LOG=debug phantom client -c /path/to/your/client.toml
```

### 9. 安全注意事项

- **私钥保护**: 自举模式的 `server.key`、加载模式的 `server_private`，文件权限必须为 600
- **开放模式**: 白名单为空时任何知道服务端公钥的客户端均可连接
- **前向保密**: 每次会话派生独立密钥
- **黑洞行为**: 服务端静默丢弃未认证连接
- **systemd 加固**: NoNewPrivileges、ProtectSystem=strict

---

## 开发者参考

### 测试

```bash
# 单元测试
cargo test --lib

# E2E 测试
cargo test -p phantom-e2e --release

# 密码协商矩阵
cargo test -p phantom-e2e --test cipher_matrix --release

# 配置生效测试
cargo test -p phantom-e2e --test config_effect --release

# 全链路测试
cargo test -p phantom-e2e --test full_link_tcp --release
cargo test -p phantom-e2e --test full_link_udp --release

# DNS 劫持
cargo test -p phantom-e2e --test dns_hijack --release

# 规则引擎
cargo test -p phantom-e2e --test rule_engine --release

# Mock 百度场景
cargo test -p phantom-e2e --test real_world --release

# 真实百度场景（手动运行）
cargo test -p phantom-e2e --test real_world --release -- --ignored

# 性能测试（手动运行）
cargo test -p phantom-e2e --test performance --release -- --ignored

# 基准测试
cargo bench -p phantom-bench
```

### 测试覆盖

| 层级 | 测试文件 | 测试数 | 覆盖范围 |
|------|---------|--------|---------|
| L0 单元测试 | 各 crate #[cfg(test)] | ~80 | 帧协议、URI 构建/解析、规则引擎、DNS、stats、handler、bootstrap (密钥/白名单/端口探测) |
| L1 配置生效 | config_effect, cipher_matrix | 9 | 白名单、密码协商、echo 模式 |
| L1 模块交互 | rule_engine, dns_hijack, stats_metrics | 15 | DNS→规则、规则→路由、stats→Prometheus |
| L1 全链路 | full_link_tcp, full_link_udp, http_tunnel | 7 | TCP echo/大数据/并发、UDP relay |
| L1 真实场景 | real_world | 4 (2 ignored) | Mock 百度、真百度 |
| L1 性能 | performance, throughput | 17 (10 ignored) | 吞吐量、延迟、并发 |
| L2 系统 | cli_system | 5 | CLI 自举（auto 生成 key/URI）、端口递增 fallback、keygen 子命令已删除、version、复用已有 key |

### 项目结构

```
core/               共享类型、配置、密码套件、帧协议、传输抽象、URI 解析、错误、常量
server/             服务端连接处理、TCP relay、UDP relay
client/             SOCKS5 代理、TUN 透明代理、规则引擎、DNS 劫持、流量统计
client/cli/         命令行入口（client / server，支持 auto / interactive / load 三种启动方式）
client/mac/         macOS SwiftUI 菜单栏客户端（SPM + PhantomMacBuilder）
client/android/     Android VPN 客户端（Jetpack Compose + VpnService）
client/harmony/     HarmonyOS NEXT VPN 客户端（ArkUI + VpnExtensionAbility）
xtask/              统一构建编排器（cargo xtask）
tests/              端到端集成测试、Mock 服务器、UDP echo
tests/bench/         性能基准测试
```

### 协议设计

1. **Noise IK 握手** (ChaCha20-Poly1305): 认证 + 密钥交换 + 密码协商（零额外 RTT）
2. **HKDF 密钥派生**: 从 Noise split keys 派生双向会话密钥
3. **帧协议**: 8 字节头 + 可变 payload，支持 SYN/FIN/RST/ACK/DATA/PING/PONG/UDP
4. **UDP relay**: SYN|UDP 帧携带 [TargetAddr + datagram]，服务端 UdpSocket 转发
5. **QUIC 多路复用**: Noise IK 握手一次，后续 stream 通过 HKDF 派生子密钥

## License

MIT
