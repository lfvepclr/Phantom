# Phantom 容器化打包 + 香港 VPS 部署 + macOS 客户端直连解锁 + 速度基线（离线可测版）

## 摘要

Podman + `deploy/Containerfile` 固定环境打包（`rust:1.96-alpine` 构建、`alpine:3.18` 验证，宿主机不装交叉 target、服务器零编译），Rust 工具链与 crate 全走 RsProxy。产物 tar.gz + SHA256，远端装 `/usr/local/bin/phantom` 并以 OpenRC 常驻 `phantom server --port 443 --proto tcp --public-host 203.0.113.10`。**测试目标全部改为可控/离线**：部署前用 `minihttpd` 本地 origin（字节对账）+ baidu.com（真实公网中继对照）；部署后用 google/gstatic `generate_204`、Cloudflare `cdn-cgi/trace`（断言出口 `ip=203.0.113.10 loc=HK`）、youtube 200 作为解锁证据；吞吐用本地 origin / VPS 侧临时 origin / Cloudflare speed 三档锚点。

## 关键改动

**1) 容器化打包（重构 `deploy/Containerfile`）**
- 多阶段：`builder`（`--platform=$BUILDPLATFORM` + `ARG RUST_VERSION=1.96` 的 `rust:*-alpine`；`ENV RUSTUP_DIST_SERVER=https://rsproxy.cn RUSTUP_UPDATE_ROOT=https://rsproxy.cn/rustup`；crate 沿用仓库 `.cargo/config.toml` 的 `sparse+https://rsproxy.cn/index/`；`CARGO_HOME`/`target` 用 cache mount）→ `artifact`（`scratch` 只放 `phantom`）→ `test-artifacts`（同一 builder 里用 `rustc -O --target x86_64-unknown-linux-musl tests/e2e/minihttpd.rs` 编出测试用 origin，**不进部署包**）→ `verify`（`alpine:3.18`）→ 可选 `runtime`（`alpine:3.18` + `/data` 卷 OCI 镜像，本 VPS 不用）。
- 架构映射 `amd64→x86_64-unknown-linux-musl`、`arm64→aarch64-unknown-linux-musl`，`rust-lld` 交叉、不触发 QEMU 模拟编译；装 `binutils` 并 `strip --strip-all`。
- 新增 `.dockerignore`：排除 `target/`(1.5G)、`.git/`、`client/mac/.build/`、`dist/`、`tests/e2e` 产物、`*.dmg`。

**2) 本地 Podman 环境（步骤 0）**
- 镜像加速写入 `~/.config/containers/registries.conf` 与 VM 内 `/etc/containers/registries.conf`：首选 `YOUR_ID.mirror.aliyuncs.com`，失败退 `docker.1ms.run`、`docker.m.daocloud.io`（本机没有 Docker daemon，`daemon.json` 的等价物是 `registries.conf`；Containerfile 与 `docker build` 兼容）。
- `podman machine set --cpus 4 --memory 8192` → `podman machine start`（现 1vCPU/2GB 且停机 16 个月）；仅当启动失败才考虑 `rm`+`init --now`（只影响 VM 内镜像，执行前再确认）。
- 校验：`podman pull alpine:3.18`、`podman run --platform linux/amd64 alpine:3.18 uname -m`；若 VM 内 binfmt 不可用，amd64 运行时验证退化到真服务器上做，不阻塞流程。

**3) xtask 集成**
- `cargo xtask package server [--platform linux/amd64|linux/arm64] [--engine auto|podman|docker] [--verify] [--runtime-image] [--no-container]` → `dist/phantom-server-<ver>-<arch>.tar.gz`（`phantom`、`install.sh`、`phantom.initd`、`phantom.confd`、`phantom.service`、`SHA256SUMS`、`README.md`）+ `.sha256`。
- `cargo xtask verify server --platform …`（**离线**）：`file` 静态检查 → 起 `phantom-server-verify` 容器（amd64/alpine:3.18）+ 同网络 `phantom-origin` 容器（minihttpd，提供 `/10mb.bin`、`/100kb.bin`、`/health`）→ 宿主机客户端穿隧道按 **origin 容器 IP** 拉 10MB（避免依赖 podman 内 DNS）→ `sha256sum` 双侧对账 + 记录吞吐 → 再经隧道访问 `http://www.baidu.com`（国内可达）证明真实公网中继可用。
- `cargo xtask deploy server --host root@203.0.113.10 --port 443 --proto tcp --public-host 203.0.113.10`：package+verify → scp → 远端解包 install → 轮询 443 → 回显 `phantom://` URI。
- `cargo xtask speedtest --uri "<URI>" [--rounds 3] [--origin loopback|vps|cloudflare] [--check-unblock]`：三档锚点 + 解锁断言（见测试计划）。
- `check-deps` 增加容器引擎、镜像加速、目标平台三行；`--no-container` 保留宿主机 rustup 交叉兜底（同样走 rsproxy）。

**4) 远端安装资产（新增 `deploy/alpine/`）**
- `install.sh`（busybox `sh`、幂等、零编译）：校验 root 与静态性 → `addgroup -S phantom` + `adduser -S -D -H -h /var/lib/phantom -s /sbin/nologin -G phantom phantom` → 建 `/var/lib/phantom` → 停旧服务后原子替换 `/usr/local/bin/phantom` → 写 `/etc/conf.d/phantom`、`/etc/init.d/phantom` → `rc-update add phantom default` → restart → 轮询 443 → 打印 URI 与状态；已有 `server.key` 绝不删除。
- `phantom.initd`：`command="/usr/local/bin/phantom"`、`command_args="server --port ${PHANTOM_PORT} --proto ${PHANTOM_PROTO} --public-host ${PHANTOM_PUBLIC_HOST}"`、`directory="/var/lib/phantom"`、`command_user="phantom:phantom"` + `capabilities="^cap_net_bind_service"`（OpenRC 0.48 已确认支持）、`supervise-daemon` respawn、`output_log=/var/log/phantom.log`、`rc_ulimit="-n 65535"`；若 443 绑定失败，install.sh 自动改用 root 重启一次并报告生效模式。
- `phantom.service`（systemd）改为 `ExecStart=/usr/local/bin/phantom server …`，非 Alpine 主机复用同一 tar.gz。

**5) macOS 客户端**
- `PhantomTunnel` 用 `UserDefaults` 持久化 `serverURI` 与 `proxyMode`；`cargo xtask build mac` → `sudo open client/mac/.build/Phantom.app`；系统 SOCKS5 自动指向 `127.0.0.1:11080`，停止时恢复原代理。

**6) 文档**
- `deploy/README.md`、`deploy/alpine/README.md`、根 `README.md` 部署章节改为「容器打包 → 离线验证 → 远端安装」；新增一节「在没有 Google 直连的环境下如何验证解锁」，列明各目标与断言。

## 测试与验收

- **T1 打包层**：`package --platform linux/amd64 --verify` 通过；`file` 为 ELF x86-64 静态链接；tarball 清单与 SHA256 校验通过；`--platform linux/arm64` 同样出包（多环境）。
- **T2 部署前离线 e2e**（替代原「容器打 google」）：穿 amd64 容器内 server 从 `phantom-origin` 拉 10MB，`sha256` 与服务端完全一致；`/100kb.bin` 多次延迟采样；经隧道访问 baidu.com 得到 200。**不需要任何被墙目标**。
- **T3 部署后解锁证据**（`--check-unblock`，全部在服务器侧已实测可用）：`https://www.google.com/generate_204` = 204；`https://www.gstatic.com/generate_204` = 204；`https://www.cloudflare.com/cdn-cgi/trace` 断言 `ip=203.0.113.10`、`loc=HK`、`colo=HKG`；`https://www.youtube.com/` = 200。
- **T4 服务器健康**：`rc-service phantom status` running、443 监听、`ldd /usr/local/bin/phantom` 非动态可执行、`rc-update show default` 含 phantom、日志无错误、内存远低于 1GB；`rc-service phantom restart` 后 URI/公钥不变。
- **T5 速度基线**：a) 本机回环（本地 origin）→ 客户端软件上限（预期 ≫100 MB/s）；b) VPS 侧临时 origin（10MB 静态文件，测完即停）→ 纯隧道天花板，零第三方依赖；c) Cloudflare speed 端点 → 交叉验证；d) 裸链路 ssh/scp → 已实测 387 KB/s（≈3 Mbps）。结论预期：隧道下载 ≈375 KB/s、上传 ≈625 KB/s，与裸链路一致 ⇒ 瓶颈在 VPS 带宽，客户端不是瓶颈。
- **T6 客户端**：`curl --socks5-hostname 127.0.0.1:1080` 走 T3 目标均通过；mac app 日志 Hello verification passed、`networksetup -getsocksfirewallproxy Wi-Fi` = `127.0.0.1:11080` 且 Enabled、Safari/Chrome 打开 google.com 正常，关闭 App 后代理恢复原状。
- **浏览器 DNS 风险与处置**：`curl --socks5-hostname` 一律远端解析，必然成功；若浏览器失败而 curl 成功，先用 `cdn-cgi/trace` 判定是 DNS 污染而非链路问题，再按序处置：浏览器 secure DNS / TUN DNS 劫持（上游 `tls://8.8.8.8:853`）/ 系统 DNS 调整。

## 假设与默认

- 只部署 TCP/443（已验证 `refused` 而非 `filtered`，无需改安全组）；QUIC 留作后续 A/B。
- 服务端装整个 `phantom` CLI（你的选择），行为与文档的零配置自举一致，无需预生成密钥。
- 部署前的验证**不依赖任何被墙目标**；解锁结论由部署后的 T3 目标集合给出。
- 容器镜像/crate 均走国内镜像；三个容器加速地址都不可用时停下来向你确认，而不是退回服务器编译。
- `server.key` 由服务器首次启动生成、重装保留；私钥不上传、不进镜像。
- 测速取 3 轮中位数；mac app 的 TUN 不装默认路由，浏览器走系统 SOCKS5 代理。
