# Phantom koolshare 插件：软件中心提交卡死修复与真机可用性收尾

- 计划日期：2026-09-13
- 目标机器：`<路由器IP>`（RT-AX86U Pro，`3.0.0.4.388_24199_koolcenter`，SSH 端口 <SSH端口>）
- 代码位置：`client/koolshare/`
- 上游计划：[20260913-phantom-koolshare-路由器插件.md](20260913-phantom-koolshare-路由器插件.md)

## 1. 摘要

「点提交后一直卡住 → 报错『后台执行失败，请看日志页』」的根因有两条，缺一条都会失败：

1. **调用约定错**。软件中心 `POST /_api/` 的契约是
   `{"id":N,"method":"<脚本>","params":[...],"fields":{…}}`：httpdb 先把 `fields`
   写进 dbus，再执行 `/koolshare/scripts/<脚本> <id> <params...>`。
   插件按 `$1=action` 解析，于是 `$1` 是随机请求 id，`case` 落到 `*)` 打印 Usage 并
   `exit 1`。**现场证据**：dbus 里 `phantom_enable=1`、`phantom_uri` 已写入（说明
   fields 落库成功），但 `/tmp/phantom_log.txt` 为 0 字节、`phantom_last_act` 仍是
   「安装完成」——脚本主体从未执行。
2. **从不回包**。脚本必须 POST 到 `http://127.0.0.1:3030/_resp/<id>`（`base.sh` 的
   `http_response` 就是这么做的），httpdb 才结束那个 HTTP 请求；不回包前端一直等，
   最后 `result != id` → 弹「后台执行失败」。

**契约证据**（真机只读核查）：`/rom/etc/koolshare/scripts/ks_app_install.sh` 用
`case $2 in` + `http_response $1`；`/koolshare/scripts/clash_downyamlsel.sh` 用
`case $2 in`；`clash_getbasicyaml.sh`（`params` 为空）仍然 `http_response $1`；
`/koolshare/scripts/base.sh` 的 `http_response(){ curl -X POST -d "$ARG0"
http://127.0.0.1:3030/_resp/$ID; }`；`netstat` 显示 `httpdb` 监听 `127.0.0.1:3030`。

顺带发现两个「插件永不可用」的既有缺陷：

- **状态/日志读不到**：`http://<路由器IP>/phantom_status.txt` 与 `/www/Lang_Hdr.txt`
  同为 404 → 本固件 httpd **不服务 docroot 下的 `.txt`**（与权限无关），把文件软链进
  `/koolshare/webs` 是死路。正确通道是 httpdb 的 `/_temp/`，物理目录 `/tmp/upload/`
  （证据：`ks_tar_install.sh` 写 `/tmp/upload/soft_log.txt`，`Module_Softsetting.asp`
  读 `/_temp/soft_log.txt`；fancyss 的 `webtest.txt` / `ss_log.txt` 同构）。
- **nat-start 无兜底**：`init.d` 只有 `S98phantom.sh`，缺 `N*` 钩子。路由器重载防火墙后
  iptables 被冲掉而进程还在，表现为「插件显示运行中、LAN 却断」。现场还残留
  `iptables -A FORWARD -i phantom0 …` 无人清理。

## 2. 关键改动

### 2.1 控制面契约（`scripts/phantom_config.sh`）

- 参数解析改为「先认直接调用，再认软件中心调用」：`$1` 命中已知 action → 直接调用
  （`$2` 为附带参数，如 `ks 1`）；否则若 `$2` 命中已知 action → 软件中心调用
  （`API_ID=$1`、`ACTION=$2`、第三个参数进 `ARG3`）；都认不出时仍先回包再报「未知动作」。
- 新增 `api_ack()`：解析出 `API_ID` 后、任何耗时动作之前 POST `body=<id>` 到
  `http://127.0.0.1:${PHANTOM_HTTPDB_PORT:-3030}/_resp/<id>`（`$CURL` 优先、
  `$WGET --post-data` 回退、5s 超时，失败只写日志），保证页面毫秒级拿到响应。
- dispatch 改用 `ACTION`；`_speedtest` / `_health` 等内部入口不触发回包。
- 新增 `start_nat` action：`enable=1` 且 `ip rule` / iptables 规则缺失时才 `restart`
  （规则健在则不动，避免无谓中断）。
- `cleanup_rules()` 增补 iptables 回滚（`FORWARD` 两条 ACCEPT + `nat PREROUTING`
  53 端口 DNAT，目标地址取自生成的 `client.toml` 的 `dns =`，回退 8.8.8.8）。

### 2.2 前端（`webs/Module_phantom.asp`）

- `post_action` 的 `params` 改字符串 `[String(flag)]`，与其它插件一致。
- `STATUS_PATHS = ['/_temp/phantom_status.txt', '/phantom_status.txt']`、
  `LOG_PATHS = ['/_temp/phantom_log.txt', '/phantom_log.txt']`（docroot 仅作兜底）。
- `PHANTOM_UI_REV` → 4；`get_log()` 收敛为单定时器（原先切 tab2 与提交后会并发两条
  1.5s 轮询）。

### 2.3 运行期文件与安装布局

- 运行期文件改到 koolshare 约定目录：`/tmp/upload/phantom_log.txt`、
  `/tmp/upload/phantom_status.txt`（pidfile 仍在 `/tmp`）；install 前
  `mkdir -p /tmp/upload`，目录不可用时回退 `/tmp`。
- 删除 docroot 与 `/www/_temp` 软链逻辑及 `PHANTOM_WWW_TEMP` 变量，install 顺带清理历史软链。
- `uninstall.sh` 补清 `/tmp/upload/phantom_*.txt`、历史软链、N 钩子。

### 2.4 开机与 nat 兜底

- 新增 `/koolshare/init.d/N98phantom.sh` → `phantom_config.sh`（`ks-nat-start.sh`
  会以 `start_nat` 调用）。
- `/jffs` 降级模式注册的 `nat-start` 行由 `restart` 改为 `start_nat`。
- 文档更正：`S*` 由 `ks-wan-start.sh` 以 `start` 调用，计划里写的 `ks <0|1>`
  入口在真实固件上并不存在（保留兼容但不作为开机路径）。

### 2.5 工具、测试与文档

- `tools/live-check.sh`：弃用 `command -v`（本机 busybox 没有），改绝对路径探测
  `dbus/curl/cru`；UI rev 用 sed 取值并与本地 ASP 比对；新增 `/tmp/upload/phantom_*`
  时间戳、`N98phantom.sh`、iptables 残留、httpdb:3030 检查；示例统一
  `ssh -p <SSH端口> admin@<路由器IP>`（选项必须在 host 之前）。
- `tests/smoke.sh` + `mock-bin`：新增 `ip`/`iptables` stub 与 curl 回包捕获，断言
  软件中心形态 `<id> 1` 必须回包并拉起隧道、直接 `1` 仍可用、`start_nat` 条件重启、
  运行期文件落在 `/tmp/upload`、ASP 首选 `/_temp/`。
- 文档同步：`client/koolshare/README.md`、`ARCHITECTURE.md` §7.0.1、
  `deploy/router/README.md`、根 `README.md`（补 koolshare 形态与
  `cargo xtask package koolshare`）、`tests/PERF_ROUTER_REPORT.md`（修正 pidfile 路径并填基线）。

## 3. 接口与契约

- 软件中心调用约定（写进文档）：
  `POST /_api/ {id, method, params, fields}` → `fields` 落 dbus → 执行
  `/koolshare/scripts/<method> <id> <params...>` → 脚本 POST `/_resp/<id>`（body=id）
  → HTTP 返回 `{"result": <body>}`，前端以 `result == id` 判成功。
- `phantom_config.sh` action：`1`/`2`/`3`/`start`/`stop`/`restart`/`status`/`cron`/
  `diag`/`_speedtest`/`_health`，新增 `start_nat`；`ks <0|1>` 保留兼容。
- HTTP 数据通道：`/_temp/phantom_status.txt`、`/_temp/phantom_log.txt`（物理
  `/tmp/upload/`）；`/_api/phantom`、`/_api/phantom_last_act`、
  `/_api/phantom_speed_last` 不变。
- dbus 键、`phantom://` URI、分流语义、`phantom.env` 布局不变（移除
  `PHANTOM_WWW_TEMP`，新增可选 `PHANTOM_HTTPDB_PORT`）。

## 4. 测试与验收

- L0：`sh -n` 全部脚本；有 shellcheck 则 `shellcheck -s sh`。
- L1：`PHANTOM_SMOKE_ROOT=$(mktemp -d)/ks sh client/koolshare/phantom/tests/smoke.sh`
  全绿（含新增契约断言）；`cargo xtask package koolshare` 重新出包成功。
- 真机：重跑 `install.sh` 刷新布局与钩子 → `live-check.sh` 无假阴性且 rev=4 →
  页面开开关、填连接串、点提交：不再卡、无「后台执行失败」，约 3–6s 内状态卡显示
  「运行中」并有速率/连接数，日志页有内容、`上次动作` 为「已启动」。
- 数据面核对：`ip rule show | grep 200`、`ip link show phantom0`、
  `curl --socks5-hostname 127.0.0.1:1080 https://www.google.com/generate_204`，
  路由器自身 `curl` 仍直连。
- 回归：测速按钮有结果；关闭开关后进程 / `ip rule` / iptables / `ip link` 全部回退；
  `ks-nat-start.sh start_nat` 冲掉规则后能自动补回；软件中心 GUI 离线安装与卸载全流程；
  路由器重启自启（需用户确认后再做）。
- 基线：按 `tests/PERF_ROUTER_REPORT.md` 采集路由器侧吞吐、CPU、TUN 收发与健康度。

## 5. 提交与假设

- 提交范围：`client/koolshare/**`、`.agents/plans/20260913-phantom-koolshare-*.md`、
  `tests/PERF_ROUTER_REPORT.md`、`xtask/src/{main.rs,pack.rs}`（koolshare 打包）、
  `deploy/router/{README.md,phantom.sh}`、`AGENTS.md`、`ARCHITECTURE.md`；`.gitignore`、
  `README.md` 等混合文件按 hunk 暂存，其它模块的存量改动留在工作区；`dist/` 不入库。
- 默认：不引入 Rust 改动（纯控制面）；其它固件若 `/_temp/` 映射不同只靠前端候选兜底；
  armv7 机型与 ROG/TUF 皮肤仍标记为「未在真机验证」。
- 若真机验收暴露 TUN / 路由层问题，只修控制面可控部分并记录，不扩到数据面改造。
