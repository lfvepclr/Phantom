# Phantom 服务端部署包（Alpine / OpenRC）

这个目录被打进 `dist/phantom-server-<version>-linux-<arch>.tar.gz`，里面的
`install.sh` 就是远程服务器上执行的安装器：**只拷贝二进制 + 写服务文件，绝不编译**。

## 包内容

| 文件 | 用途 |
|------|------|
| `phantom` | 静态链接（musl）的 `phantom` CLI，服务器上以 `phantom server` 自举运行 |
| `install.sh` | Alpine/OpenRC 安装器：建用户、装二进制、写 `/etc/conf.d/phantom` 与 `/etc/init.d/phantom`、启动、回显 URI |
| `phantom.initd` | OpenRC 服务脚本（`start-stop-daemon`/`supervise-daemon` + `cap_net_bind_service`） |
| `phantom.confd` | 服务默认参数（端口 / 协议 / 公网 host / 加密 / 运行用户） |
| `phantom.service` | systemd 单元，供非 Alpine 主机复用同一个包 |
| `SHA256SUMS` | 包内文件校验和 |

## 一键部署（推荐，从开发机执行）

```bash
# 1) 打包（默认在固定容器里构建；--no-container 用宿主机 rust-lld 交叉）
cargo xtask package server --platform linux/amd64

# 2) 上传 + 安装 + 回显 phantom:// URI
cargo xtask deploy server \
  --host root@203.0.113.10 \
  --public-host 203.0.113.10 \
  --port 443 --proto tcp
```

## 手动部署

```bash
scp dist/phantom-server-0.1.0-linux-amd64.tar.gz root@HOST:/tmp/
ssh root@HOST
  mkdir -p /tmp/phantom-pkg && tar xzf /tmp/phantom-server-0.1.0-linux-amd64.tar.gz -C /tmp/phantom-pkg
  cd /tmp/phantom-pkg
  PHANTOM_PORT=443 PHANTOM_PROTO=tcp PHANTOM_PUBLIC_HOST=1.2.3.4 sh install.sh
```

## 安装器做了什么

1. 校验 root、架构、以及二进制确实是静态链接；
2. 建 `phantom` 系统用户/组（Alpine 上用 busybox `adduser -S -D -H`）；
3. 建 `/var/lib/phantom`（`server.key` / `server.toml` 的落点，重装保留）；
4. 停旧服务 → 原子替换 `/usr/local/bin/phantom`；
5. 写 `/etc/conf.d/phantom`、`/etc/init.d/phantom`；
6. `rc-service phantom restart` → 轮询实际监听端口（自举端口被占时会自动 +1）；
7. 若以非 root 启动失败则自动改写 `PHANTOM_USER="root"` 重启一次并报告
   （**实测结论**：Alpine 3.18 + OpenRC 0.48 无法把 CAP_NET_BIND_SERVICE 交给
   非 root 用户，`start-stop-daemon --capabilities` 依然绑定失败，因此这台
   服务器最终以 root 运行；这与 Alpine 上 VPN/代理守护进程的常规做法一致）；
8. `rc-update add phantom default`，打印 URI。

## 取 URI / 运维

```bash
# URI（含 psk，客户端直接用这一行）
sed -n 's|^#[[:space:]]*\(phantom://.*\)$|\1|p' /var/lib/phantom/server.toml | head -1

rc-service phantom status
rc-service phantom restart
tail -f /var/log/phantom.log
```

改端口 / 协议 / 公网 host：编辑 `/etc/conf.d/phantom` 后 `rc-service phantom restart`。

> `server.key` 一旦生成就不要再删：删掉会换公钥，已分发的 URI 全部失效。

## 为什么不用 glibc 构建

Alpine 的 musl 与 glibc 不兼容，`x86_64-unknown-linux-musl` 静态二进制在任何
Alpine 版本上都能直接跑（本包 4.9 MiB），因此服务器不需要装任何运行时依赖。
