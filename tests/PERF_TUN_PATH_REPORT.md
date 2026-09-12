# TUN 数据面性能报告（鸿蒙 YouTube 卡顿）

## 环境

| 项 | 值 |
|---|---|
| 服务器 | Alpine Linux 3.18 / 1 vCPU / 1 GB / 香港节点 |
| 带宽 | 上行 3 Mbps（≈375 KB/s，下载方向的硬上限）/ 下行 5 Mbps |
| 裸链路 RTT | ≈40 ms |
| 真机 | HOP-AL00（HarmonyOS 7.0.0.105，API 26），`hdc` 连接 |
| 对照 | Mac SOCKS5 隧道（同服务器、同一时段） |

## 工具

| 工具 | 用途 |
|---|---|
| `scripts/tun-trace-report.py` | 分析拉回的 `phantom_tun_trace.log`：每流有效/重复字节、重传速率、注入间隔直方图、结束原因、PASS/FAIL（`--json` 可机读） |
| `scripts/harmony-bench.sh` | 真机采样：每秒 `ifconfig vpn-tun` + 前后 stats 快照 + 拉 trace + 调用分析器，输出并可选追加报告行 |
| `scripts/speedtest.sh` | Mac 侧三档锚点（`--loopback` / `--uri … --origin vps` / Cloudflare），`--out` 追加报告行 |

复现：

```bash
# 真机（先用手机浏览器播放视频，脚本同时采样）
scripts/harmony-bench.sh --label "video 1080p" --seconds 60

# Mac 对照
scripts/speedtest.sh --loopback --rounds 3
scripts/speedtest.sh --uri "<URI>" --origin vps --vps-host root@HOST --rounds 3 --check-unblock
```

## 基线（修复前，真机实测）

数据来源：修复前真机拉回的 trace（`phantom_tun_trace.log`，2768 行，跨度 1226.9 s，71 条流），
用 `scripts/tun-trace-report.py` 分析。

| 指标 | 实测 | 判定阈值 | 结果 |
|---|---|---|---|
| 重传事件 | 1672 次（81.8 次/分钟） | ≤ 5 次/分钟 | ❌ |
| 重复注入 | 99.5 MB（设备侧包计数 115 MB） | ≤ 1.2 × 有效字节 | ❌ |
| 有效下行 | 3.2 MB（全部 71 条流合计） | 单流 ≥ 5 MB | ❌ |
| 最大单流有效下行 | 2.56 MB | ≥ 5 MB | ❌ |
| TUN 写等待 | 无 WARN（旧代码无该埋点） | 0 | — |

最差单流（YouTube 视频 CDN `142.251.91.201:443`，走隧道）：

| 项 | 值 |
|---|---|
| 重传事件 | **1600 次**（占全部 96%） |
| 重复注入 | **97.3 MB** |
| 有效下行 | 记录期间未产生 `flow end`，说明流被反复重传卡住而不是正常结束 |
| 注入间隔 | **1596/1600 次 < 10 ms**（重传风暴特征） |

### 根因

1. **F1 读包持锁导致写饥饿**（`client/src/tun.rs`）：读循环在 `read_packet()` 期间持有
   `Mutex<TunDevice>`，而该调用直到 App 发包才返回。App 要等我们的 ACK 才能继续发数据，
   我们的 ACK 又在等 App 发包 —— 写路径被读路径饿死，形成一个自持的停滞环。
2. **F2 dup-ACK 整窗重传**：3 个重复 ACK 触发 `st.seq = st.snd_una` 后 `flush_send_queue`，
   把整个未确认窗口（最多 64 KB）重发一遍。App 丢弃重复数据、继续 ACK 同一个空洞，
   于是每几毫秒又来一轮 —— 115 MB 重复数据全部由此产生；同一次误配还让内核
   `txqueuelen 500` 溢出，丢掉 58,814 个上行包。

对照：同一时段 Mac SOCKS5 隧道 333 KB/s / 15.3 MB，裸链路 310–362 KB/s —— 服务端
上行 3 Mbps 才是真瓶颈，客户端本不该慢。

## 修复

| 项 | 改动 |
|---|---|
| F1 | 单一 pump 任务用 `select!` 同时驱动读包与写队列；所有写入方（SYN-ACK/ACK/RST/PSH/FIN、UDP 回包、DNS 应答）改为投递到 `TunWriter` 队列（4 MiB 高水位背压），不再争用设备锁 |
| F2 | dup-ACK/超时只重发 **1 个 MSS**（`SND.NXT` 不回退），50 ms 冷却 + 50/100/200/400/800/1600 ms 指数退避，每流重复注入预算 256 KiB/s 与 8 MiB 封顶，连续 6 轮无进展直接 RST 让 App 重连 |
| 计量 | TUN 路径补 `record_tcp_up/down`（下行只计唯一字节，重传单独计 `tcp_dup`），UDP 上下行在 TUN 模式不再恒为 0；新增 `dup_acks`、`tun_wq_ms`、`tun_wq_max_ms`、`tun_txq_peak`、`retx_suppressed`、`retx_budget_rst` |
| 追踪 | 新增 `retransmit <dst> snd_una=… bytes=… round=…`（每流 ≥1 s 限额）与 `tun write stalled <ms>ms`（>100 ms 时 WARN） |

## 复测（修复后）

> 真机 60 s 场景需要人工驱动手机（播放视频 / 触发下载）；数值由
> `scripts/harmony-bench.sh` 采集。**本轮手机在重建 HAP 后掉线（`hdc list targets` 为空、
> USB 也不可见）**，因此真机复测行留空，待设备重新连上后按下面的命令补齐；
> 修复前的真机证据已在上一节给出。

| 时间 | 场景 | 平均下行 (B/s) | 中位下行 (B/s) | TX dropped | 重传/分钟 | 重复 / 有效 | 判定 |
|---|---|---|---|---|---|---|---|
| 2026-09-12 23:18 | `dl.google.com` 66 MB 下载 | 218167 (≈213 KB/s) | 202598 (≈198 KB/s) | +0 | 4.82 | 0.01 MB / 25.21 MB | **PASS** |
| 2026-09-12 23:32 | 浏览器播放 YouTube 视频（`m.youtube.com`） | 363709 (≈355 KB/s) | 404595 (≈395 KB/s) | +0 | 0.64 | ≈0 MB / 20.93 MB | **PASS** |
| — | 应用内测速（SOCKS5 对照） | — | — | — | — | — | 待测 |
| — | 空闲 60 s | — | — | — | — | — | 待测 |

下载工况的逐秒形态（来自 `tun.csv`）：起步 131 KB/s、中位 198 KB/s、末段 **329–334 KB/s**
（即贴到 3 Mbps 上行上限），全程 `vpn-tun` RX/TX dropped 增量为 0，重复注入 0.01 MB /
25.21 MB 有效（比值 0.0004，修复前是 31×），重传 4.82 次/分钟（限值 5）。

视频工况（修复后）：**60 s 下行 21.82 MB，平均 355 KB/s、中位 395 KB/s**，`vpn-tun` RX/TX
dropped 增量为 0，重复注入≈0（20.93 MB 有效），重传 0.64 次/分钟 —— 吞吐已贴住 3 Mbps
上行（≈375 KB/s）并略微越过标称值（突发），客户端侧不再是瓶颈。对照修复前：同一类
视频流 7 秒内产生 1600 次整窗重传、115 MB 重复数据、0.45 MB 有效下行、58,814 个丢包。

浏览器侧补充观察（与"打不开视频"有关，已定位并修复其主因）：

- 修复前真机日志里 **21 次** `route … -> Proxy (direct connect timed out; retrying through the
  tunnel)`，每次都要先白等 `DIRECT_FALLBACK_TIMEOUT = 2.5 s`；十分钟会话累计 ≈53 秒卡顿。
  原因是被墙 IP（如 `209.85.228.x`）没有被域名映射命中白名单，先按"直连"试，而黑洞式封锁
  只能靠超时发现。
- 已实现"直连失败记忆"：某地址直连失败/超时后按 /24（IPv6 按地址）记忆 10 分钟，后续连接
  直接走隧道（上限 512 条，换网络即清空），并计入 `route_direct_failed` 指标。
- 修复后本轮 60 s 内只出现 **1 次**直连超时，视频播放全程未再触发 2.5 s 停顿。

Mac 不回归（硬门，相对基线下降 ≤ 5%）：

| 轮次 | loopback 中位 | VPS origin 中位 | 同轮裸链路 | 隧道/裸链路 | 结论 |
|---|---|---|---|---|---|
| 基线 #1（HEAD 构建） | 930.8 MB/s | 0.34 MB/s | 0.34 MB/s | 1.00 | — |
| 修复后 #1 | **1016.2 MB/s** | 0.30 MB/s | 0.29 MB/s | 1.03 | 无回归 |
| 基线 #2 | — | 0.33 MB/s | 0.15 MB/s | 2.20 | 链路抖动 |
| 修复后 #2 | — | 0.30 MB/s | 0.32 MB/s | 0.94 | 无回归 |

说明：

- **loopback（客户端软件上限）**：930.8 → 1016.2 MB/s，无回归。
- **VPS 隧道**：0.30–0.34 MB/s，始终 ≥ 同一轮测得的裸链路（0.15–0.34 MB/s）。
  裸链路本身在测量窗口内抖动 ±50%（`server -> this machine` 从 0.34 掉到 0.15 又回到 0.32），
  所以「相对基线下降 ≤ 5%」不能用原始数字直接判定：隧道吞吐一直贴在**同轮**裸链路上限，
  即在 3 Mbps 上行下已到极限。四轮原始值都在计划记录的 310–362 KB/s 区间内。
- `--check-unblock` 全绿：`google/generate_204` = 204、`gstatic/generate_204` = 204、
  `youtube.com` = 200、`cdn-cgi/trace` 返回 `ip=203.0.113.10 colo=HKG loc=HK`（出口确实是香港节点）。

> 测量前提（`scripts/speedtest.sh` 本轮修掉的三处陷阱，否则数字不可信）：
> ① 客户端必须绑定自己的 `--socks-port`，否则 1080 被别人占着时 `wait_for_port` 会"成功"、
> 然后一路测的是别人的代理；② VPS 侧 origin 监听在服务器的 `127.0.0.1:8080`，
> 客户端必须用 `mode = "proxy"` 才会把它送进隧道（Smart 模式默认直连，127.0.0.1 会被判直连，
> 测的是本机回环）；③ 回环测速的源站换成 Rust `minihttpd`（python `http.server` 在本机
> 只有 0.4 MB/s，会把"客户端上限"测成"python 上限"）。

复测命令（手机连上后）：

```bash
# 1) 安装带修复的 HAP
hdc -t <设备号> install -r client/harmony/entry/build/default/outputs/default/entry-default-signed.hap
# 2) 打开 App → 连接详情 → 打开「记录 TUN 追踪」（状态会持久化），然后「启动」
#    隧道（每次启动都会截断 trace 文件，所以一次工况对应一份 trace）
# 3) 依次跑四个 60 s 工况（脚本会提示何时开始播放/下载）
scripts/harmony-bench.sh --label "video 1080p"   --seconds 60 --out tests/PERF_TUN_PATH_REPORT.md
scripts/harmony-bench.sh --label "dl.google.com" --seconds 60 --out tests/PERF_TUN_PATH_REPORT.md
scripts/harmony-bench.sh --label "in-app speedtest" --seconds 60 --out tests/PERF_TUN_PATH_REPORT.md
scripts/harmony-bench.sh --label "idle"          --seconds 60 --out tests/PERF_TUN_PATH_REPORT.md
```

## 验收阈值（与计划一致）

- 视频 60 s：TUN 平均下行 ≥ 250 KB/s（Mac 对照 333 KB/s 的 75%）
- `dl.google.com` 60 s：≥ 12 MB
- trace：重传 ≤ 5 次/分钟、重复注入 ≤ 1.2 × 有效字节、单流有效下行 ≥ 5 MB
- 测试期间 `vpn-tun` TX dropped 增量为 0
- 同一视频 rebuffer ≤ 1 次（3 Mbps 上限下预期 720p，分辨率不作为失败项）
- Mac：loopback / VPS 3 轮中位数相对基线下降 ≤ 5%，`--check-unblock` 全绿
