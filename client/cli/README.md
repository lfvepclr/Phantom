# Phantom CLI（`phantom-cli`）

`phantom` 单二进制的入口，把服务端与客户端两条路径收在同一个可执行文件里。
没有业务逻辑——所有能力来自 `phantom-server` 与 `phantom-client`，本 crate 只做**参数解析、模式选择、进程生命周期**。

## 子命令与模式

| 命令 | 模式 | 行为 |
|------|------|------|
| `phantom server` | auto（默认） | 零配置自举：CWD 下缺 `./server.key` 则生成，端口被占自动 +1（最多 10 次），写 `./server.toml`（顶部带 `phantom://` URI 注释），立即监听 |
| `phantom server -c <toml>` | load | 加载既有配置，供 systemd / CI 使用，模板见 `config/server.toml` |
| `phantom server -i` | interactive | TTY 向导：端口 → 监听 IP → 加密套件 → 传输协议 |
| `phantom client --server "<URI>"` | SOCKS5 | 本地 `127.0.0.1:1080`，首字节嗅探同时支持 SOCKS5 与 HTTP |
| `phantom client --server "<URI>" --tun` | TUN 透明代理 | 需要 root（macOS / Linux） |
| `phantom client --server "<URI>" --tun --gateway` | Linux 网关 | TUN + `ip rule` 策略路由，代理整个 LAN，仅 Linux |

> `phantom keygen` **已删除**——密钥由 `phantom server` 自举生成，不要再提这个子命令（有 L2 用例断言它不存在）。

## 参数约束（`client/cli/src/main.rs`）

- `--tun-addr` 必须是合法 CIDR，且**不得与 LAN 网段重叠**（默认 `10.7.0.1/24`）。
- `--tun-name` 默认 `utun7`（macOS）/ `phantom0`（Linux）。
- `--gateway` 只在 Linux 有效，非 Linux 直接报错退出。
- TUN 模式需要 root：macOS 上会检查 effective uid，`sudo open X.app` 不会提权，必须直接运行可执行文件。

## 常用命令

```bash
cargo xtask build cli                 # 构建
cargo run -p phantom-cli -- client --server "$URI"
RUST_LOG=debug cargo run -p phantom-cli -- server -i
cargo test -p phantom-e2e --test cli_system --release   # 自举 / 端口递增 / 参数校验
```
