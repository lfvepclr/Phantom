# Phantom 路由器（RT-AX86U Pro）性能基线

与 [`PERF_TUN_PATH_REPORT.md`](PERF_TUN_PATH_REPORT.md)（macOS / HarmonyOS 客户端路径）同构，
本文件只记录**路由器形态**的吞吐与资源基线。

## 1. 被测环境

| 项 | 值 |
|---|---|
| 机型 | ASUS RT-AX86U Pro（BCM4912，四核 1.8 GHz，aarch64） |
| 固件 | `3.0.0.4.388_24199_koolcenter`（koolshare 官改，软件中心 1.5 代） |
| 内核 | 4.19.183（hnd/axhnd，userspace 为 32 位 ARM、kernel 为 aarch64） |
| 客户端 | `client/koolshare/` 插件，`phantom client -c … --server … --tun --gateway` |
| 二进制 | musl 全静态，`cargo xtask package koolshare`（aarch64 + armv7 双架构） |
| 服务端带宽 | **上行 3 Mbps / 下行 5 Mbps** |

## 2. 天花板（先记住这个数再判断快慢）

方向是反的：

| 客户端动作 | 占用服务端 | 理论上限 |
|---|---|---|
| 下载 | 服务端**上行** 3 Mbps | ≈ 375 KB/s（0.37 MB/s） |
| 上传 | 服务端**下行** 5 Mbps | ≈ 625 KB/s（0.61 MB/s） |

**结论：下载跑到 0.35 MB/s 左右即已到顶。** 低于此值才需要排查路由器侧瓶颈。

`phantom_speedtest.sh` 会在日志里输出「达到服务端带宽上限的百分之多少」：

- ≥ 80% → 链路健康
- 50–80% → 可优化（MTU / RTT / QUIC）
- < 50% → 排查 CPU、TUN 写队列、重传

## 3. 基线（2026-09-13 首次真机采集）

> 采集时间：2026-09-13 17:14–17:20（CST），隧道 `smart` 模式 + TCP 传输，
> 路由器自身直连、LAN 转发走 table 200 → phantom0。
> 采集命令：`/bin/sh /koolshare/scripts/phantom_config.sh diag`、`phantom_config.sh 3`、
> `top -b -n 4 -d 3`、`ip -s link show phantom0`、`curl 127.0.0.1:9150/metrics`。

### 3.1 吞吐

| 场景 | 命令 | 结果 | 日期 |
|---|---|---|---|
| 路由器侧（SOCKS5 下载 5MB） | `phantom_config.sh 3` | **359 KB/s**（5MB/14.2s，首字节 0.23s）→ 上限 375 KB/s 的 96% | 2026-09-13 |
| 路由器侧（复测） | `phantom_config.sh 3` | **380 KB/s**（5MB/13.5s）→ 103%（服务端实际略高于标称 3 Mbps） | 2026-09-13 |
| 服务端可达性（经隧道） | `curl-fancyss --socks5-hostname 127.0.0.1:1080 https://www.google.com/generate_204` | `204` / 0.27s | 2026-09-13 |
| LAN 客户端（Mac，经路由器） | `curl https://www.youtube.com/` | `200` / 2.20s；路由器记录 `route 142.251.151.4:443 -> Proxy (whitelist)` | 2026-09-13 |
| LAN 客户端直连判定（对照） | `curl https://www.google.com/generate_204` | `204` / 0.25s | 2026-09-13 |

> **注意**：路由器自带 `/usr/sbin/curl` 用 `--disable-proxy` 编译（一调 `--socks5-hostname`
> 就报 `proxy support is disabled in this libcurl`），测速必须用
> `/koolshare/bin/curl-fancyss`；`install.sh` 会把探测结果写进 `PHANTOM_CURL_SOCKS`。

### 3.2 资源

| 指标 | 命令 | 结果 |
|---|---|---|
| phantom CPU（空闲） | `top -b -n 1` | 0.0%（VSZ 26.8 MB，2.6%） |
| phantom CPU（5MB 满速下载中） | `top -b -n 4 -d 3` | 0.5–1.0%（四核合计） |
| 采样循环 CPU | `top` 看 `phantom_status.sh` | 0.2–0.3% |
| TUN 收发（累计） | `ip -s link show phantom0` | RX 94.9 MB / 119,723 pkt；TX 24.4 MB / 79,567 pkt；`errors/dropped` 全为 0 |
| 软中断（累计） | `/proc/softirqs` | NET_RX 1,587,735；NET_TX 169,045（四核分布均匀） |
| 内存 | `top` | phantom 常驻 26 MB 量级；系统 free 266 MB / cached 157 MB |

### 3.3 TUN 健康度（5 Mbps 这类低速链路上尤其重要）

| 指标 | 含义 | 期望 |
|---|---|---|
| 指标 | 实测值 | 判读 |
|---|---|---|
| `phantom_tcp_dup_bytes` | 109,636 B（对 `tcp_bytes_down` 131 MB ≈ **0.08%**） | 远低于 1%，健康 |
| `phantom_dup_acks_total` | 2,605（8 MB 下行/774 连接） | 低速链路上的常见量级 |
| `phantom_tun_write_wait_max_ms` | **3 ms** | 远小于 100 ms，写侧没有被卡 |
| `phantom_tun_txq_peak_bytes` | 101,151 B（≈99 KB） | 与 5 Mbps × RTT 的 BDP 同量级 |
| `phantom_retransmit_suppressed_total` | 15 | 预算守卫偶尔触发，正常 |
| `phantom_route_direct_failed_total` | 51 | smart 模式下直连超时回退，白名单命中后自然消失 |

## 4. 调优记录

| 日期 | 改动 | 前 | 后 | 结论 |
|---|---|---|---|---|
| 2026-09-13 | 控制面修复：软件中心 POST 契约（`$1=id`/回包 `/_resp`）、状态日志走 `/tmp/upload` + `/_temp/`、新增 `N98phantom.sh` nat-start 兜底 | 点提交「卡住 → 后台执行失败」，状态/日志页读不到 | 提交毫秒级返回、状态与日志正常、LAN 走隧道 | 控制面修复，不涉及数据面 |
| 2026-09-13 | 测速改用带 SOCKS 的 `curl-fancyss` + 修正吞吐百分比单位（kbps vs KB/s） | 测速 000 全失败；复测打印「达到 786%」 | 359–380 KB/s，达上限 96–103% | 固件自带 curl 是 `--disable-proxy` 编译 |

## 5. 方法说明

1. **先测直连再测隧道**，否则无法区分服务端带宽与路由器损耗。
2. **路由器侧测速 ≠ 客户端网速**：前者只经过 phantom 用户态转发（SOCKS5），
   后者还要过 LAN→TUN 的 NAT 转发路径，两者差值就是这段路径的损耗。
3. 每次只改一个变量，改完跑同一条命令对比。
4. 低速链路上样本不要太大：默认 5 MB（约 13 秒），再小会被 TCP 慢启动低估。
