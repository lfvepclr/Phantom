# 配置模板（`config/`）

仓库内的**参考配置**，不是运行时配置。真正生效的文件在部署目录（auto 模式写 CWD 下的 `./server.toml`，systemd 场景拷到 `/etc/phantom/server.toml`）。

| 文件 | 用途 | 使用方式 |
|------|------|----------|
| `server.toml` | 服务端 **load 模式**模板（`bind` / `private_key` / `clients` / `cipher` / `[quic]` / `[performance]`） | `deploy/install.sh` 会拷到 `/etc/phantom/server.toml` |

## 服务端两种配置来源

| 模式 | 配置来源 | 说明 |
|------|----------|------|
| auto（默认，`phantom server`） | 自动生成 `./server.toml` + `./server.key`（CWD） | 密钥三行：公钥 / 私钥 / PSK；`server.toml` 顶部有 `phantom://` URI 注释可直接分发 |
| load（`phantom server -c <toml>`） | 手工维护的 TOML，本目录即模板 | `private_key` 必填，指向三行密钥文件；`clients` 留空 = 开放模式 |

## 客户端

客户端**没有仓库内模板**：推荐用一行 `phantom://` URI（`phantom client --server "<URI>"`）；
需要多服务器 failover、自定义规则、DNS、代理认证时才写 TOML，字段清单见根 `README.md` §3.2。

## 约定

- 改字段名必须同步 `core/src/config.rs` 与 `ARCHITECTURE.md` §5 配置契约表。
- 密钥文件权限 600；`server.key` 缺第 3 行 PSK 时启动会自动补写，**旧 URI 全部失效**。
- 本目录只放跨环境可提交的示例，禁止放真实密钥或真实服务器地址。
