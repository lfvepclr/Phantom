# Phantom 幽灵 — 高性能加密代理隧道

Phantom 是一个基于 Rust 的加密代理隧道，使用 Noise IK 协议认证，支持自适应加密套件选择，在 Apple Silicon 上可达 5+ GB/s 吞吐量。支持 SOCKS5 代理和 TUN 透明代理两种模式，提供 macOS / Android / HarmonyOS NEXT / 路由器 / CLI 多平台客户端。

## 特性

- **自适应加密**: 自动检测 CPU 能力，选择最优加密算法（AES-256-GCM / AES-128-GCM / ASCON-128 / ChaCha20-Poly1305）
- **抗主动探测**: 线下 PSK 绑定在第一条握手消息，无 PSK 的扫描者得不到任何回应
- **零额外往返**: 密码协商嵌入 Noise 握手消息
- **透明代理**: macOS / Android / Linux TUN 模式，无需手动配置应用代理
- **路由器网关**: 华硕 RT-AX86U Pro 等 ARMv8 路由器上作为透明网关，LAN 全屋设备零配置
- **白名单分流（默认直连）**: 智能模式只把**被墙域名清单**里的目标送进隧道，其余全部直连；清单用预构建 FST 索引（4.3k 域名 ≈ 37 KiB，零解析加载），可用 `cargo xtask rules update` 更新，并在 macOS 客户端里自行增补。规则引擎同时支持域名/IP/端口/GeoIP 自定义规则；全局/自动/直连三种模式
- **DNS 劫持**: TUN 模式自动拦截 DNS 查询，防止 DNS 泄露
- **UDP Relay**: TUN 模式 UDP 流量通过帧协议隧道转发
- **系统代理自启**: macOS 启动后自动设置系统 SOCKS5 代理
- **单串配置**: `phantom://` URI 格式，一行配置包含服务器信息
- **配置热重载**: 运行中修改配置文件，规则 / 模式 / DNS 上游 / 服务器列表自动更新
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
cargo xtask build router   # 仅构建路由器客户端（aarch64 musl 静态）
cargo xtask build mac      # 仅构建 macOS 客户端
cargo xtask build android  # 仅构建 Android 客户端
cargo xtask build harmony  # 仅构建 HarmonyOS 客户端

# 服务端打包 / 部署 / 测速（服务器零编译，见 deploy/README.md）
cargo xtask package server --platform linux/amd64   # 固定容器环境出包（+校验和）
cargo xtask verify  server --platform linux/amd64   # alpine:3.18 容器内离线端到端验证
cargo xtask deploy  server --host root@HOST         # 上传 + 安装 + 回显 phantom:// URI
cargo xtask speedtest --uri "<URI>" --check-unblock # 吞吐 + 解锁断言
cargo xtask speedtest --loopback                    # 客户端软件上限（本机回环）

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

普通模式无需 sudo：`open client/mac/.build/Phantom.app` 启动后点 Start，客户端以普通用户监听
SOCKS5 `127.0.0.1:11080` 并用 `networksetup` 自动设置/还原系统代理（与其它菜单栏代理软件一致）。
可选的 TUN 透明模式才需要 root，且 `sudo open X.app` 不会提权，需直接运行可执行文件：
`sudo client/mac/.build/Phantom.app/Contents/MacOS/Phantom`。完整说明见 `client/mac/README.md`。

菜单栏只放状态与快捷开关，主界面和日志是**可缩放的独立窗口**（日志卡随窗口高度自适应，
支持 `仅隧道 / 全部` 过滤、暂停、清空与独立日志窗口）。连接信息面板提供地址、协议、加密、
密钥指纹、连接时长、实时速率与流量、隧道·直连计数，以及**测延迟 / 测速**（都经过当前隧道）
和分享二维码；退出（底部按钮 / ⋯ 菜单 / ⌘Q）会先断开隧道并还原系统代理。

菜单栏图标是模板图，四个状态按**形状**区分：空心轮廓 = 未连接、轮廓+圆点 = 连接中、
实心 = 已连接、实心+感叹号 = 错误（彩色图形会被 macOS 渲染成黑块，故不使用颜色）。

macOS 原生客户端启动后，系统代理自动生效，无需手动配置。

### 路由器客户端（华硕 RT-AX86U Pro 等）

把路由器变成透明网关，LAN 内所有设备无需任何配置：

```bash
# A. 带 koolshare 软件中心（官改 / ks 梅林）：出离线插件包，软件中心「离线安装」
cargo xtask package koolshare          # 双架构（aarch64 + armv7），产物 dist/phantom-<版本>.tar.gz

# B. 无软件中心（梅林 / 官方固件）：命令行安装
# 1. 交叉编译静态二进制（aarch64-unknown-linux-musl）
cargo xtask build router

# 2. 推送到路由器并配置开机自启
bash deploy/router/install.sh <路由器IP> "phantom://KEY@vpn.example.com:443"
```

软件中心插件带 Web 管理界面（开关、连接串、模式、白名单、定时重启、测速、上下行速率、
日志），装在 `/koolshare` 下，控制面与数据面分离、不改 Rust 核心；
完整说明见 [client/koolshare/README.md](client/koolshare/README.md)。

命令行方式的完整说明（前置条件、路由原理、DNS 取舍、故障排查）见
[deploy/router/README.md](deploy/router/README.md)。

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

> 国内用户：项目已配置字节跳动 rsproxy 镜像源（`.cargo/config.toml`），无需额外设置。

### 2. 服务端部署

#### 2.0 一键打包 + 部署（推荐，服务器零编译）

开发机出包、容器内验证、远端只解包安装：

```bash
cargo xtask package server --platform linux/amd64
# dist/phantom-server-<version>-linux-amd64.tar.gz (+ .sha256)

cargo xtask verify server --platform linux/amd64      # 需要 docker/podman

cargo xtask deploy server \
  --host root@203.0.113.10 --public-host 203.0.113.10 \
  --port 443 --proto tcp
# 结束时打印 phantom:// URI
```

默认在 `deploy/Containerfile`（`rust:1.96-alpine3.18`）里构建，宿主机不装交叉 target；
没有容器引擎时加 `--no-container` 走宿主机 `rust-lld` 交叉。Alpine/OpenRC 的安装细节、
无 Google 直连环境下的验证目标表，见 [deploy/README.md](deploy/README.md)。

#### 2.1 自举模式（零配置）

首次运行 `phantom server`（无 `-c`）即进入**自举模式**：

1. 读取 CWD 下的 `./server.key`；不存在则生成 X25519 密钥对 **与 32 字节握手 PSK** 并写入（权限 600）
2. 读取 CWD 下的 `./server.toml` 内联 `[[allowed_clients]]` 白名单；空则开放模式（info 级日志提示）
3. 默认从 0.0.0.0:443 开始探测，端口占用时自动 +1（最多 10 次）
4. 自动探测本机公网 IP（UDP socket 探测），写入 `server.toml` URI 注释的 host 段
5. 拼接 `phantom://...` URI（**含 `psk=`**），以 `#   phantom://...` 注释形式写入 `./server.toml` 顶部
6. 打印启动摘要（监听地址、URI、白名单条目数），然后启动服务

> **从旧版升级**：若 `server.key` 只有两行（PSK 支持之前生成），启动时会自动生成 PSK 并追加到第 3 行，**公钥保持不变**，但已分发的旧 URI 全部失效 —— 需重新分发日志中打印的新 URI。

| 文件 | 内容 | 权限 |
|------|------|------|
| `./server.key` | 第 1 行 base64 公钥，第 2 行 base64 私钥，第 3 行 base64 握手 PSK | 600 |
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

`phantom server -c <toml>` 时读取 TOML。模板见 [config/server.toml](config/server.toml)，`install.sh` 会自动拷贝到 `/etc/phantom/server.toml`。典型内容（精简版）：

```toml
bind = "0.0.0.0:443"
private_key = "/etc/phantom/server_private"   # 由 auto 模式的 server.key 复制得到；load 模式必填
cipher = "auto"                                # auto / aes-256-gcm / aes-128-gcm / ascon-128 / chacha20-poly1305
                                               # 注意：proto=quic 时不支持 ascon-128（QUIC 的 Noise 后端仅 AESGCM/ChaChaPoly）

[quic]
max_streams = 100
keep_alive_interval = 45
congestion = "cubic"          # cubic / bbr / new-reno

[performance]
io_uring = false              # Linux 5.1+
workers = 0                   # 0 = CPU 核心数
```

| 字段 | 默认值 | 说明 |
|------|--------|------|
| `bind` | `0.0.0.0:443` | 监听地址和端口 |
| `private_key` | — | 服务端密钥文件路径（必需） |
| `clients` | — | 客户端公钥白名单文件路径（空=开放） |
| `cipher` | `auto` | 加密套件 |
| `quic.congestion` | `cubic` | 拥塞控制: cubic / bbr / new-reno |
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
| `psk=` | 握手预共享密钥（base64，**必须**） | `psk=AAAA...` |
| `cipher=` | 密码套件 | `auto`, `aes-256-gcm`, `ascon-128`, `chacha20-poly1305` |
| `proto=` | 传输协议 | `tcp`（默认）, `quic`（不支持 cipher=ascon-128） |
| `#name` | 服务器名称 | `#primary` |

> **URI 是完整凭据**：它同时包含服务端公钥和 PSK，泄露即等于交出访问权。请通过安全渠道传递（勿贴到公开仓库 / 聊天群 / 截图）。

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
listen = "127.0.0.1:1080"             # SOCKS5+HTTP 同端口（首字节嗅探）；0.0.0.0 即局域网共享
dns = "8.8.8.8:53"                    # 走隧道的解析器（被墙域名 / proxy 模式）
dns_direct = "223.5.5.5:53"           # 直连解析器（smart 模式下未命中白名单的域名）
mode = "smart"
cipher = "auto"
# metrics_listen = "127.0.0.1:9150"   # Prometheus /metrics 端点

# 局域网共享时强烈建议开启认证（SOCKS5 RFC1929 / HTTP Basic）：
# [client.proxy_auth]
# username = "your-name"
# password = "your-secret"

[failover]
health_check_interval = 30
health_check_timeout = 5
failover_threshold = 3
graceful_migration = true             # false = 切服时主动断开旧服在飞隧道

# [hello]
# timeout = 10                        # Hello-ACK 等待超时（秒）
# targets = ["http://example.com/health"]  # 服务端优先探测的外网 URL

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

#### 4.0 客户端运行形态

| 命令 | 入口 | 适用场景 |
|---|---|---|
| `phantom client --server "$URI"` | 本地 SOCKS5（默认 `127.0.0.1:1080`） | 浏览器 / 单应用代理 |
| `sudo phantom client --server "$URI" --tun` | SOCKS5 + TUN 透明代理 | 本机全局代理（macOS / Linux） |
| `sudo phantom client --server "$URI" --tun --gateway` | TUN + 策略路由 | Linux 路由器，代理整个 LAN |

TUN 相关参数：

| 参数 | 默认值 | 说明 |
|---|---|---|
| `--tun-name` | `utun7`（macOS）/ `phantom0`（Linux） | TUN 接口名 |
| `--tun-addr` | `10.7.0.1/24` | TUN 地址，不得与 LAN 网段重叠 |
| `--tun-mtu` | `1500` | MTU |
| `--lan-interface` | `br0` | 需代理的 LAN 接口，可重复传入 |
| `--bypass` | RFC1918 + 回环 + 组播 | 继续走主路由表的目标网段，传入则覆盖默认集 |
| `--table` | `200` | 隧道默认路由所在路由表 |
| `--no-lan-dns-hijack` | 关闭（默认劫持） | 不把 LAN 的 53 端口导入隧道 |

`--gateway` 为 Linux 专属，依赖 iproute2 策略路由；进程退出时自动回滚全部路由与防火墙改动。

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

客户端运行中修改 TOML 配置文件（5 秒内自动生效，仅 TUN 模式下生效）：

| 变更项 | 行为 |
|---|---|
| `[[rules]]` / `rules.final_action` | 重建规则引擎；新规则集解析失败时保留旧规则并告警 |
| `client.mode` | smart ↔ proxy ↔ direct 切换 |
| `client.dns` / `client.dns_direct` | 重定向隧道/直连两个解析器；隧道侧会丢弃旧 UDP 流并在下次查询时重建，飞行中的查询不丢 |
| `[[servers]]` | 替换服务器池；**当前活跃服务器若仍在新列表中则保持不动**，避免无必要的切换 |
| `[failover]` | 健康检查间隔 / 超时 / 阀值即时生效 |

已有连接不受影响，仅新连接走新配置。

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

- **握手**: `Noise_IKpsk1_25519_ChaChaPoly_SHA256` —— 双向静态公钥认证 + 线下 PSK 叠加在 ephemeral DH 之上
- **抗主动探测**: PSK 绑定在**第一条**握手消息（`psk1`），无 PSK 的扫描者连第一条消息都无法构造，服务端直接断开且不回应任何数据（表现与端口无服务一致）
- **私钥保护**: `server.key`（三行：公钥/私钥/PSK）权限必须为 600
- **URI 是完整凭据**: 含公钥 + PSK，需通过安全渠道传递
- **开放模式**: 白名单为空时，任何**同时**知道服务端公钥与 PSK 的客户端均可连接
- **前向保密**: 每次会话派生独立密钥；即使 PSK 与静态私钥同时泄露，已录制的历史流量仍不可解
- **抗量子过渡**: 对称 PSK 本身抗量子，即使未来 X25519 被量子计算机突破，无 PSK 仍无法解密
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
| L0 单元测试 | 各 crate #[cfg(test)] | 109 | 帧协议、URI 构建/解析、规则引擎、DNS（上游解析/热切换）、failover（池热重载/探测切换）、stats、handler、bootstrap |
| L0 CLI 单测 | client/cli 内联 | 6 | `--tun-addr` CIDR 解析、clap 参数依赖关系 |
| L0 网关单测（Linux） | client/src/gateway.rs | 12 | ip rule / iptables 指令集、优先级排序、回滚对称性、保留路由表校验 |
| L1 配置生效 | config_effect, cipher_matrix | 9 | 白名单、密码协商、echo 模式 |
| L1 模块交互 | rule_engine, dns_hijack, stats_metrics | 15 | DNS→规则、规则→路由、stats→Prometheus |
| L1 全链路 | full_link_tcp, full_link_udp, http_tunnel | 7 | TCP echo/大数据/并发、UDP relay |
| L1 真实场景 | real_world | 4 (2 ignored) | Mock 百度、真百度 |
| L1 性能 | performance, throughput | 17 (10 ignored) | 吞吐量、延迟、并发 |
| L2 系统 | cli_system | 11 | CLI 自举、端口递增 fallback、keygen 已删除、TUN/网关参数校验与平台限制 |

> 网关单测为 Linux 专属，在 macOS 开发机上不参与运行；可用
> `cargo test -p phantom-client --target aarch64-unknown-linux-musl` 验证编译。

### 项目结构

```
core/               共享类型、配置、密码套件、帧协议、传输抽象、URI 解析、错误、常量
server/             服务端连接处理、TCP relay、UDP relay
client/             SOCKS5 代理、TUN 透明代理、规则引擎、DNS 劫持、流量统计、Linux 网关
client/cli/         命令行入口（client / server，支持 auto / interactive / load 三种启动方式）
client/mac/         macOS SwiftUI 菜单栏客户端（SPM + PhantomMacBuilder）
client/android/     Android VPN 客户端（Jetpack Compose + VpnService）
client/harmony/     HarmonyOS NEXT VPN 客户端（ArkUI + VpnExtensionAbility）
deploy/router/      路由器部署（Asuswrt-Merlin 安装脚本 + 开关包装脚本）
xtask/              统一构建编排器（cargo xtask）
tests/              端到端集成测试、Mock 服务器、UDP echo
tests/bench/         性能基准测试
```

### 协议设计

1. **Noise IKpsk1 握手** (ChaCha20-Poly1305): 双向认证 + 密钥交换 + PSK 叠加 + 密码协商（零额外 RTT）
2. **HKDF 密钥派生**: 从 Noise split keys 派生双向会话密钥
3. **帧协议**: 8 字节头 + 可变 payload，支持 SYN/FIN/RST/ACK/DATA/PING/PONG/UDP
4. **UDP relay**: SYN|UDP 帧携带 [TargetAddr + datagram]，服务端 UdpSocket 转发
5. **QUIC 多路复用**: Noise-over-QUIC（quinn-hyphae，100% Rust），连接级 Noise IK 握手一次（PSK 经 prologue 绑定），后续 stream 复用 QUIC 原生多路复用，不再叠加会话层加密

## License

MIT
