# Phantom `phantom server` 自举改造方案

## Context

**现状**：`phantom server` 需要三步：先 `phantom keygen` 生成密钥对，再手写 `server.toml` 配置，最后启动。客户端白名单也是手维护。`deploy/` 目录里有 `install.sh` 和 `phantom.service`，但没有任何文档说明用途。

**目标**：将服务端从「手动三步」改造为「零配置自举」：
1. **删除** `phantom keygen` 子命令
2. **`phantom server`（无参数）= 自动模式**：CWD 下缺失 `./server.key` 时自动生成，端口被占用时自动递增（最多 10 次），写 `./server.config`（单行 `phantom://` URI），立即启动
3. **`phantom server -c <toml>` = 加载模式**：保持原行为兼容 systemd 部署
4. **`phantom server -i` / `--interactive` = 交互模式**：依次询问端口、IP、算法、协议
5. **服务端自举**：从生成的 URI 反向解析 bind 地址和算法，直接启动 Noise 监听器
6. **新增** `deploy/README.md` 部署说明

**用户已确认决策**：
- `server.config` = 纯文本单行 URI
- 私钥 = CWD 下 `./server.key`（权限 600）
- 端口策略 = 默认 443；auto 模式递增 10 次后报错；interactive 模式失败时询问

---

## 修改文件清单

### 核心代码

| 文件 | 改动 |
|---|---|
| [client/cli/src/main.rs](file:///Users/spencer/workspace/qoder/phantom/client/cli/src/main.rs) | 删除 `Keygen` 变体；重写 `Server` 变体支持 `-c <file>` / `-i` / `--public-host` / `--port` / `--cipher` / `--proto` |
| [core/src/uri.rs](file:///Users/spencer/workspace/qoder/phantom/core/src/uri.rs) | 新增 `build_phantom_uri()` 反序列化函数 + 单元测试 |
| [core/src/lib.rs](file:///Users/spencer/workspace/qoder/phantom/core/src/lib.rs) | `pub use uri::build_phantom_uri;` |
| [core/src/transport/tcp.rs](file:///Users/spencer/workspace/qoder/phantom/core/src/transport/tcp.rs) | 新增 `try_bind_tcp_with_fallback()` 端口探测 |
| [core/src/transport/quic.rs](file:///Users/spencer/workspace/qoder/phantom/core/src/transport/quic.rs) | 新增 `try_bind_quic_with_fallback()` 端口探测 |
| [server/src/lib.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/lib.rs) | 新增 `BootstrapOptions` 结构；拆分 `run()` 为 `run()`（薄壳）/`run_with_options()`/`run_from_uri()` |
| **新增** [server/src/bootstrap.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/bootstrap.rs) | `run_auto` / `run_interactive` / `run_from_uri` 三个公开函数；密钥/白名单/URI 文件管理；公网 IP 探测 |

### 测试

| 文件 | 改动 |
|---|---|
| [tests/tests/cli_system.rs](file:///Users/spencer/workspace/qoder/phantom/tests/tests/cli_system.rs) | 删除 `cli_keygen`；新增 auto/port-fallback/interactive/keygen-removed 4 个集成测试 |

### 部署与文档

| 文件 | 改动 |
|---|---|
| **新增** [deploy/README.md](file:///Users/spencer/workspace/qoder/phantom/deploy/README.md) | 部署说明（自举、systemd、密钥管理、端口冲突） |
| [deploy/phantom.service](file:///Users/spencer/workspace/qoder/phantom/deploy/phantom.service) | 新增 `WorkingDirectory=/var/lib/phantom` 与对应 `ReadWritePaths` |
| [deploy/install.sh](file:///Users/spencer/workspace/qoder/phantom/deploy/install.sh) | 删除 `keygen` 步骤；改为 auto 模式提示 |
| [README.md](file:///Users/spencer/workspace/qoder/phantom/README.md) | 删除 keygen 章节；新增自举 / 交互模式文档 |
| [PROJECT_PLAN.md](file:///Users/spencer/workspace/qoder/phantom/PROJECT_PLAN.md) | 更新 §2.4 CLI 描述、§3 测试计数、§7.3 配置 |
| [ARCHITECTURE.md](file:///Users/spencer/workspace/qoder/phantom/ARCHITECTURE.md) | 更新 §3 crate 矩阵、§11 测试覆盖 |

---

## 关键设计要点

### 1. CLI 子命令重构（[client/cli/src/main.rs](file:///Users/spencer/workspace/qoder/phantom/client/cli/src/main.rs#L13-L34)）

`Server` 变体从 `{ config: String }` 扩展为：

```rust
Server {
    /// -c <file>: 加载 TOML 配置
    config: Option<String>,
    /// -i / --interactive: 交互式向导
    #[arg(short, long)]
    interactive: bool,
    /// --port: 起始端口（auto/interactive）
    #[arg(long)]
    port: Option<u16>,
    /// --public-host: URI 中的 host 部分
    #[arg(long)]
    public_host: Option<String>,
    /// --cipher: 加密算法
    #[arg(long)]
    cipher: Option<String>,
    /// --proto: tcp 或 quic
    #[arg(long)]
    proto: Option<String>,
}
```

分发逻辑（互斥由 clap 校验）：
- `config.is_some()` → `phantom_server::run(&path)`（旧行为不变）
- `interactive` → `phantom_server::bootstrap::run_interactive(opts)`
- 其他 → `phantom_server::bootstrap::run_auto(opts)`

### 2. 统一启动结构（[server/src/lib.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/lib.rs)）

新增 `BootstrapOptions`：
```rust
pub struct BootstrapOptions {
    pub bind: SocketAddr,           // 已探测的真实监听地址
    pub secret_key: [u8; 32],
    pub allowed_clients: Vec<[u8; 32]>,
    pub cipher: CipherPreference,
    pub protocol: TransportProtocol,
    pub quic_congestion: CongestionAlgorithm,
    pub io_uring: bool,
}
```

三个公开入口：
- `run(config_path)` — 加载 TOML → 构造 `BootstrapOptions` → `run_with_options`
- `run_with_options(opts)` — 原 `run_tcp` / `run_quic` 分派逻辑
- `run_from_uri(uri, key_path)` — `parse_phantom_uri` → 一致性校验（URI 公钥 == 本地公钥）→ 解析 address → 构造 opts → `run_with_options`

### 3. Bootstrap 流程（[server/src/bootstrap.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/bootstrap.rs)）

**`run_auto` 核心顺序**（端口探测必须先于 URI 生成）：

1. CWD 解析 → `./server.key`、`./server.config`、`./clients`
2. **密钥**：存在则 `load_secret_from_file`（必须复用同一密钥），不存在则 `generate` + `save_secret_to_file`
3. **白名单**： `./clients` 存在则逐行 base64 解码，否则空 = 开放模式（warn）
4. **协议与算法默认值**：cipher=Auto, protocol=Tcp
5. **端口探测**：调用 `try_bind_*_with_fallback(start_addr, 10)`，拿到 `(listener, bound_addr)`
6. **公网 IP 探测**：UDP socket trick（`bind 0.0.0.0:0` + `connect 8.8.8.8:80` + `local_addr`），失败 fallback `0.0.0.0` + warn
7. **组装 URI**：`build_phantom_uri(pubkey, "{host}:{port}", cipher, proto, Some("default"))`
8. **写 `server.config`**：单行 URI + `\n`
9. **启动摘要日志**：密钥路径、URI、监听地址、白名单模式
10. `run_with_options(opts)`

**`run_interactive`**：端口/IP/算法/协议均通过 stdin 询问（每个 prompt 提供默认值），端口冲突不退出而回到询问循环；非 TTY 报错提示用 auto 模式。

### 4. URI 构建（[core/src/uri.rs](file:///Users/spencer/workspace/qoder/phantom/core/src/uri.rs)）

新增 `build_phantom_uri(key, addr, cipher, protocol, name) -> String`，与 `parse_phantom_uri` 完全对称。生成格式：
```
phantom://<key>@<addr>?cipher=<c>&proto=<p>[#<name>]
```

`cipher_to_str` / `protocol_to_str` 反向映射与 kebab-case 一致。

### 5. 端口探测（[core/src/transport/tcp.rs](file:///Users/spencer/workspace/qoder/phantom/core/src/transport/tcp.rs) 与 quic.rs）

`try_bind_*_with_fallback(start_addr, max_attempts)`：
- 保持 IP 不变，从 `start_addr.port()` 开始
- 捕获 `AddrInUse` → 端口 +1 重试
- 其他错误立即返回
- 超限返回 `PhantomError::Config` 含尝试过的端口范围
- 成功返回 `(listener, SocketAddr::new(ip, chosen_port))`

QUIC 版本在循环内每次重新调用 `QuicListener::bind`（cert/key 重新生成无副作用）。

---

## 新增/删除测试

### 删除
- `cli_system.rs::cli_keygen`（整段）

### 新增（集成）
- `cli_server_auto_generates_key_and_uri`：临时目录跑 auto，验证 `./server.key`（权限 600）与 `./server.config` 存在，URI 可解析且公钥一致
- `cli_server_auto_port_fallback`：预先占住端口 N，auto 模式下生成的 URI 端口为 N+1
- `cli_server_interactive_reads_stdin`：pipe 输入 `1445\n0.0.0.0\nauto\ntcp\n`，验证启动日志
- `cli_server_keygen_subcommand_removed`：执行 `phantom keygen`，断言 clap "unrecognized subcommand" 错误

### 新增（单元）
- [core/src/uri.rs](file:///Users/spencer/workspace/qoder/phantom/core/src/uri.rs)：5 个（`build_minimal_uri` / `build_full_uri` / `build_uri_no_name` / `build_uri_roundtrip` / `cipher_to_str_kebab_case`）
- [core/src/transport/tcp.rs](file:///Users/spencer/workspace/qoder/phantom/core/src/transport/tcp.rs)：1 个端口递增
- [core/src/transport/quic.rs](file:///Users/spencer/workspace/qoder/phantom/core/src/transport/quic.rs)：1 个端口递增
- [server/src/bootstrap.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/bootstrap.rs)：白名单解析 2 个 + 默认值 1 个

---

## 执行顺序

1. **基础工具层**：[core/src/uri.rs](file:///Users/spencer/workspace/qoder/phantom/core/src/uri.rs) 加 `build_phantom_uri` + 单测 → `cargo test -p phantom-core` 绿
2. **传输层**：[core/src/transport/tcp.rs](file:///Users/spencer/workspace/qoder/phantom/core/src/transport/tcp.rs) 与 quic.rs 加端口探测 + 单测
3. **服务端重构**：[server/src/lib.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/lib.rs) 拆分 `run` 为三函数 → 原 `phantom-server` 二进制仍可 TOML 启动
4. **Bootstrap 模块**：新增 [server/src/bootstrap.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/bootstrap.rs) + 单测
5. **CLI 改造**：[client/cli/src/main.rs](file:///Users/spencer/workspace/qoder/phantom/client/cli/src/main.rs) 删除 Keygen、重写 Server 分发
6. **集成测试**：[tests/tests/cli_system.rs](file:///Users/spencer/workspace/qoder/phantom/tests/tests/cli_system.rs) 删旧加新 → `cargo test -p phantom-e2e --test cli_system --release`
7. **部署**：改 [deploy/phantom.service](file:///Users/spencer/workspace/qoder/phantom/deploy/phantom.service) + [deploy/install.sh](file:///Users/spencer/workspace/qoder/phantom/deploy/install.sh)
8. **新增 deploy 文档**：[deploy/README.md](file:///Users/spencer/workspace/qoder/phantom/deploy/README.md)
9. **三份主文档**：[README.md](file:///Users/spencer/workspace/qoder/phantom/README.md) / [PROJECT_PLAN.md](file:///Users/spencer/workspace/qoder/phantom/PROJECT_PLAN.md) / [ARCHITECTURE.md](file:///Users/spencer/workspace/qoder/phantom/ARCHITECTURE.md) 同步

---

## 关键边缘情况（实施时注意）

| 场景 | 行为 |
|---|---|
| 重复启动 | `./server.key` 已存在 → 复用（不重新生成），日志 "reusing existing key" |
| `./server.key` 损坏 | 读取失败不自动覆盖，报错明确指向该文件 |
| 端口连续占用 | 10 次后报错列出尝试范围；interactive 模式回到 port 询问 |
| bind 成功但写 `server.config` 失败 | 错误不关闭 listener，日志提示用户，提供 `--print-uri`（v2 暂不加，但日志应打印 URI） |
| systemd CWD | `WorkingDirectory=/var/lib/phantom` 强约束，`ReadWritePaths` 同步 |
| 公网 IP 探测失败 | fallback `0.0.0.0` + warn，提供 `--public-host` 覆盖 |
| 非 TTY 跑 `-i` | 报错并提示用 auto 或 `-c` |
| URI 公钥 ≠ 本地公钥 | `run_from_uri` 一致性校验失败并报错 |
| QUIC + io_uring | `run_with_options` 内 QUIC 路径忽略 `io_uring` 字段并 warn |

---

## 端到端验证

```bash
# 1. 单元测试全绿
cargo test --lib

# 2. auto 模式端到端
WORK=$(mktemp -d) && cd "$WORK"
cargo run --bin phantom -- server --port 0 &
sleep 1 && kill $!
ls -la ./server.key            # 600
cat ./server.config            # phantom://...
URI=$(cat ./server.config)
cargo run --bin phantom -- client --server "$URI" &
sleep 1
curl -x socks5h://127.0.0.1:1080 https://example.com -o /dev/null -s -w "%{http_code}\n"
kill %2

# 3. 端口递增
python3 -c "import socket; socket.socket().bind(('127.0.0.1', 1444))" &
cd "$WORK" && cargo run --bin phantom -- server --port 1444 &
sleep 1 && kill $!
grep -oE ':[0-9]+' ./server.config   # 应为 :1445

# 4. 交互模式
cd "$WORK" && printf "1445\n127.0.0.1\nauto\ntcp\n" | cargo run --bin phantom -- server -i &
sleep 1 && kill $!

# 5. load 模式回归（systemd 路径）
cargo run --bin phantom -- server -c config/server.toml   # 行为不变

# 6. keygen 子命令消失
cargo run --bin phantom -- keygen 2>&1 | grep -i "unrecognized"

# 7. 集成测试
cargo test -p phantom-e2e --test cli_system --release

# 8. systemd 端到端
sudo mkdir -p /var/lib/phantom && sudo chown $USER /var/lib/phantom
cargo build --release
sudo cp target/release/phantom /usr/local/bin/
sudo cp deploy/phantom.service /etc/systemd/system/
sudo systemctl daemon-reload && sudo systemctl start phantom
sudo cat /var/lib/phantom/server.config
sudo systemctl stop phantom
```

