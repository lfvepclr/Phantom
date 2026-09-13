# Phantom xtask（统一构建编排器）

`cargo xtask` 是本仓库**唯一的构建入口**：跨平台交叉编译、打包、部署、测速、图标生成、规则更新全部收敛在这里。
目的是让"怎么构建/怎么出包"只有一处答案，避免每个平台各写一套 shell。

## 源码结构

| 文件 | 职责 |
|------|------|
| `src/main.rs` | 命令解析、目标构建（`cli` / `server` / `router` / `router-armv7` / `mac` / `android` / `harmony`）、`check-deps` / `icons` / `clean` |
| `src/pack.rs` | 容器内打包（`package server`）、koolshare 插件组装（`package koolshare`）、离线验证（`verify`）、远端部署（`deploy`）、`speedtest` |
| `src/rules.rs` | `rules update` / `rules verify`：拉取上游规则并重建 `client/data/` 的 FST 索引 |

## 命令

```bash
cargo xtask build [all|server|cli|router|router-armv7|mac|android|harmony] [--release|--debug]
cargo xtask package server [--platform linux/amd64] [--engine auto|podman|docker|none]
cargo xtask package koolshare                 # 路由器软件中心离线插件（双架构，见 client/koolshare/）
cargo xtask verify  server
cargo xtask deploy  server --host root@HOST [--public-host IP] [--port 443] [--proto tcp]
cargo xtask speedtest --uri "<phantom://...>" [--check-unblock] | --loopback
cargo xtask rules update [--extended] | verify
cargo xtask check-deps        # 检查并引导安装 rustup / target / xcode / java / sips 等
cargo xtask icons             # 从 appicon.png 重新生成各平台图标
cargo xtask clean             # 清理全部构建产物
```

## 设计约定

- **宿主机不装交叉 target**：`package server` 在 `deploy/Containerfile`（`rust:1.96-alpine3.18`）里构建，服务器零编译；无容器引擎时加 `--no-container` 才走宿主机 `rust-lld`。
- **路由器两个形态分开出包**：`package koolshare` 走宿主机交叉编译（`rust-lld`，纯 Rust 依赖树），
  一次性构建 aarch64 + armv7 并组装成软件中心能识别的 `.valid` 离线包；手工部署仍是
  `build router` + `deploy/router/install.sh`。
- 产物统一落入 `dist/`：`phantom-server-<version>-<arch>.tar.gz`、`phantom-<version>.tar.gz` + 同名 `.sha256`。
- 新增平台构建请先加 `xtask` 子命令，再让 `scripts/` 里的脚本调用它，不要反过来。
