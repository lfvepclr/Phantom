# Phantom 路由器客户端部署（ASUS RT-AX86U Pro / Asuswrt-Merlin）

把路由器变成透明网关：LAN 内所有设备无需任何配置，流量自动经 Phantom 隧道出网。

---

## 0. 先选安装方式

| 场景 | 用什么 | 说明 |
|---|---|---|
| **固件带 koolshare 软件中心**（官改 / ks梅林，如 `3.0.0.4.388_24199_koolcenter`） | [`client/koolshare/`](../../client/koolshare/README.md) 插件 | 有 Web 管理界面：开关、连接串、模式、白名单、定时重启、测速、上下行速率、日志；`cargo xtask package koolshare` 出离线包，软件中心「离线安装」即可 |
| 梅林 / 官方固件，无软件中心 | 本目录的 `install.sh`（命令行），或插件的**降级安装模式** | 插件 `install.sh` 检测不到 `/koolshare` 时会自动改为装到 `/jffs/phantom` 并注册 `services-start` / `nat-start`，配置存文件而非 dbus |
| 只要一份最小脚本、不要 Web | 本目录 | 下面正文即此方式 |

> 本目录的 `phantom.sh` 与插件共用同一套启动参数（`client --tun --gateway`），
> 但配置来源不同：这里是 `/jffs/phantom/phantom.conf`，插件是 dbus。
> **两者不要同时启用**，否则两个进程会抢同一个 TUN 与路由表。

---

## 1. 原理

```
LAN 客户端 ──┐
             │  转发流量 (iif br0)
             ▼
   ┌─────────────────────────────────────────────┐
   │  路由器 (RT-AX86U Pro, aarch64)             │
   │                                             │
   │  ip rule iif br0 → table 200                │
   │  table 200: default dev phantom0            │
   │                     │                       │
   │                     ▼                       │
   │  phantom0 (TUN) ─► TunProxy ─► SOCKS5 ─┐   │
   │                                         │   │
   └─────────────────────────────────────────┼───┘
                                             │ Noise + AEAD
                                             ▼
                                        Phantom Server
```

关键设计：**按入向接口（`iif`）选路，而不是按源网段。**

路由器自身发起的流量（包括 Phantom 到服务端的隧道连接、dnsmasq 的上游查询）没有
`iif`，因此永远不会匹配到 table 200，也就不可能回环进 TUN。这样就不需要为服务端 IP
或 WAN 网关做任何特例路由。

私有网段（RFC1918、回环、组播等）通过更高优先级的 `lookup main` 规则放行，LAN 内互访
和访问路由器管理页面不受影响。完整默认放行列表见
[`client/src/gateway.rs`](../../client/src/gateway.rs) 的 `DEFAULT_BYPASS_CIDRS`。

---

## 2. 前置条件

**路由器侧**（Web 管理界面 → 系统管理 → 系统设置）：

| 项 | 要求 |
|---|---|
| SSH | 启用（Enable SSH = LAN only 即可） |
| JFFS 分区 | 启用（Enable JFFS custom scripts and configs） |
| 固件 | Asuswrt-Merlin（官方固件缺少 `/jffs/scripts` 钩子） |
| 架构 | `aarch64`（BCM4912/BCM4908 机型）；用 `ssh admin@router uname -m` 确认 |

> 本文示例统一写 `ssh admin@<路由器IP>`。若路由器把 SSH 改到了别的端口（示例写作
> `<SSH端口>`），把 `-p` 加在 host **之前**：`ssh -p <SSH端口> admin@<路由器IP>` ——
> 写成 `ssh admin@<路由器IP> -p <SSH端口>` 会被当成远端命令执行。
> scp 同理（大写 `-P`）：`scp -P <SSH端口> <file> admin@<路由器IP>:/tmp/`。
> 另外路由器上的 `sh` 必须写成 `/bin/sh`（`/usr/sbin/sh` 是 Broadcom 调试工具）。

`ip`、`iptables`、`/dev/net/tun` 在 Merlin 上默认可用（OpenVPN 依赖它们），
安装脚本会在拷贝任何文件之前逐项校验。

**开发机侧**：

```bash
cargo install cross --locked
podman machine init && podman machine start    # 或 Docker Desktop
cargo xtask check-deps                          # 确认 router 目标就绪
```

---

## 3. 构建与安装

```bash
# 1. 交叉编译静态二进制（aarch64-unknown-linux-musl，容器内完成）
cargo xtask build router
# 产物：target/aarch64-unknown-linux-musl/release/phantom

# 2. 推送到路由器并配置开机自启
bash deploy/router/install.sh <路由器IP> "phantom://KEY@vpn.example.com:443?cipher=auto"
```

URI 从服务端获取：

```bash
ssh your-server "grep '^#   phantom://' /var/lib/phantom/server.toml | sed 's/^#   //'"
```

`install.sh` 会：

1. 校验路由器环境（JFFS、`/jffs/scripts`、`ip`/`iptables`、架构匹配）
2. 拷贝二进制到 `/jffs/phantom/phantom`、包装脚本到 `/jffs/phantom/phantom.sh`
3. 生成 `/jffs/phantom/phantom.conf`（权限 600；**重装时只更新 URI，保留本地调优**）
4. 注册两个钩子：
   - `/jffs/scripts/services-start` — 开机自启
   - `/jffs/scripts/nat-start` — Asuswrt 重建 iptables 时重启 Phantom 补回规则
5. 启动并打印状态

选择静态 musl 而非 glibc 的原因：Merlin 的 glibc 版本较老且不提供开发头文件，
静态链接是唯一能在多个固件版本间通用的方案。

---

## 4. 日常运维

```bash
ssh admin@<路由器IP> /jffs/phantom/phantom.sh status     # 进程 + ip rule + table + metrics
ssh admin@<路由器IP> /jffs/phantom/phantom.sh log 100    # 最近 100 行日志
ssh admin@<路由器IP> /jffs/phantom/phantom.sh restart
ssh admin@<路由器IP> /jffs/phantom/phantom.sh stop       # 停止并回滚全部路由/防火墙改动
```

### 配置项（`/jffs/phantom/phantom.conf`）

| 变量 | 默认值 | 说明 |
|---|---|---|
| `PHANTOM_URI` | — | 服务端 `phantom://` 链接（必填） |
| `TUN_NAME` | `phantom0` | TUN 接口名 |
| `TUN_ADDR` | `10.7.0.1/24` | TUN 地址，**不得与 LAN 网段重叠** |
| `LAN_IF` | `br0` | 需要代理的 LAN 接口，空格分隔（访客网络加 `br1`） |
| `TABLE_ID` | `200` | 隧道默认路由所在路由表 |
| `LAN_DNS_HIJACK` | `1` | 是否把 LAN 的 53 端口流量导入隧道 |
| `RUST_LOG` | `info` | 日志级别，排障时设 `debug` |

改完执行 `phantom.sh restart` 生效。

### DNS 的取舍

`LAN_DNS_HIJACK=1`（默认）会把 LAN 客户端的 53 端口请求 DNAT 走隧道，交给 TUN 的
DNS 劫持模块处理。**这是域名类分流规则（`domain-suffix` 等）对 LAN 客户端生效的前提** —— 
规则引擎依赖 DNS 响应建立 IP→域名映射。

代价是绕过了路由器的 dnsmasq，因此本地主机名（如 `router.asus.com`、DHCP 静态映射的
主机名）不再可解析。若更看重本地 DNS，设 `LAN_DNS_HIJACK=0`；此时 LAN 客户端只能命中
`ip-cidr` / `port` / `geoip` 类规则。

---

## 5. 故障排查

| 现象 | 原因 | 处理 |
|---|---|---|
| `/dev/net/tun unavailable` | tun 模块未加载 | 在 Web UI 开一次 OpenVPN 客户端触发加载，或 `insmod tun` |
| 启动即退出，日志有 `Hello verification failed` | 服务端不可达 / 公钥不符 / 白名单拒绝 | 见根 README §8 |
| LAN 客户端完全断网 | `TUN_ADDR` 与 LAN 网段重叠 | 改 `TUN_ADDR` 到不冲突网段后重启 |
| 隧道正常但 LAN 走的是直连 | `LAN_IF` 名字不对 | `ip link show` 确认桥接名，多数机型是 `br0` |
| 只有域名规则不生效 | `LAN_DNS_HIJACK=0` | 设为 `1` 后重启 |
| 重启路由器后失效 | JFFS 自定义脚本未启用 | Web UI 启用后重跑 `install.sh` |
| 运行一段时间后 iptables 规则消失 | Asuswrt 重建了 NAT 表 | 确认 `/jffs/scripts/nat-start` 含 phantom 行 |

手工核对内核状态：

```bash
ssh admin@<路由器IP> 'ip rule show; ip route show table 200; ip link show phantom0'
```

`ip rule show` 应能看到：优先级 9040 的一批 `lookup main` 放行规则，以及优先级 9050 的
`iif br0 lookup 200`。

### 彻底卸载

```bash
ssh -p <SSH端口> admin@<路由器IP> '/bin/sh -c "
/jffs/phantom/phantom.sh stop
sed -i /phantom/d /jffs/scripts/services-start /jffs/scripts/nat-start
rm -rf /jffs/phantom
"'
```

---

## 6. 其他 Linux 路由器 / 主机

同一套机制适用于任何有 iproute2 的 Linux（OpenWrt、树莓派旁路由、x86 软路由）：

```bash
# 网关模式（代理整个 LAN）
sudo phantom client --server "$URI" \
  --tun --tun-name phantom0 --tun-addr 10.7.0.1/24 \
  --gateway --lan-interface br-lan --table 200

# 单机透明代理（只代理本机，不动转发路由）
sudo phantom client --server "$URI" --tun
```

非 aarch64 目标改 `--target` 重新编译，例如 32 位 ARM：

```bash
cross build -p phantom-cli --release --target armv7-unknown-linux-musleabihf
```

`--gateway` 为 Linux 专属；在 macOS 上传该参数会直接报错退出。
