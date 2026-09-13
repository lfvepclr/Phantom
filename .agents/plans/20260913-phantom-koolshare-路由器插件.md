# Phantom koolshare 路由器插件（RT-AX86U Pro）实现计划

- 目标机器：`http://<路由器IP>/`（RT-AX86U Pro，koolshare 官改 `3.0.0.4.388_24199_koolcenter`）
- 计划日期：2026-09-13
- 代码位置：`client/koolshare/`（与 mac / android / harmony 并列的第 5 个客户端形态）

## 1. 产品概述

为 Phantom 增加一个客户端形态：**koolshare 软件中心离线插件**（rogsoft / hnd-axhnd）。装进 `/koolshare` 后可在路由器 Web 管理界面直接管理 Phantom 透明网关，功能对齐 macOS / Android / HarmonyOS 客户端。插件以 **armv7 + aarch64 双架构静态二进制**分发，`install.sh` 按 `uname -m` 选择；同一套后台脚本保留"无软件中心"降级安装路径（`/jffs/phantom` + `services-start` / `nat-start`）。

**总体策略：不改 Rust 数据面，只加控制面外壳。** 指标走 `/metrics` 采样求差、白名单走 `PHANTOM_PROXY_DOMAINS` 文件、模式规则走生成的 `client.toml`、测速走 SOCKS5 的 `curl`（与 Android `Probe.kt` / Harmony `TunnelProbe.ets` 一样把测速放在平台壳层）。零回归风险，符合"共享核心 + 平台壳"分层。

## 2. 核心功能

| 功能 | 实现要点 |
|---|---|
| 开关与连接配置 | 总开关、`phantom://` 连接串（password 输入 + 显示切换）、模式（smart/proxy/direct）、传输协议、LAN 接口、TUN 地址、DNS 劫持 |
| 状态与上下行速度 | 实时上下行速率 + 累计流量 + 连接数 + 直连/代理分流计数，2s 刷新 |
| 白名单 | 多行域名编辑，与内置被墙域名表共同决定"默认直连、命中走隧道" |
| 定时重启 | 每日/每周定时重启，`cru` 优先、`crontab` 回退，开机重注册、卸载清理 |
| 测速 | 一键吞吐探测（`curl --socks5-hostname 127.0.0.1:1080`），验证性能上限 |
| 日志 | 页面内滚动日志（1.5s 轮询、无变化 200 次停）、一键清空、截断 500 行 |
| 皮肤 | asuswrt（灰蓝）/ ROG（红）/ TUF（橙）三套，`/* W3C rogcss */`、`/* W3C asuscss */` 标记由 install.sh 切换 |
| 安全 | 配置权限 600、日志/状态对密钥打码、真实服务器地址与密钥不入库 |

## 3. 技术栈

- 插件外壳：POSIX sh（BusyBox ash，**禁止 bash 语法**）+ HTML/jQuery（软件中心自带 `/res/softcenter.css`、`/js/jquery.js`）
- 控制面存储：`dbus`（skipd）；前端 `GET /_api/phantom` 读、`POST /_api/` 提交
- 数据面：现有 `phantom` musl 静态二进制，`client --tun --gateway` + SOCKS5 `127.0.0.1:1080` + `127.0.0.1:9150/metrics`
- 出包：`cargo xtask package koolshare`（复用 `build_router` / `build_router_armv7`），产物 `dist/phantom-<version>.tar.gz` + `.sha256`

## 4. 关键设计决策

1. **状态采集用常驻采样循环**：前端 `POST /_api/` 每次 spawn 脚本，2s 一次太重。改为 phantom 启动后拉起 `phantom_status.sh` 后台循环，每 2s 抓 `/metrics` 与上次快照求差 → 写 `/tmp/phantom_status.json`，软链到 `/www/_temp/phantom_status.txt`，前端 GET 静态文件。生命周期跟随 phantom 启停。
2. **测速口径**：gateway 模式只代理 `iif br0` 转发流量，**路由器自身发起的连接是直连的**，测速必须显式 `--socks5-hostname 127.0.0.1:1080`，否则测的是裸宽带。结果含义为"phantom 用户态转发吞吐上限"，页面注明不含 LAN→TUN 的 NAT 转发路径。
3. **URI → client.toml**：CLI 无 `--mode` 参数，config.sh 需从 `phantom://<key>@<host>:<port>?cipher=&protocol=` 拆出 `[[servers]]` 生成 TOML（先读 `core/src/uri.rs` 确认格式）。**兜底**：解析失败回退 `--server "$URI"` 命令行启动并在日志 warn。
4. **白名单不进 TOML**：dbus 不支持多行文本 → UI 用 textarea、存储用逗号分隔单行；后端拆行写入 `proxy_domains.txt` 并用 `PHANTOM_PROXY_DOMAINS` 注入，写入前做域名白字符过滤，避免注入 TOML。
5. **定时重启**：`cru` 优先、缺则 `crontab`；start/restart 幂等重注册，stop/uninstall 清理。
6. **与 `deploy/router` 的关系**：保留已有脚本（已有用户部署），控制逻辑统一到 `phantom_config.sh`，新增无软件中心降级分支，`deploy/router/README.md` 指向插件手动模式。

## 5. 目录结构

```
client/koolshare/
├── README.md                          # 架构、dbus 键表、action 契约、调试与验收清单
├── VERSION
└── phantom/                           # 离线包内容（解压后即 /tmp/phantom，module=phantom）
    ├── install.sh / uninstall.sh / version / .valid（含 hnd）
    ├── res/    icon-phantom.png、phantom.css
    ├── scripts/ phantom_config.sh  phantom_status.sh  phantom_speedtest.sh
    │            phantom_cron.sh    phantom_diag.sh    phantom_watchdog.sh
    ├── webs/   Module_phantom.asp
    ├── bin/    phantom-aarch64 / phantom-armv7（打包注入、不入库）
    └── tests/  mock 环境（stub dbus/nvram/cru）+ smoke.sh
xtask/src/main.rs / pack.rs            # package koolshare
AGENTS.md / ARCHITECTURE.md / deploy/router/README.md / .gitignore
```

## 6. 关键接口约定

- `phantom_config.sh` action：`1`=保存并应用、`2`=清日志、`3`=测速、`start`/`stop`/`restart`/`status`、`ks <0|1>`（init.d 开机入口）、`cron`、`diag`。
- dbus 键：`phantom_enable`、`phantom_uri`、`phantom_mode`、`phantom_protocol`、`phantom_lan_if`、`phantom_tun_addr`、`phantom_dns_hijack`、`phantom_whitelist`、`phantom_cron_enable`、`phantom_cron_time`、`phantom_log_level`、`phantom_last_act`、`phantom_version` + `softcenter_module_phantom_{version,install,name,title,description}`。
- 前端：读 `GET /_api/phantom`；提交 `POST /_api/` 体 `{"id":N,"method":"phantom_config.sh","params":[flag],"fields":{...}}`；日志 `GET /_temp/phantom_log.txt`；状态 `GET /_temp/phantom_status.txt`。

## 7. 页面结构（单页六区块 / 三个 tab）

1. 顶部标题条 + 版本号 + 返回软件中心 + 总开关卡（含运行状态徽标、上次动作）
2. 状态总览卡：上下行速率大数字（单位自适应）、累计流量、连接数、直连/代理分流、TUN 与策略路由状态，2s 刷新 + 0.3s 过渡色
3. 服务配置卡：连接串 / 模式 / 协议 / LAN 接口 / TUN 地址 / DNS 劫持 + 提交
4. 白名单卡：textarea + 内置表开关 + 条数统计与非法行标红
5. 运维卡：定时重启开关与时间、测速按钮与结果条、重启/停止
6. 日志卡（tab2，等宽深色、自动滚底、清空）、帮助卡（tab3）

未启用时隐藏 3/4/5 卡，只留开关与版本。

---

## 8. 调试、性能调优与修复报错（重点）

### 8.1 三档调试环境

| 档位 | 手段 | 用途 |
|---|---|---|
| L0 本地静态 | `sh -n *.sh`、`shellcheck -s sh`、JSON 片段校验 | 提交前最低防线，**不能依赖真机发现语法错** |
| L1 容器模拟 | `podman/docker run --platform linux/arm64 alpine`，`mkdir -p /koolshare /tmp/phantom /www/_temp`，注入 `client/koolshare/phantom/tests/mock-bin/` 里的 `dbus`/`nvram`/`cru`/`skipd` stub，跑 `install.sh` + `config.sh 1/start/stop` 全流程 | 无真机也能验证安装、dbus 契约、启动参数拼装、卸载清理 |
| L2 真机 | `ssh admin@<路由器IP>` | 唯一能验证 TUN、策略路由、皮肤、软件中心 API 的地方 |

macOS 本地**不能**直接跑脚本验证（BSD 用户态 + zsh 语义不同），必须进 busybox ash 容器。

### 8.2 迭代闭环（改一行脚本到看到效果）

不要每次走"打包 → 软件中心离线安装"，太慢。按改动类型选最短路径：

```bash
# 1) 只改后台脚本：单文件推送，不重装插件
scp client/koolshare/phantom/scripts/phantom_config.sh admin@<路由器IP>:/koolshare/scripts/
ssh admin@<路由器IP> 'chmod 755 /koolshare/scripts/phantom_*.sh && sh -x /koolshare/scripts/phantom_config.sh restart'

# 2) 只改页面：ASP 是静态文件，推送后浏览器 Ctrl+F5 即可（httpd 无需重启）
scp client/koolshare/phantom/webs/Module_phantom.asp admin@<路由器IP>:/koolshare/webs/
# 打开 http://<路由器IP>/Module_phantom.asp

# 3) 只换二进制
scp target/aarch64-unknown-linux-musl/release/phantom admin@<路由器IP>:/koolshare/bin/phantom-aarch64
ssh admin@<路由器IP> 'sh /koolshare/scripts/phantom_config.sh restart'

# 4) 完整重装（install.sh / res / version 变更时）
tar czf /tmp/phantom.tar.gz -C client/koolshare phantom
scp /tmp/phantom.tar.gz admin@<路由器IP>:/tmp/
ssh admin@<路由器IP> 'rm -rf /tmp/phantom && tar xzf /tmp/phantom.tar.gz -C /tmp && sh /tmp/phantom/install.sh'
```

`sh -x` 是 ash 上最强的一招：直接看 dbus 取值、参数拼装、分支走向。

### 8.3 浏览器 / 软件中心侧调试

- 直接打开 `http://<路由器IP>/_api/phantom` 看 dbus JSON（键是否写全、值是否被截断/转义）。
- F12 → Network 看 `POST /_api/` 的响应 `result` 是否等于请求 `id`（不等即脚本执行失败）；Console 看 JS 报错。
- 直接 `GET /_temp/phantom_status.txt`、`/_temp/phantom_log.txt` 验证软链是否生效。**风险点**：部分固件 `/www` 只读导致软链建不出来，install.sh 必须探测可写性并在日志告警、回退到 `/koolshare/webs/_temp/` 并在页面提示。
- 皮肤调试：直接改 `/koolshare/webs/Module_phantom.asp` 里的标记行 + 强制刷新，确认 ROG/TUF/ASUSWRT 三套主色。

### 8.4 数据面调试（不经过 UI）

```bash
# 绕过页面，直接改配置再应用（最快验证配置项是否生效）
ssh admin@<路由器IP> 'dbus set phantom_mode="proxy"; sh /koolshare/scripts/phantom_config.sh 1'

# 指标（累计值，需两次采样求差得速率）
ssh admin@<路由器IP> 'curl -s --max-time 2 http://127.0.0.1:9150/metrics | grep ^phantom'

# 路由/防火墙是否按预期装好
ssh admin@<路由器IP> 'ip rule show | grep -E "lookup (main|200)"; ip route show table 200; ip link show phantom0'

# 隧道连通性（路由器自身是直连的，必须走 SOCKS5 才测得准）
ssh admin@<路由器IP> 'curl -s -o /dev/null -w "%{http_code} %{time_total}\n" --socks5-hostname 127.0.0.1:1080 https://www.google.com/generate_204'

# 最近日志
ssh admin@<路由器IP> 'tail -n 200 /tmp/phantom.log'
```

### 8.5 性能调优

**先看数据再动手**，测量项与工具：

| 维度 | 命令/来源 | 说明 |
|---|---|---|
| 进程 CPU/内存 | `top -b -n 1`、`ps -o pid,pcpu,pmem,comm` | 判断瓶颈是不是 phantom 本身 |
| 采样循环开销 | 同上，看 `phantom_status.sh` 占比 | 若偏高把 2s 放宽到 3-5s，或改用 `wget -q -O -` |
| TUN 收发 | `ip -s link show phantom0`、`/proc/net/dev` | 核对与 metrics 是否一致（不一致说明计数或路径有漏） |
| 软中断 | `cat /proc/softirqs`（NET_RX/NET_TX） | 转发路径是否打满 |
| 吞吐上限 | `phantom_speedtest.sh`（SOCKS5 下载） | **用户态转发上限**，不含 LAN 侧 NAT |
| 真实体验 | LAN 客户端实测（Mac/iPhone 经路由器） | 与上一项对比，差值即 LAN→TUN 路径损耗 |
| TUN 健康度 | `tcp_dup` / `dup_acks` / `tun_wq_max_ms` / `tun_txq_peak` / `retx_budget_rst` / `route_direct_failed` | **现状问题：这些只在 `snapshot_json()` 里，Prometheus 未暴露** |

**已知待办**：`stats.rs::render_prometheus()` 目前只暴露 9 个计数器，不含 TUN 重传/写队列/直连失败指标。排查"隧道正常但视频卡"时缺这些就看不了。建议在调试阶段按需补齐（改 `render_prometheus` + 同步 `ARCHITECTURE.md` §5 与 README 指标清单），或先让 `phantom_status.sh` 读日志侧写。

优化手段（按性价比排序）：

1. 采样周期与日志级别是最大变量：`RUST_LOG=info`（debug 会显著吃掉 CPU）；
2. `cipher`：BCM4912 有 ARM CE，`cipher=auto` 应命中 AES-256-GCM，确认日志里没退化到 ChaCha20/Ascon；
3. 绑定大核 / 限制 tokio 线程数（若暴露相关参数），避免与 httpd、dnsmasq 抢核；
4. TCP 窗口与 `net_tune.rs` 相关内核参数（参考 `client/src/net_tune.rs` 与 `README.md` §7 性能调优）；
5. 每台机器记录基线：结果写入 `tests/PERF_ROUTER_REPORT.md`（与已有 `tests/PERF_TUN_PATH_REPORT.md` 同构），含固件版本、内核、CPU、吞吐、CPU 占用。

### 8.6 报错修复闭环

1. **日志分级开关**：dbus `phantom_log_level`（error/info/debug），应用后重启生效；日志**必须截断**（最近 500 行 / 上限 256KB），JFFS 写满会拖垮整台路由器。
2. **看门狗** `phantom_watchdog.sh`：每 5 分钟由 `cru` 触发，进程不在且 `phantom_enable=1` 则拉起并写日志；连续失败次数写 dbus，页面可见。
3. **崩溃残留清理**：stop 先 SIGTERM（让 Drop 回滚 ip rule/iptables），超时再 `cleanup_rules()`（沿用 `deploy/router/phantom.sh` 逻辑）。
4. **nat-start 兜底**：Asuswrt 重建 iptables 会丢规则，`nat-start` 钩子必须重启 phantom 补规则。
5. **一键诊断** `phantom_diag.sh`：无参数输出到 stdout，也可 `diag > /tmp/phantom_diag.txt` 由页面下载/复制。采集：固件与内核（`nvram get productid/odmpid/extendno/buildno`、`uname -a`）、dbus `phantom_*`（**URI 与白名单脱敏**）、进程与 CPU、`ip rule/route/link`、`iptables -t nat/mangle -S` 摘要、`/proc/net/dev`、两次 metrics 采样（间隔 3s，得速率）、最近 200 行日志、cru 条目、`df -h /koolshare /jffs`。
6. **回传开发机**：`ssh admin@<路由器IP> 'sh /koolshare/scripts/phantom_config.sh diag' > /tmp/phantom_diag.txt`，直接贴给分析方。

### 8.7 真机验收清单

- [ ] 软件中心 → 离线安装 `phantom.tar.gz` 成功（`.valid` 校验通过）
- [ ] 平台检测：非 hnd/axhnd 固件 / 无 `/usr/bin/skipd` 时拒绝安装并给出可读提示
- [ ] 双架构：同包在 aarch64 与 armv7 机型上都能选中正确二进制并启动
- [ ] 开关：开→隧道起来、LAN 客户端能走隧道；关→规则与进程完全回退
- [ ] 状态卡 2s 刷新，速率与累计流量和 `/metrics` 对得上
- [ ] 白名单：加一条域名后该域名走隧道、其余仍直连
- [ ] 定时重启：`cru l` 能看到条目且到点确实重启；关闭后条目消失
- [ ] 测速：结果合理（与 LAN 客户端实测同量级），页面注明口径
- [ ] 日志：滚动、清空、截断生效，不无限增长
- [ ] 三皮肤：ASUSWRT / ROG / TUF 主色正确
- [ ] 重启路由器后自动拉起；`nat-start` 后规则补回
- [ ] 卸载：`/koolshare` 下文件、init.d 软链、cru 条目、dbus 键全部清理

## 9. 实施顺序

1. 契约核对（URI / CLI 参数 / client.toml 字段 / gateway 回滚逻辑）
2. 插件骨架：install.sh、uninstall.sh、version、.valid、res
3. `phantom_config.sh` 总控：dbus 契约、双架构选择、client.toml 与白名单生成、起停与清理
4. `phantom_status.sh` / `phantom_speedtest.sh` / `phantom_cron.sh` / `phantom_diag.sh` / `phantom_watchdog.sh`
5. `Module_phantom.asp`（六区块 + 三皮肤）
6. `cargo xtask package koolshare`（双架构构建 + 组装 tar.gz + sha256）
7. 无软件中心降级安装路径 + 更新 `deploy/router/README.md`
8. 文档（`client/koolshare/README.md`、`AGENTS.md` 地图、`ARCHITECTURE.md` 章节）+ 容器 mock 冒烟测试 + 真机验收

## 10. 风险

| 风险 | 缓解 |
|---|---|
| `/www/_temp` 不可写导致状态/日志读不到 | install.sh 探测可写性，回退 `/koolshare/webs/_temp/` 并页面提示 |
| armv7 二进制在 aarch64 官改上跑不起来（内核未开 CONFIG_COMPAT） | 双架构 + install.sh 按 `uname -m` 选；真机首验项 |
| dbus 不支持多行文本导致白名单丢内容 | 逗号分隔单行存储 + 白字符过滤 |
| 日志/状态写满 JFFS | 截断 500 行 + 大小上限 + watchdog 巡检 |
| 测速口径被误解为"网速" | 页面明确写明"phantom 转发吞吐上限，不含 LAN 侧路径" |
| 密钥/真实服务器地址入库 | `.gitignore` 忽略 `bin/` 与生成物，诊断脚本默认脱敏 |

---

## 11. 真机结论（2026-09-13 首次部署后补齐）

首次用 SSH 手工部署（`ssh -p <SSH端口> admin@<路由器IP>`）后做只读核查，实测事实修正了本
计划里的若干假设；对应修复见
[20260913-phantom-koolshare-插件真机可用与软件中心提交卡死修复.md](20260913-phantom-koolshare-插件真机可用与软件中心提交卡死修复.md)。

### 11.1 环境实测值

| 项 | 计划假设 | 实测 |
|---|---|---|
| 内核 | 4.1.x | **4.19.183**（aarch64 kernel + 32 位 ARM userspace，静态 aarch64 二进制可跑） |
| SSH 端口 | 22 | **<SSH端口>**（`-p` 必须在 host 之前） |
| `/www/_temp` | 可能不存在 | **不存在**，且 `/www` 只读；更关键的是 docroot 软链方案本身不可行（见 11.3） |
| `command -v` | 不可用 | 确认不可用（`/bin/sh: command: not found`），live-check 自身也曾误报 |
| `cru` | 可能在 | `/usr/sbin/cru` **存在**（早期 live-check 误报缺失） |

### 11.2 软件中心 POST 契约（关键）

`POST /_api/ {id, method, params, fields}` → httpdb（监听 `127.0.0.1:3030`）先把 `fields`
写进 dbus，再执行 `/koolshare/scripts/<method> <id> <params...>`；脚本必须 POST 回
`/_resp/<id>`（body=id，即 `base.sh` 的 `http_response`）它才结束那个 HTTP 请求。
**$1 是请求 id、action 在 $2** —— 证据：`/rom/etc/koolshare/scripts/ks_app_install.sh`
（`case $2 in` + `http_response $1`）、`clash_downyamlsel.sh`（`case $2 in`）、
`clash_getbasicyaml.sh`（params 为空仍 `http_response $1`）。

原实现按 `$1=action` 解析且从不回包，于是「点提交」表现为：fields 已进 dbus、脚本主体没跑、
日志 0 字节，页面转圈一阵后弹「后台执行失败，请看日志页」。

### 11.3 状态/日志通道

`GET /phantom_status.txt` 与固件自带的 `GET /Lang_Hdr.txt` 同为 **404** —— 本固件 httpd
**不服务 docroot 下的 `.txt`**（`.asp`/`.js`/`.css`/`.png` 正常），与权限、软链无关。
可用通道是 httpdb 的 `/_temp/<name>`，映射 **`/tmp/upload/<name>`**（证据：
`ks_tar_install.sh` 写 `/tmp/upload/soft_log.txt`、`Module_Softsetting.asp` 读
`/_temp/soft_log.txt`；fancyss 的 `webtest.txt` / `ss_log.txt` 同构）。

### 11.4 钩子参数

`S*` 由 `/koolshare/bin/ks-wan-start.sh` 以 `start` 调用（来自 wan-start），
`N*` 由 `ks-nat-start.sh` 以 `start_nat` 调用（来自 nat-start），`V*` 由
`ks-services-start.sh` 调用 —— **`ks <0|1>` 入口在真机上不会被调用**，只保留兼容。
原实现缺 `N*` 钩子：Asuswrt 重建 iptables 后转发/DNAT 规则丢失而进程还在，
表现为「插件显示运行中、LAN 却断」。

### 11.5 另外两个真机才暴露的问题

| 问题 | 现象 | 修复 |
|---|---|---|
| 固件自带 curl 是 `--disable-proxy` 编译 | 测速一律 `http=000` 失败（`proxy support is disabled in this libcurl`），而本机 metrics 那种不走代理的请求照常可用 | `install.sh` 改为「按 SOCKS 真调一次」探测，优先 `/koolshare/bin/curl-fancyss`，写进 `PHANTOM_CURL_SOCKS`；探测必须不加 `-s`（否则连错误文案都被静音，探测永远成功） |
| 测速百分比单位错 | 真机打印「本次达到 **786%**」 | `speed`（bytes/s）与「服务端上行 Mbps」都换算成 kbps 再比 |

### 11.6 首次真机验收结果（2026-09-13）

- 安装：`/bin/sh /tmp/phantom/install.sh` 干净通过（含 S98/N98 钩子、`/tmp/upload` 布局）
- 页面：点提交不再卡、无「后台执行失败」；状态卡 2s 刷新、日志页有内容（`/_temp/`）
- 数据面：`ip rule` 9040/9050 + table 200 + `phantom0` 就位；LAN 客户端命中白名单走隧道
  （`route www.youtube.com:53 -> Proxy (dns tunnel)` / `-> Proxy (whitelist)`）
- 测速：359–380 KB/s（服务端上行 3 Mbps ≈ 375 KB/s，达 96–103%），phantom 满速下载时 CPU 0.5–1.0%
- 基线数据见 [`tests/PERF_ROUTER_REPORT.md`](../../tests/PERF_ROUTER_REPORT.md) §3
