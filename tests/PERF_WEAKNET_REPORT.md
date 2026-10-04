# 弱网（地铁 / 蜂窝）性能报告

## 背景

用户报告：地铁里用蜂窝开 VPN，外网"特别慢、加载不出来"。代码盘点定位到五个结构性缺陷，
本轮先补仪器化与可复现台架，再修，最后用同一套脚本复测：

| 编号 | 缺陷 | 位置 |
|------|------|------|
| W1 | 外层隧道 socket 静默死亡最长 ~90 s 才被发现（keepalive 60 s + 3×10 s，无 `TCP_USER_TIMEOUT`） | `core/src/transport/tcp.rs` |
| W2 | 内层流 6 s 无进展就被静默丢弃：不发 RST、不给 relay 停止信号（任务与上游 socket 泄漏） | `client/src/tun.rs` |
| W3 | 鸿蒙侧任何网络能力变化都 `bump_network_epoch()`，所有流立刻 RST | `PhantomVpnExtensionAbility.ets` |
| W4 | 多条 App 流复用同一条到 VPS 的 TCP（`pooled session`），一次丢包阻塞全部流 | 传输选型 |
| W5 | VPN 只接管 IPv4；双栈蜂窝下 AAAA 直连物理链路，被墙地址黑洞 | `PhantomVpnExtensionAbility.ets` + `client/src/dns.rs` |
| W5b | **修 W5 时暴露出来的第二层缺陷**：出口（服务器）没有 IPv6，App 拿到 AAAA 后反复重试一个永不可达的地址，**不回落 IPv4** —— 页面永远打不开 | `tun.rs::handle_dns_query` + `dns.rs::build_nodata_response()` |

## 环境与工具

| 项 | 值 |
|---|---|
| 服务器 | Alpine 3.18 / 1 vCPU / 香港；**实测已运行 BBR + fq**（`tcp_available_congestion_control = reno cubic bbr`，`default_qdisc = fq`），**无 IPv6 出口** |
| 真机 | HOP-AL00 / HarmonyOS 7.0.0.x（API 26），蜂窝为中国联通 5G（IPv4 + 2408::/64 双栈） |
| 弱网台架 | `scripts/weaknet-mac.sh`（Mac 做 pf rdr+NAT 中继 + dummynet 塑形） |
| 真机采样 | `scripts/harmony-bench.sh`（`vpn-tun` 计数 + **外层内核重传** `/proc/net/tcp{,6}` `retrnsmt` + trace 分析） |
| 抓包分析 | `scripts/tun-trace-report.py` |
| Mac 对照 | `phantom client --tun-trace <path>`（新增，桌面用同一套分析器） |

### 弱网档位（固定，不再临时定义）

| 档位 | 单向时延 | 丢包 | 限速 | 说明 |
|------|----------|------|------|------|
| `W1` 通勤抖动 | 40 ms | 1% | 10 Mbit/s | 有信号但换小区频繁 |
| `W2` 弱信号 | 100 ms | 3% | 2 Mbit/s | 车厢内 |
| `W3` 过隧道断流 | 75 ms | 5% | 4 Mbit/s | 每 40 s 插入 8 s 全丢包 |

> macOS 的 dummynet 没有抖动参数（`dnctl pipe … config` 只有 `bw`/`delay`/`plr`/`queue`），
> 因此档位里的"抖动"用固定时延近似；时延按**单方向**配置，RTT 约为其两倍。

### 复现

```bash
# 1) Mac 当前 IP（脚本会自动发现），手机需在同一 Wi-Fi
sudo scripts/weaknet-mac.sh setup w2 --vps <VPS_IP>
#    然后把手机上的 phantom:// 链接的 host:port 换成 <Mac IP>:443（key/psk 不变）导入
#    打开 App → 设置 → 记录 TUN 追踪 → 启动隧道

# 2) 每个档位跑两个工况各 60 s
scripts/harmony-bench.sh --label "w2 dl.google.com" --seconds 60 --out tests/PERF_WEAKNET_REPORT.md
scripts/harmony-bench.sh --label "w2 youtube"       --seconds 60 --out tests/PERF_WEAKNET_REPORT.md

# 3) Mac 对照：同一档位、同一时段
sudo phantom client --server "<指向 Mac 中继的 URI>" --tun --tun-trace /tmp/mac-tun-trace.log
python3 scripts/tun-trace-report.py /tmp/mac-tun-trace.log

# 4) 收工必须还原机器状态
sudo scripts/weaknet-mac.sh teardown
```

## 验收阈值

- 鸿蒙 TUN 平均下行 ≥ 同档 SOCKS5 传输参照的 **60%**，且 ≥ 同档 Mac 桌面 TUN 参照的 **80%**；
- 内层重传 ≤ **5 次/分钟**、重复注入 ≤ **1.2×** 有效字节（沿用 `PERF_TUN_PATH_REPORT.md`）；
- 外层内核重传首轮只报告（`W1/W2` 目标 ≤3%、`W3` ≤8%，基线出来后校准）；
- `W3` 每次 8 s 断流结束后 **5 s 内**下行恢复到断流前的 ≥50%，整段 `flow_stall_drops = 0`、`vpn-tun` TX dropped 增量为 0；
- QUIC 对照：同档比较 60 s 平均下行与首字节时间，倍数写进下表。

## 改动前基线（待采集）

> 需要 sudo 运行台架、并手动在手机上驱动工况，因此这一节留待正式采集时补齐；
> 采完后按下面的表逐行填入，**不要凭印象填写**。

| 档位 | 场景 | 路径 | 平均下行 (B/s) | 中位 (B/s) | 外层重传/分钟 | 内层重传/分钟 | 重复/有效 | 断流恢复 (s) | 判定 |
|---|---|---|---|---|---|---|---|---|---|
| — | — | — | — | — | — | — | — | — | 待采集 |

## 改动后（已完成部分：代码级验证）

以下三条是今天在真机 / 真服务器上**实际跑出来**的结果，不是推断。

### 1. IPv6（W5 + W5b）：已修，蜂窝下实测

修复前（同一台手机、同一张卡，改前构建）：

| 观测 | 结果 |
|------|------|
| `/proc/net/tcp6` | 2 条 `SYN_SENT` → `2404:6800:4005:0827::200e:443`（ipv6.google.com），源地址是**运营商地址**（`2408:8441:…`），持续 25 s+ 不消失 |
| 应用日志 | `ipv6` 出现 **0** 次 —— 这条连接从未进入隧道 |
| 浏览器 | `No response from this site` |

修复后（同一台手机、同一张卡，**第一次修复**）：

| 观测 | 结果 |
|------|------|
| `vpn-tun` IPv6 | 已持有 `fd00:8:8::2/64`，并装上 `::/0` 默认路由（`/proc/net/ipv6_route`） |
| `/proc/net/tcp6` | 到 `2404:6800:4005:0827::200e` 的连接**源地址是 `fd00:8:8::2`**（隧道内），不再是运营商地址 |
| 应用日志 | `route 2404:6800:4005:827::200e:443 -> Proxy (whitelist)` → 走隧道 |
| 结果 | 服务器**没有 IPv6 出口**，因此该目标被服务器快速 RST（`Server unreachable`） |

> **这一版是错的，而且比"黑洞挂死"更难看出来。** 当时的判断是"快速失败，App 自然会回落
> IPv4"，但没有验证回落真的发生。用户复测指出"手机浏览器上访问 google.com 根本没有成功"，
> 取真机日志才看到真相：App 拿到 IPv4（`route m.youtube.com:53 -> Proxy (dns tunnel) 142.250.199.238`）
> 之后仍然先去连 AAAA，失败后**又重试同一个 IPv6 地址**，一秒一轮，从不使用那条 IPv4：
>
> ```
> route 2404:6800:4005:827::200e:443 -> Proxy (whitelist)
> Tunnel failed → [2404:6800:4005:827::200e]:443: Server unreachable: default
> route 2404:6800:4005:827::200e:443 -> Proxy (whitelist)      ← 同一地址重试
> Tunnel failed → [2404:6800:4005:827::200e]:443: Server unreachable: default
> ```
>
> 教训：**"隧道能连上"和"页面能打开"是两件事**，只有后者算验收。

#### W5b 修复：隧道内域名不下发 AAAA

出口能不能用 IPv6 是未知数，那就不要把不可用的地址交给 App。新增
`client.dns_ipv6_via_tunnel`（默认 `false`）：**隧道内**域名（Proxy 路由）的 AAAA 查询
由劫持直接回 `NOERROR/NODATA`（`build_nodata_response()`），App 于是只能用 A 记录；
**直连**域名（Local 路由，国内站点）的 AAAA 照常转发，它的 IPv6 是真能用的。
服务器确实有 IPv6 出口时把它打开即可恢复。

同一台手机、同一张卡、同一 Wi-Fi，重测结果：

| 观测 | 结果 |
|------|------|
| 启动日志 | `DNS hijack enabled, …, tunnel AAAA = suppressed` |
| google.com | **页面完整加载**（Google 首页、搜索框、热门搜索） |
| m.youtube.com | **页面完整加载**，视频首帧渲染、评论与推荐位都是实内容 |
| 视频 CDN | `googlevideo.com` → `74.125.106.106` / `142.251.80.226`，全部走隧道 |
| 会话统计 | `tunnel_down = 2.81 MB`、`tunnel_connects = 39`、**`tunnel_connect_failures = 0`**、`net_epoch_bumps = 0` |
| `2404:6800` / `Tunnel failed` 出现次数 | **0**（修复前每个 Google 连接都在这里打转） |

### 2. 传输换挡（W4）：已修，双端实测

- 鸿蒙设置面板新增「传输协议 TCP / QUIC」，写入 `phantom_ui.transport`，启动请求里的 URI 会被改写为对应 `proto=`（真机确认 `phantom_vpn_start.txt` 的 URI 带 `proto=quic`，卡片与详情页显示生效值）。
- VPS 上并跑 `phantom-quic`（OpenRC 实例，UDP 443；`deploy/alpine/enable-quic.sh`）：实测 `netstat -lun` 有 `0.0.0.0:443`，TCP 443 同时保持监听。
- 端到端：Mac CLI 用同一 URI 分别以 `proto=quic` / `proto=tcp` 访问 `https://www.google.com/generate_204`：

| 传输 | 结果 | 日志 |
|------|------|------|
| QUIC | HTTP 204，0.28 s | `Connecting to server default (…, quic)` → `Tunnel established` |
| TCP | HTTP 204，0.26 s | `Connecting to server default (…, tcp)` → `Tunnel established (pooled session)` |

> 这是干净链路下的**功能验证**（证明两条路都通）；弱网下的收益仍需按上面的档位采集。
> 客户端日志现在会显式打印 `(address, tcp|quic)`，"我的切换到底生效没有"不再靠猜。
>
> 手机侧已补验：`Connecting to server default (…, quic)` 紧跟
> `Tunnel established → 142.250.199.238:443`，说明 QUIC 会话在手机上真的建起来了
> （当时 Google 打不开是 W5b，与传输无关）。验证完已把真机开关切回 TCP。

### 3. 断流容忍（W1/W2）：单测覆盖

- 内层流只在「30 s 无 ACK 进展 **且** 至少 3 轮重传」后才放弃，放弃时**发 RST + 通知 relay 停止 + 计 `flow_stall_drops`**（修掉原先的静默丢弃与泄漏）；
  `cargo test -p phantom-client --lib tun::tests::stalled_flow…` 通过（3 s 内完成，用回填进度时间戳代替真实等待）。
- 单服务器链路被判死时（健康检查连续失败达阈值）→ 主动清空 TCP/QUIC 会话池并提升网络 epoch，让 App 立刻重连；多服务器场景仍然只做迁移、不重置：
  `failover::tests::single_server_outage_fires_the_datapath_reset_once`、`multi_server_outage_migrates_without_a_datapath_reset`。
- 鸿蒙侧只有「默认网络 netId 变化」或「当前默认网络 netLost」才重建数据面，同一网络的能力/信号波动不再重置；重建带 3 s 冷却：
  真机日志形如 `net change: default 100 -> 101 (netLost); tunnel flows reset (epoch N)`。

## 不做 / 未做

- **地铁真机复核**：用户确认无法执行，弱网验收以 Mac 台架为主；台架复现的是"路径劣化"（时延/丢包/限速/断流），不覆盖射频层（RRC 状态迁移、上行调度）。
- **窗口缩放（wscale）**：按证据触发，未实施。触发条件：同一档位 trace 中某流 `inflight` 连续 ≥200 ms 顶在 App 通告窗口的 ≥95%，且该流有效下行 < 同档 SOCKS5 参照的 60%。
- **IPv6 出口**：服务器没有 IPv6，因此隧道内 IPv6 目标只能快速失败并回落 IPv4；如需要真正的 IPv6 出海，属于加 IPv6 出口的独立事项。

## 回归（硬门）

| 项 | 命令 | 结论 |
|---|---|---|
| 鸿蒙 / WiFi 两工况 | `scripts/harmony-bench.sh --label … --seconds 60` | 待采集（阈值同 `PERF_TUN_PATH_REPORT.md`） |
| Mac loopback | `scripts/speedtest.sh --loopback --rounds 3` | **PASS**：中位 **1104 MB/s**（基线区间 930–1016 MB/s），客户端软件上限无回归 |
| Mac → VPS | `scripts/speedtest.sh --uri … --origin vps --rounds 3 --check-unblock` | **PASS**：隧道中位 0.35 MB/s vs 同轮裸链路 0.34 MB/s（比值 1.03，贴着链路上限）；`google/gstatic` 204、`youtube.com` 200、`cloudflare trace ip=… colo=HKG loc=HK` |
| 单元测试 | `cargo test -p phantom-client --lib` | **90 passed, 0 failed** |
| 端到端 | `cargo test -p phantom-e2e --release` | **全绿**（含 `weak_network` 6 passed / 4 ignored、`tcp_session_pool`、`quic_mux`、`correctness`、`dns_hijack`） |
| 路由器 | 共享核心，本轮以 loopback + 单测覆盖 | 真机 A/B 待开放路由 SSH |

> 附带修掉一个让这条硬门跑不起来的脚本缺陷：`scripts/speedtest.sh` 在等待本地
> 服务端写 `server.toml` 时，`sed` 读一个还不存在的文件会返回非零，配合
> `set -e`/`pipefail` 会让脚本在第一次重试前就退出（表现为"只有标题、没有数字"）。
> 现在会先判断文件存在，并在取 URI 时兜底。
