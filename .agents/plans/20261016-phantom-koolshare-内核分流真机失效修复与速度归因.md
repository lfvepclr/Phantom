# koolshare 插件：内核分流真机失效修复与速度归因（服务端 3 Mbps 天花板）

> 起因：用户报「插件里有没有异常」「VPN 很慢，YouTube 看不了 720p，某些站点也很慢」「手机上打开也慢」。
> 本文是 2026-10-16 晚高峰（23:14–23:25）真机只读体检 + 带宽实测的结论与本轮修复方案。
> 全部命令只读（含在**独立测试链**里做能力探针，不触碰在跑流量）；唯一一次写操作事故记录见 §1.5。

## 一、体检结论

### 1.1 最大异常：**内核分流在这台固件上从来没有生效过**

日志（`/tmp/upload/phantom_log.txt`）每次启动都有同一段：

```
ERROR phantom_client::gateway: Gateway install failed at `iptables -t mangle -I PREROUTING
      -i br0 -m conntrack --ctstate NEW -m set --match-set phantom_proxy dst
      -j MARK --set-mark 1 -j CONNMARK --save-mark`:
      iptables v1.4.12.2: multiple -j flags not allowed
 WARN phantom_client::gateway: kernel-split gateway unavailable …; falling back to user-space relay mode
```

「一条规则里串两个 `-j`」是 **iptables ≥ 1.6** 才支持的写法；本机 `iptables v1.4.12.2` 直接拒绝。
失败发生在**第一条标记规则**上 → 整个 kernel-split 被判不可用 → 自动回退 relay。

三条现场指纹（同时满足即可确诊）：

| 检查 | 现状（回退中） | 内核分流生效时 |
|---|---|---|
| `ip rule show` | 只剩 `9050: from all iif br0 lookup 200` | 多一条 `fwmark 0x1 lookup 200`，**没有**通配 iif 规则 |
| `iptables -t mangle -S PREROUTING` | 空的 | `… --match-set phantom_proxy dst -j MARK …` 两条 |
| 状态文件 `gw` / 页面「生效模式」 | `relay` / 兼容模式 | `kernel-split` / 内核分流 |

**后果**：LAN 的**全部**流量（包括国内直连）都被塞进 TUN，由 phantom 在用户态逐包终结重发，
Broadcom Runner / Flow Cache 完全用不上 —— 这正是「非 VPN 的网站、视频也慢、CPU 还高」的成因。

**能力探针（在独立 mangle 链 + 临时 ipset 里做，不碰在跑流量）**：

| 能力 | 结果 |
|---|---|
| `ipset create/add/list/destroy` | ✅ rc=0 |
| `iptables -m set --match-set … -j MARK --set-mark 1` | ✅ rc=0 |
| `iptables -m conntrack --ctstate NEW … -j CONNMARK --save-mark` | ✅ rc=0 |
| 一条规则串两个 `-j` | ❌ `multiple -j flags not allowed` |

→ **拆成两条同匹配的规则即可让内核分流在这台固件上跑起来**（本轮实施）。

### 1.2 速度归因：VPN 慢 = **服务端 3 Mbps 上行**，不是路由器

同一个 URL（`dl.google.com`，在分流白名单里）两条路径对照：

| 路径 | 实测 | 说明 |
|---|---|---|
| 经隧道（`--socks5-hostname 127.0.0.1:1080`） | **324 KB/s ≈ 2.6 Mbps** | 服务端上行 3 Mbps 的 ~87%，链路健康 |
| 同 URL 直连（路由器本机） | 72 MB/s ≈ 577 Mbps | 本线路对 Google CDN 有就近缓存 |
| 国内站点经路由器（Mac / 5G WiFi） | 10.7 MB/s ≈ 86 Mbps | 直连路径正常 |

**结论**：必须在隧道里走的外网站点，天花板就是服务端 3 Mbps 上行 ≈ 375 KB/s。
YouTube 720p 约需 2.5 Mbps 且启动阶段要有突发余量 —— 这条管道结构性做不到，
**改客户端任何设置都无效**。要稳定 720p 得换一台上行 ≥ 20 Mbps 的服务器。

旁证：隧道域名走的是远端解析（`redirector.googlevideo.com` 在本地上游 DNS 被解析成
Facebook 的 IP，典型 DNS 污染），所以 YouTube 无法靠"改本地 DNS"绕开隧道。

### 1.3 无线与手机

- 2.4G（eth6）：`txretrans` 峰值 **1.7 万/5 分钟**、`rxcrc` 2–3 千；5G（eth7）同口径明显更轻。
- 关联分布：2.4G 7 台、5G 3 台（Mac 在 5G，rssi −75）。**手机若挂在 2.4G，慢在这里**。
- `eth2` 只协商到 100 Mb/s（对端只广告 10/100 模式），挂在这口上的设备上限就是 100 Mb/s。

### 1.4 其它日志异常（不致命）

1. 每次启动都有 `WARN failover: Config reload: server list is empty, keeping the previous pool` ——
   `--server` 传入的地址不进 `servers` 段导致，属噪音，可后续消音。
2. 开机到 NTP 同步前，插件日志时间戳是 `2018-05-05`（固件 RTC 未同步），易误读。
3. 采样日志落在 `/tmp/upload/phantom_perf.log`，**重启即丢**（本次重启把整晚 45 行采样带走了）。

### 1.5 本轮事故记录（必须留痕）

23:16 整机重启，原因：内核 `out_of_memory` → `Kernel panic - not syncing: Out of memory:
system-wide panic_on_oom is enabled`（victim `avahi-daemon`，机型 RTAX86U_PRO，内核 4.19.183）。

- **诱因是本次体检的操作失误**：在真机上跑了一条**无上界**的上传测试
  （`curl --data-binary @/dev/zero` 经隧道无限灌数据），紧接着整机重启。
- 重启前 phantom RSS 仅 5 MB、系统内存 1 GB，事后无内存历史可查（采样器没有内存列）。
- 路由器由 nat 钩子自愈（`已启动 10-16 23:17:23（nat）`），WiFi 与插件均恢复。
- **教训**：真机只跑有界测试（`-m` + 固定字节数）；采样器补 `rss_kb` / `mem_free_kb` 两列。

## 二、本轮改动

### 2.1 数据面：内核分流规则拆成两条（`client/src/gateway.rs`）

- 安装计划把 `… -j MARK --set-mark 1 -j CONNMARK --save-mark` 拆成两条同匹配规则；
  **顺序**：`-j MARK` 在前、`-j CONNMARK --save-mark` 在后（save-mark 存的是当前包的 mark）。
  因 `-I` 是前插，计划里先下发 save-mark。
- 最终链顺序不变：`restore-mark → DNS(53) → MARK → save-mark`（语义与修复前完全一致）。
- teardown **三个形态全删**：新的两条 + 老版本那条串两个 `-j` 的（升级不留残渣）。
- 新增/更新单测：`plan()` 里任何规则不得出现两个 `-j`（防回归）、MARK 与 save-mark 是两条规则、
  计划顺序（save-mark 先下发）、teardown 三种形态都删。

### 2.2 页面：回退必须看得见（`webs/Module_phantom.asp`，rev 11 → 12）

- 新增 `requested_mode()` 读下拉框值；当**填的是内核分流、实际 `gw` 是 relay** 时，
  顶部直接出告警：「分流没生效：填的是内核分流，实际跑的是兼容模式（全部流量都走用户态转发…）」。
- 与用户主动选择「兼容模式」区分开（后者不告警）。

### 2.3 采样：补内存两列（`scripts/phantom_perf.sh`，表头 ver 2 → 3）

- 追加 `rss_kb`（phantom 常驻内存）与 `mem_free_kb`（MemAvailable）：下次再 OOM 才有据可查。

### 2.4 测试与文档

- `tests/smoke.sh`：新增 5 条断言（页面回退告警三处标记 + 采样表头两列），现 **194 通过 / 0 失败**。
- `client/koolshare/README.md` §7.0.1 增「iptables 1.4 拒绝多 `-j`」踩坑记录；§7.1 增真机基线表（本次实测数字）。
- 本文档。

## 三、接口与默认值

- **无新增** dbus 键 / CLI 参数 / HTTP 通道 / 指标；`phantom://` URI、分流语义、ipset 名（`phantom_proxy`）、
  fwmark（0x1）、表号（200）全部不变。
- 采样日志新增两列属向后兼容变更（分析按表头名取值，旧行仍可读）。

## 四、测试与验收

### 已完成（本机）

- L0：`for f in install.sh uninstall.sh scripts/*.sh; do sh -n "$f"; done` → 通过。
- `cargo check --tests --target aarch64-unknown-linux-musl -p phantom-client` → 通过
  （gateway 单测是 `#[cfg(test)]` 但模块被 `cfg(target_os = "linux")` 门控，**macOS 上跑不到**，
  这条记录为已知限制；本机 Podman 的 Linux VM 起不来，无法就地跑）。
- L1：`PHANTOM_SMOKE_ROOT=$(mktemp -d)/ks sh client/koolshare/phantom/tests/smoke.sh` → **194/0**。
- 出包：`cargo xtask package koolshare`（aarch64 + armv7 双架构）。

### 待真机验收（部署新包后）

1. 页面显示 rev 12；「生效模式」应为**内核分流**、`ipset` 条目数 > 0、顶部**无**回退告警。
2. `ip rule show` 出现 `fwmark 0x1 lookup 200` 且**没有**通配 `iif br0`；
   `iptables -t mangle -S PREROUTING | grep -c match-set` = 2。
3. 直连快路径：Mac → `dl.google.com` 直连（不经隧道）应明显高于 10 MB/s；同 URL 经隧道仍 ~324 KB/s。
4. 隧道站点行为不变：Google 204、Telegram、GitHub/JetBrains 白名单仍走隧道；
   关闭开关后进程 / ip rule / iptables / ipset 全部回退。
5. 采样日志出现 `rss_kb` / `mem_free_kb` 两列。

## 五、后续（未做，需要决定）

- **真正的 720p**：服务端上行 ≥ 20 Mbps。当前 3 Mbps 是硬上限。
- **可实验**：本线路对 Google CDN 直连实测 577 Mbps —— 若把 `googlevideo` 类 CDN 做成
  「DNS 走隧道拿真实 IP + 连接走直连」，YouTube 有可能绕开 3 Mbps 天花板（未验证，需单独一轮）。
- 2.4G 空口整治（信道/带宽/客户端迁移到 5G）与 `eth2` 的 100 Mb/s 归属确认。
- `failover: server list is empty` 噪音消音；采样日志跨重启留存（写 `/jffs` 或开机转存）。
