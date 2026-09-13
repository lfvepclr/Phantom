# Phantom — Agent 通用入口

本文件是**跨 Agent 的统一入口**（Claude Code / Codex / Qoder / CodeBuddy / Cursor 等都会自动读取根目录的 `AGENTS.md`）。
工具专属入口只是指向本文件的软链，内容只有这一份。

> **动手前先读文档**：本仓库每个模块目录都有 `README.md` 说明它的架构与功能边界。
> 改动某个目录 = 先读那个目录的 `README.md`，再读根 `ARCHITECTURE.md` 的对应章节，最后才动代码。
> 不要凭目录名或文件名猜职责——`server/` 和 `client/cli/src/main.rs` 都提供 `phantom server`，但职责完全不同。

## 1. 文档地图（按这个顺序读）

### 第一层：根级文档，先建立全局认知

| 文档 | 回答什么问题 | 何时必读 |
| --- | --- | --- |
| [README.md](README.md) | 这是什么、怎么装、怎么配、怎么排障（用法手册） | 涉及配置字段、URI 格式、部署步骤、故障排查 |
| [ARCHITECTURE.md](ARCHITECTURE.md) | 系统怎么分层、数据怎么流、协议长什么样、配置项由谁实现 | **任何跨模块改动**；§5 配置契约表是"配置声明→数据面实现"的唯一对照 |
| [PROJECT_PLAN.md](PROJECT_PLAN.md) | 当前路线图与阶段目标 | 判断某功能是否计划中 / 已放弃 |

### 第二层：模块 README，动哪个目录读哪个

| 目录 | README | 架构与功能 |
| --- | --- | --- |
| `core/` | [core/README.md](core/README.md) | `phantom-core`：配置、密码套件、Noise 握手、帧协议、传输抽象、`phantom://` URI。所有 crate 的依赖根，**禁止平台特判** |
| `server/` | [server/README.md](server/README.md) | `phantom-server`：Noise responder、TCP/UDP relay、Hello 验证、QUIC 多路复用、零配置自举 `bootstrap.rs`、io_uring |
| `client/` | [client/README.md](client/README.md) | `phantom-client` 共享核心：SOCKS5、TUN、规则引擎、DNS 劫持、failover、统计、Linux 网关 |
| `client/cli/` | [client/cli/README.md](client/cli/README.md) | `phantom-cli`：单二进制入口，`server` 的 auto/load/interactive 三模式与 `client` 的 SOCKS5/TUN/gateway 三形态 |
| `client/mac/` | [client/mac/README.md](client/mac/README.md) | macOS SwiftUI 菜单栏客户端（SPM + 自写 bundler，无 Xcode 工程） |
| `client/android/` | [client/android/README.md](client/android/README.md) | Android VPN 客户端（Jetpack Compose + VpnService + JNI） |
| `client/harmony/` | [client/harmony/README.md](client/harmony/README.md) | HarmonyOS NEXT 客户端（ArkUI + VpnExtensionAbility + NAPI） |
| `client/koolshare/` | [client/koolshare/README.md](client/koolshare/README.md) | 路由器 koolshare 软件中心插件：ASP 管理界面 + dbus 配置 + 双架构离线包，控制面外壳，不碰数据面 |
| `client/data/` | [client/data/README.md](client/data/README.md) | 内置被墙域名白名单（FST 索引 + CIDR），**默认直连**策略的数据来源，由 `cargo xtask rules update` 生成 |
| `xtask/` | [xtask/README.md](xtask/README.md) | 统一构建编排器：构建 / 打包 / 部署 / 测速 / 规则更新，**唯一的构建入口** |
| `tests/` | [tests/README.md](tests/README.md) | `phantom-e2e`：端到端用例索引、固件（echo / mock / 弱网）、各份测试报告 |
| `tests/bench/` | [tests/bench/README.md](tests/bench/README.md) | `phantom-bench`：divan 微基准（AEAD / 握手 / 帧编解码 / 规则引擎） |
| `config/` | [config/README.md](config/README.md) | 服务端 load 模式 TOML 模板；auto 与 load 两种配置来源的区别 |
| `scripts/` | [scripts/README.md](scripts/README.md) | 辅助脚本（构建 / 签名 / 部署 / 诊断），常规任务优先走 xtask |
| `deploy/` | [deploy/README.md](deploy/README.md) | 部署指南：容器化出包、VPS 安装、systemd |
| `deploy/alpine/` | [deploy/alpine/README.md](deploy/alpine/README.md) | Alpine / OpenRC 场景的部署包说明 |
| `deploy/router/` | [deploy/router/README.md](deploy/router/README.md) | 路由器透明网关（ASUS RT-AX86U Pro / Asuswrt-Merlin） |

### 第三层：计划与 Skills

| 路径 | 内容 |
| --- | --- |
| `.agents/plans/` | 历史计划与规格，命名 `YYYYMMDD-<标题>.md`（旧 `PLAN (n).md` 已全部重命名） |
| `.agents/skills/` | Skills 唯一事实来源：`harmonyos-build-deploy`、`harmonyos-device-automation`、`rust-async-patterns`、`rust-best-practices` |

## 2. 目录约定（工具链不再各自为政）

| 路径 | 用途 |
| --- | --- |
| `AGENTS.md` | 唯一的内容入口（本文件） |
| `CLAUDE.md` | → `AGENTS.md`（软链，给 Claude Code） |
| `.agents/skills/`、`.agents/plans/` | 唯一事实来源 |
| `.claude/skills/` | → `../.agents/skills`（软链，无独立内容） |
| `.qoder/skills/`、`.qoder/plans/` | → `../.agents/...`（软链，无独立内容） |

> 新增 Skill 请直接放 `.agents/skills/<name>/SKILL.md`，所有 Agent 自动可见，不要再往工具专属目录里放。
> `.claude/settings.local.json` / `.qoder/settings.local.json` 是工具自己的权限白名单，天然无法统一，保持原样。

## 3. 项目速览

`Phantom` 是一个全 Rust 的加密代理隧道：`Noise_IKpsk1` 握手（PSK 绑在第一条消息，抗主动探测）+ TCP/QUIC 传输，
服务端零配置自举，客户端覆盖 CLI / macOS / Android / HarmonyOS / 路由器。
分流策略是**默认直连**：只有命中白名单的目标进隧道。

## 4. 常用命令

```bash
cargo check --workspace                 # 快速校验
cargo test -p phantom-core               # 核心单测
cargo test -p phantom-e2e --release      # 端到端
cargo bench -p phantom-bench             # 性能基准
cargo xtask build <cli|server|router|mac|android|harmony> [--release]
cargo xtask package koolshare                   # 路由器软件中心离线插件（双架构）
cargo xtask package server --platform linux/amd64   # 打服务端发布包
cargo xtask rules update | verify                   # 更新 / 校验分流白名单
```

## 5. 硬约定（违反即跑偏）

- **构建只走 `cargo xtask`**，不要手写交叉编译命令；跨平台能力先加 xtask 子命令，再让 `scripts/` 调用。
- **新增配置字段必须双向同步**：`core/src/config.rs` + `ARCHITECTURE.md` §5 配置契约表 + 数据面实现，缺一不可。
- **`core/` 不写平台特判**，平台差异放 `client/src/platform/` 或各平台工程目录。
- **PSK 必须是 `psk1`**，帧格式、`stream_id = 0` 保留控制流都是跨版本兼容面，改动需先确认回归用例。
- **密钥与真实服务器地址禁止入库**，`config/` 只放可提交的示例模板。
- 规格 / 计划文档统一放 `.agents/plans/`，按 `YYYYMMDD-<标题>.md` 命名。
- 新增模块目录时**必须同时写 `README.md`**（说明架构与功能），并在这里的文档地图补一行。
