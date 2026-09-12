# Phantom 服务端容器化打包 + 香港 VPS 部署 + macOS 客户端直连 Google + 速度基线

## 摘要

用 Podman + `deploy/Containerfile` 做**固定环境的容器化打包**：`rust:1.96-alpine` 构建、`alpine:3.18`（与服务器同版本）验证，宿主机不装任何交叉 target、服务器完全不编译。同一份 Containerfile 通过 `TARGETARCH` 支持 `linux/amd64` 与 `linux/arm64`（多环境）。产物是带 SHA256 的 tar.gz，装到远端 `/usr/local/bin/phantom`，以 OpenRC 常驻 `phantom server --port 443 --proto tcp --public-host 203.0.113.10`，密钥与 URI 跨重启稳定。本地构建并按你的选择部署 `phantom` CLI（含 `server` 子命令的零配置自举），macOS 客户端持久化 URI 后经系统 SOCKS5 `127.0.0.1:11080` 直连 google.com，最后测出裸链路/隧道/回环三档速度基线。

## 关键改动

**1) 容器化打包（重构 `deploy/Containerfile`）**
- 多阶段：`builder`（`--platform=$BUILDPLATFORM` + `ARG RUST_VERSION=1.96` 的 `rust:*-alpine`，`RUSTUP_DIST_SERVER=https://mirrors.aliyun.com/rustup` 装目标 std，复用仓库 `.cargo/config.toml` 的 rsproxy 源，`CARGO_HOME`/`target` 用 cache mount）→ `artifact`（`scratch`，只放 `phantom`，用 `podman build -o type=local,dest=dist/stage` 导出）→ `verify`（`alpine:3.18`，跑 `ldd`/启动校验）→ 可选 `runtime`（`alpine:3.18` + `/data` 卷的 OCI 镜像，供将来容器化部署，本 VPS 不用）。
- 架构映射：`amd64 → x86_64-unknown-linux-musl`、`arm64 → aarch64-unknown-linux-musl`；构建走 `rust-lld` 交叉、**不触发 QEMU 模拟编译**；另装 `binutils` 并 `strip --strip-all` 减小体积。
- 新增 `.dockerignore`：排除 `target/`(1.5G)、`.git/`、`client/mac/.build/`、`dist/`、`tests/e2e` 产物、`*.dmg`。

**2) 本地 Podman 环境（步骤 0，先把不确定项打掉）**
- 镜像加速写进 `~/.config/containers/registries.conf` 与 VM 内 `/etc/containers/registries.conf`，首选你给的 `https://YOUR_ID.mirror.aliyuncs.com`，失败依次退 `docker.1ms.run`、`docker.m.daocloud.io`（两者 registry API 实测正常）。注意本机没有 Docker daemon，所以你贴的 `daemon.json` 在这里等价物是 `registries.conf`；Containerfile 本身与 `docker build` 完全兼容。
- `podman machine set --cpus 4 --memory 8192` → `podman machine start`（现为 1 vCPU/2GB 且已停机 16 个月；fat-LTO 构建用它太慢）。若启动失败才考虑 `podman machine rm` + `init --now`（只影响 VM 内镜像，执行前会再确认）。
- 可行性校验：`podman pull alpine:3.18`、`podman run --platform linux/amd64 alpine:3.18 uname -m`（确认 VM 内 binfmt/QEMU 可用；不可用则 amd64 运行时验证退化到第 4 步在真服务器上做，不阻塞流程）。

**3) xtask 集成（cargo 内完成打包/验证/部署/测速）**
- `cargo xtask package server [--platform linux/amd64|linux/arm64] [--engine auto|podman|docker] [--verify] [--runtime-image] [--no-container]` → `dist/phantom-server-<version>-<arch>.tar.gz` + `.sha256`，内含 `phantom`、`install.sh`、`phantom.initd`、`phantom.confd`、`phantom.service`、`SHA256SUMS`、`README.md`。
- `cargo xtask verify server --platform …`：静态检查（`file` 为 ELF x86-64 statically linked）+ 在 `alpine:3.18` amd64 容器里真实启动服务 + 宿主机客户端穿容器端到端（映射 `127.0.0.1:18443`，URI 端口重写）访问 `https://www.google.com` 成功。
- `cargo xtask deploy server --host root@203.0.113.10 --port 443 --proto tcp --public-host 203.0.113.10`：package+verify → scp 到 `/tmp` → 远端解包执行 `install.sh` → 轮询端口 → 回显 `phantom://` URI。
- `cargo xtask speedtest --uri "<URI>" [--rounds 3] [--loopback]`：裸链路 / 隧道（CLI 客户端 SOCKS5 `127.0.0.1:1080`，多轮中位数）/ 回环三档，输出 MB/s 与 Mbps。
- `check-deps` 增加容器引擎、镜像加速、目标平台三行状态；`--no-container` 保留宿主机 rustup 交叉作为兜底（不污染环境的默认路径仍是容器）。

**4) 远端安装资产（新增 `deploy/alpine/`）**
- `install.sh`（busybox `sh`、幂等、零编译）：校验 root 与二进制静态性 → `addgroup -S phantom` + `adduser -S -D -H -h /var/lib/phantom -s /sbin/nologin -G phantom phantom` → 建 `/var/lib/phantom` → 停旧服务后原子替换 `/usr/local/bin/phantom` → 写 `/etc/conf.d/phantom` 与 `/etc/init.d/phantom` → `rc-update add phantom default` → restart → 轮询 443 → 打印 URI 与服务状态；已存在的 `server.key` 绝不删除。
- `phantom.initd`：`command="/usr/local/bin/phantom"`、`command_args="server --port ${PHANTOM_PORT} --proto ${PHANTOM_PROTO} --public-host ${PHANTOM_PUBLIC_HOST}"`、`directory="/var/lib/phantom"`、`command_user="phantom:phantom"` + `capabilities="^cap_net_bind_service"`（OpenRC 0.48 已确认支持）、`supervise-daemon` respawn、`output_log=/var/log/phantom.log`、`rc_ulimit="-n 65535"`；**若 443 绑定失败，install.sh 自动改写为 root 运行并重启一次，然后报告当前生效模式**。
- `phantom.service`（systemd）改成 `ExecStart=/usr/local/bin/phantom server …`，供非 Alpine 主机复用同一 tarball。

**5) macOS 客户端**
- `PhantomTunnel` 用 `UserDefaults` 持久化 `serverURI` 与 `proxyMode`（`didSet` 写、`init` 读），重启 App 免重贴；`cargo xtask build mac` 产出 `client/mac/.build/Phantom.app`，`sudo open` 启动（utun 需要 root），系统 SOCKS5 自动指向 `127.0.0.1:11080`，停止时恢复原代理状态。
- 文档更新：`deploy/README.md`、`deploy/alpine/README.md`、根 `README.md` 部署章节改为「容器打包 → 容器验证 → 远端安装」。

## 测试与验收

- 打包层：`cargo xtask package server --platform linux/amd64 --verify` 通过，`file` 显示 ELF x86-64 静态链接，tarball 内容与 SHA256 校验通过；`--platform linux/arm64` 同样能出包（多环境验证）。
- 容器实跑：`alpine:3.18` amd64 容器内服务正常启动并监听 443，宿主机客户端穿它访问 google.com 返回 200（部署前的真实字节路径验证）。
- 服务器：`rc-service phantom status` running、443 监听、`ldd /usr/local/bin/phantom` 非动态可执行、`rc-update show default` 含 phantom、日志无错误、内存占用远低于 1 GB；`rc-service phantom restart` 后 URI/公钥不变。
- 客户端：`curl --socks5-hostname 127.0.0.1:1080 -sI https://www.google.com` 返回 200；mac app 日志出现 Hello verification passed，`networksetup -getsocksfirewallproxy Wi-Fi` 为 `127.0.0.1:11080` 且 Enabled，Safari/Chrome 打开 google.com 正常，关闭 App 后代理恢复关闭状态。
- 速度结论：预期隧道下载 ≈375 KB/s（服务器上行 3 Mbps）、上传 ≈625 KB/s（下行 5 Mbps），与裸链路一致即证明瓶颈在 VPS 带宽；`--loopback` 客户端软件上限预期远超 100 MB/s，据此给出「客户端实现不是瓶颈」的量化结论。

## 假设与默认

- 只部署 TCP/443（已验证公网可达，`refused` 而非 `filtered`，无需改安全组）；QUIC 留作后续 A/B。
- 服务端装整个 `phantom` CLI（你的选择）：体积大于 `phantom-server`，但行为与文档中的零配置自举完全一致，且不需要预生成密钥。
- 镜像加速首次需要能拉到 `rust:*-alpine` 与 `alpine:3.18`；若三个加速地址都不可用，会停下来向你确认，而不是退回在服务器上编译。
- 私钥不上传、不进镜像：`server.key` 由服务器首次启动时在 `/var/lib/phantom` 生成，之后重装保留。
- 测速按 3 轮取中位数；mac app 的 TUN 不装默认路由，浏览器走系统 SOCKS5 代理这条路。
