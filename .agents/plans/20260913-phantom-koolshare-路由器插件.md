# Phantom koolshare 路由器插件（RT-AX86U Pro）

日期：2026-09-13（2026-10-04 更新：内核分流与夜间性能优化另见
[20261004-phantom-路由器内核分流-直连回内核快路径与夜间性能取证.md](20261004-phantom-路由器内核分流-直连回内核快路径与夜间性能取证.md)）
范围：`client/koolshare/`（koolshare 软件中心离线插件，Phantom 的第 5 个客户端形态）+ `cargo xtask package koolshare` 打包 + 相关文档；
本轮的边界是**不动 Rust 数据面**，只做控制面外壳（数据面改动在 10-04 那一轮）。
目标机：ASUS RT-AX86U Pro（BCM4912，koolshare 官改 `3.0.0.4.388_24199_koolcenter`），`<路由器IP>`。
调试、性能、报错与验收的可操作清单已落在 [`client/koolshare/README.md`](../../client/koolshare/README.md) §6–§9，本文只留决策与结论。

## 目标

为 Phantom 增加一个客户端形态：**koolshare 软件中心离线插件**（rogsoft / hnd-axhnd）。装进 `/koolshare`
后可在路由器 Web 管理界面直接管理 Phantom 透明网关，功能对齐 macOS / Android / HarmonyOS 客户端。

| 功能 | 实现要点 |
|---|---|
| 开关与连接配置 | 总开关、`phantom://` 连接串（password + 显示切换）、模式 smart/proxy/direct、传输协议、LAN 接口、TUN 名称/地址、路由表、DNS 劫持 |
| 状态与速率 | 上下行速率 + 累计流量 + 连接数 + 直连/走隧道计数，页面 2s 刷新（采样循环 3s） |
| 白名单 | 自定义域名（textarea，逗号分隔存 dbus）+ 内置被墙域名表开关；写 `proxy_domains.txt`，`PHANTOM_PROXY_DOMAINS` 注入 |
| 定时重启 / 看门狗 | `cru` 优先、`crontab` 回退；开机重注册、卸载清理；看门狗每 5 分钟巡检 |
| 测速 | 经 SOCKS5 下载样本，给出「达到服务端带宽上限百分之多少」的结论 |
| 日志 | 页面 1.5s 轮询、一键清空、500 行 / 256KB 截断（看门狗每 5 分钟兜底裁剪） |
| 皮肤 | ASUSWRT / ROG / TUF 三套，`/* W3C rogcss */`、`/* W3C asuscss */` 标记由 `install.sh` 切换 |
| 分发 | aarch64 + armv7 双架构静态二进制，`install.sh` 按 `uname -m` 选；无软件中心自动降级 `/jffs/phantom` |
| 安全 | 配置 600、日志与状态对密钥/URI 打码、真实地址与密钥不入库 |

技术选型：插件外壳 POSIX sh（BusyBox ash，禁 bash 语法）+ 软件中心自带 jQuery/ASP；配置存 `dbus`（skipd）；
数据面复用现有 `phantom client --tun --gateway`，指标取 `127.0.0.1:9150/metrics`，测速走 `127.0.0.1:1080`。

## 结论（实施记录）

### 根因

前提事实（真机实测，修正了计划里的假设）：内核 **4.19.183**（aarch64 kernel + 32 位 ARM userspace，
静态 aarch64 二进制可跑）；`/www/_temp` **不存在且 `/www` 只读**；busybox **没有 `command -v`/`type`**；
`/usr/sbin/sh` 是 Broadcom `memaccess`（不是 shell，必须写 `/bin/sh`）；`cru` 在 `/usr/sbin/cru`。

| 编号 | 缺陷 | 证据 / 现象 |
|---|---|---|
| K1 | **软件中心 POST 契约理解错**：`POST /_api/ {id,method,params,fields}` 是 httpdb 先落 dbus，再执行 `/koolshare/scripts/<method> <id> <params...>` —— **`$1` 是请求 id、action 在 `$2`**；原实现按 `$1=action` 解析 | `ks_app_install.sh`（`case $2 in` + `http_response $1`）、`clash_downyamlsel.sh` 同款。现场：dbus 里 `phantom_enable=1` 已写入，但日志 0 字节、`last_act` 停在「安装完成」——脚本主体从未执行 |
| K2 | **脚本从不回包**：必须 POST `127.0.0.1:3030/_resp/<id>`（body=id）httpdb 才结束请求 | `base.sh` 的 `http_response()` 即此；不回包页面一直转圈，最后弹「后台执行失败，请看日志页」 |
| K3 | **状态/日志通道错**：docroot 下的 `.txt` 恒 404 | `GET /phantom_status.txt` 与固件自带 `GET /Lang_Hdr.txt` 同为 404（`.asp/.js/.css/.png` 正常）；可用通道是 httpdb 的 `/_temp/<name>` → 物理 `/tmp/upload/<name>`（`ks_tar_install.sh` + `Module_Softsetting.asp`、fancyss 同构） |
| K4 | **缺 nat-start 兜底**：init.d 只有 `S98phantom.sh` | Asuswrt 重建 iptables 后转发/DNAT 规则丢失而进程还在 → 「插件显示运行中、LAN 却断」；现场还残留 `FORWARD -i phantom0` 无人清理 |
| K5 | **日志轮换换 inode**：`trim_log` 用 `mv` 换文件 | 运行中的 phantom 持有旧 fd，之后所有输出写进无人引用的文件 → 日志页停在「配置已保存…」而隧道在满速跑（实测 28,614 行、17:25 后全部丢失） |
| K6 | **固件 curl 是 `--disable-proxy` 编译** | 测速一律 `http=000`（`proxy support is disabled in this libcurl`），但不走代理的 metrics 请求正常；只有 `/koolshare/bin/curl-fancyss` 能用 SOCKS |
| K7 | **测速百分比单位错** | `speed`(bytes/s) 与「服务端上行 Mbps」未对齐单位，真机打印「本次达到 **786%**」 |

### 改动

- **控制面契约**（`phantom_config.sh`）：参数解析改为「先认直接调用（`$1`=action），再认软件中心调用
  （`$1`=id、`$2`=action、第三参数进 `ARG3`）」；新增 `api_ack()` 在 dispatch 之前回包
  `/_resp/<id>`（curl 优先、wget 回退、5s 超时），耗时动作仍走 `detach_run`；新增 `start_nat`
  （仅在规则确实丢失时重启）与 `_trim`（看门狗裁剪日志用）；`cleanup_rules()` 补 iptables 残留清理。
- **运行期文件与布局**：状态/日志改到 `/tmp/upload/phantom_{status,log}.txt`，页面经 httpdb 的
  `/_temp/` 读；删掉 docroot `/www/_temp` 软链方案与 `PHANTOM_WWW_TEMP`；安装时清理历史死软链。
- **日志与轮换**：`trim_log` 改为「tail 到临时文件后 `cat` 回原文件」（保持 inode，运行中进程的
  输出不丢）；默认级别 `warn`；看门狗每 5 分钟调 `_trim`。
- **开机与 nat 钩子**：`init.d/S98phantom.sh`（`ks-wan-start.sh` 以 `start` 调用）+
  `init.d/N98phantom.sh`（`ks-nat-start.sh` 以 `start_nat` 调用）；降级模式的 `/jffs/scripts/nat-start`
  行同步改为 `start_nat`。`ks <0|1>` 在真机上不会被调用，仅保留兼容。
- **测速**：新增带 SOCKS 的 curl 探测（真调一次 `--socks5-hostname`，不给 `-s`，否则错误被静音），
  结果写 `PHANTOM_CURL_SOCKS`；百分比单位对齐为 kbps vs kbps。
- **页面（`Module_phantom.asp`）**：单页三 tab 六区块（开关/状态、服务配置、白名单、运维、日志、帮助）
  + 三皮肤；`params` 改字符串；状态/日志首选 `/_temp/`；`get_log()` 收敛为单定时器；UI rev 随改动递增
  （当前 r5，右上角显示，用于确认浏览器加载的是哪一版）。
- **工具与测试**：`tools/live-check.sh` 弃用 `command -v` 改绝对路径探测、UI rev 用 sed 取值并与本地
  ASP 逐项比对、新增 `/tmp/upload/phantom_*`/N 钩子/iptables 残留/httpdb:3030 检查；
  `tests/smoke.sh` + `mock-bin`（dbus/nvram/cru/curl/phantom/ip/iptables/ipset）覆盖安装→启动→采样→
  测速→定时→诊断→停止→卸载，并对软件中心契约回包、fd 上限、采样落盘、ipset 清理逐项断言。
- **离线包**：`cargo xtask package koolshare` 双架构构建 + 组装 `.valid=hnd` 的 tar.gz 与 sha256；
  `bin/` 不入库；降级模式同包可用（`/jffs/phantom` + `services-start`/`nat-start`）。
- **关键接口**（详见 `client/koolshare/README.md` §4）：action `1|2|3|start|stop|restart|status|start_nat|cron|diag|_speedtest|_health|_trim`；
  dbus `phantom_{enable,uri,mode,protocol,gateway_mode,block_doh,lan_if,tun_name,tun_addr,table,dns_hijack,builtin_wl,whitelist,cron_enable,cron_time,watchdog,log_level,server_up_mbps,server_down_mbps,last_act,speed_last}` +
  `softcenter_module_phantom_*`；前端 `GET /_api/phantom`、`POST /_api/`、`GET /_temp/phantom_status.txt`、`GET /_temp/phantom_log.txt`。

### 验证

- L0/L1：`sh -n` 全脚本；假根冒烟 **koolshare 模式 119/119、`/jffs` 降级模式 119/119**
  （含"页面自检"：JS 里 `params_inp`/`params_chk` 的每个 id 都必须在 DOM 里真实存在）。
- 真机（2026-09-13）：`/bin/sh install.sh` 干净通过（S98/N98 钩子 + `/tmp/upload` 布局）；
  页面点提交毫秒级返回、状态卡有速率、日志页可读、`上次动作`=已启动；
  `ip rule` 就位、LAN 命中白名单走隧道（日志 `-> Proxy (whitelist)`）;
  测速 **359–380 KB/s**（服务端上行 3 Mbps ≈ 375 KB/s，达 96–103%）。
- 真机（2026-10-04，内核分流上线后复测）：UI r5、`gw=kernel-split`、ipset 174 条、fd 23/16384、
  DoH 拦截 16 条、`phantom_perf.log` 持续在写；页面 ASP md5 与本地源码逐字节一致。
- 部署路径：软件中心「离线安装」与 SSH（`/bin/sh /tmp/phantom/install.sh`）两条都验证过；
  重装幂等（先停服务再装，配置与白名单保留）。
- 回归：关闭开关后进程 / `ip rule` / iptables / `ip link` 全部回退；`ks-nat-start.sh start_nat`
  在规则被冲掉后自动补回；`relay ⇄ kernel-split` 模式切换各验一次。

### 明确不做

- **不在本轮改 Rust 数据面**（"控制面外壳、零回归"是当时定的边界；数据面优化另开 10-04 那一轮）。
- 不做无软件中心以外的平台适配：armv7 机型、ROG/TUF 皮肤**未在真机验证**（代码路径存在，靠 `uname -m`
  与机型判断切换）。
- 不把密钥/真实服务器地址写进仓库；`config/` 只放示例模板，诊断输出默认脱敏。

## 遗留

- 路由器**重启自启**（`S98phantom.sh` 路径）未做真机验证，需要一次重启窗口。
- 软件中心 GUI 完整走一遍「卸载 → 离线安装」尚未逐项截图确认（SSH 路径已验证）。
- 页面人工点击提交（浏览器侧）由用户完成；UI rev 写入页面右上角便于确认版本。
- armv7 机型与 ROG/TUF 皮肤、`fc_hw` 硬件 offload 命中情况：见 2026-10-04 那份文档的遗留清单。
