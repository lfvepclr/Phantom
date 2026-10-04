# koolshare 插件「提交失败（请求未完成）」：失效会话打崩 httpd 的止损与自愈

日期：2026-10-04
范围：`client/koolshare/**`（ASP + 状态采样 + 工具/测试/文档），不动 Rust 数据面。
关联：[20260913-phantom-koolshare-路由器插件.md](20260913-phantom-koolshare-路由器插件.md)（插件本体与
软件中心调用约定）、[20261004-phantom-路由器内核分流-直连回内核快路径与夜间性能取证.md](20261004-phantom-路由器内核分流-直连回内核快路径与夜间性能取证.md)。

## 目标

在管理页「自定义域名」里填 `github.com` 点提交，弹「提交失败（请求未完成）。请用 SSH 排查：
/bin/sh /koolshare/scripts/phantom_config.sh 1」。要定位真因并修到「提交可用、且不再反复把
路由器 Web 服务打崩」。

## 结论（实施记录）

### 根因

**插件服务端没问题，请求死在 httpd 这一跳；而 httpd 是被「失效会话 + httpdb 通道」打崩的。**

| 证据 | 观测 |
|---|---|
| 服务端链路正常 | 把页面真实 payload（19 字段 + `params:["1"]`）直接 POST 给 httpdb：68 ms 返回 `{"result":"779500"}`，`phantom_whitelist=github.com` 落库，日志出现「配置已保存，后台启动隧道…」 |
| 失败那次没到 httpdb | 报错时 dbus 白名单仍为空、日志无新行 —— `fields` 从未写入 |
| httpd 在崩 | `/tmp/syslog.log` 129 次 `Comm: httpd` + watchdog 反复 `start_httpd`；用户点提交的 15:58:46 正好崩了一次 |
| 触发条件可复现 | 带**失效会话**请求 httpdb 通道（`/_api/…`、`/_temp/…`）→ httpd 立即 SIGSEGV（pc 落在它自己的 CGI 名表 rodata 里）；带同样 cookie 请求 `.asp`、或不带 cookie 请求 `/_api/` 都正常 |
| 会自我维持 | httpd 一崩，watchdog 重启它 → 所有旧 token 失效 → 抓着旧 token 的页面下一次请求又把它打崩。现场实测到 **每 30 秒一次**、以及历史上**每分钟一次连续 2 小时 10 分**的崩溃循环（后台标签页被浏览器限流成低频率，正好对上） |

触发源是 ASUS 固件自身的缺陷（`http_autologout=30` 到点即失效会话），插件修不了固件，但**绝不能
继续往这个坑里踩**：管理页每 2 秒轮询状态、每 5 秒轮询上次动作（外加提交），正是这台「人肉 DoS」的主力。

### 改动

- **会话探针**：新增 `webs/Module_phantom_ping.asp`（正文 `phantom-ping-ok`）。真机验证：带无效
  cookie 请求它返回 200 + 登录跳转 HTML，**崩溃计数零增长**；而同样 cookie 打
  `/_api/phantom_last_act` 立刻崩 —— `.asp` 是唯一安全的会话检查通道。
- **取名踩坑（首版 404 后修）**：httpd 里硬编码 `Module_` 前缀 + `/koolshare/webs`
  （`strings /usr/sbin/httpd`：`isWebServer` / `websApply Updateing asp` / `Module_`）。
  首版叫 `phantom_ping.asp`，真机 `GET /phantom_ping.asp` **404**（浏览器控制台直接看到）；
  改名 `Module_phantom_ping.asp` 后才被 webs 处理器接管。页面同时加了兜底：探针页取不到
  （404/内容不含标记）就固定回退到 `Module_phantom.asp` 本体，只按「是否返回登录跳转页」
  判会话，不再依赖单一未验证通道。
- **白名单多行（用户第二次实测暴露）**：占位符写的是「一行一个域名，或用逗号分隔」，
  但 **dbus 存不了多行文本**，且 httpdb 不处理 JSON 转义 —— 用户敲两行 `github.com` /
  `jetbrains.com`，`"github.com\njetbrains.com"` 被**原样**落库成字面 `\n`（真机
  `dbus list` 复核确认），按逗号切只有一条：页面标红「1 条格式不合法」，脚本写出
  `github.comnjetbrains.com` 垃圾域名。修法：页面新增 `split_whitelist` /
  `normalize_whitelist`，`collect_fields()` 发送前把换行/空格/逗号统一折成单行；
  `write_domains` 把字面 `\n`、空白一并当分隔符兜底。顺带修正 mock dbus 的转义保真度
  （sed 替换串里的 `\n`/`&` 不再篡改值），冒烟测试加「逗号分隔」「字面 `\n`」两条功能断言。
- **失效即停**（`Module_phantom.asp` rev 8）：所有 httpdb 请求（读配置、状态、日志、测速、提交）
  统一走 `probe_then()`；探针发现会话失效就清掉全部定时器、禁用提交/测速，顶部横幅提示重新登录，
  在重新登录前不再发出任何 httpdb 请求。`.asp` 探针无响应时退避重试 6 次（约 15 s）再判定，
  兼顾 httpd 被重启的窗口。
- **后台标签页不轮询**：`visibilitychange` + `document.hidden` 停/启轮询，掐掉「每分钟崩一次」的形态。
- **提交自愈**：提交前先探针；撞上 httpd 崩溃时先等它恢复、确认会话仍在、**换新请求 id 重试一次**
  （拿旧会话盲目重试等于再崩一次）；最后一次仍无响应就用 `/_api/phantom` 的实际值核对是否已生效，
  不把「响应丢了但配置已写入」误报成失败。
- **少一个 httpdb 通道**：`phantom_status.sh` 把 dbus 的 `last_act` 写进状态 JSON，页面不再单独轮询
  `/_api/phantom_last_act`（接口与 dbus 键保留兼容，SSH / 其它工具照旧）。
- **工具与防线**：`tools/live-check.sh` 新增 httpd 健康项（崩溃计数、最近一次崩溃、watchdog 重启次数、
  `http_autologout`、探针页是否存在，并提示收敛办法）；`tests/smoke.sh` 新增 17 条断言
  （rev、探针命名与回退、`SESSION_LOST`、`document.hidden`、各轮询先过探针、状态含 `last_act`、
  白名单分隔符的两条**功能**断言、配置文件写入转义、安装/卸载探针页）。

### 验证

- L0：全部插件脚本 `sh -n` 通过；`live-check.sh` `bash -n` 通过；ASP 内联 JS `node --check` 通过。
- L1：`PHANTOM_SMOKE_ROOT=$(mktemp -d)/ks` 与 `PHANTOM_SMOKE_MODE=jffs` 两种形态各 **137/137 通过**
  （含白名单分隔符的功能断言）。
- 打包：`cargo xtask package koolshare` → `dist/phantom-0.1.0.tar.gz`（sha256
  `dc527925a18a26530298a78e81ed5137682f7e72f1e3cc490e4bf70f80e421df`），包内含
  `webs/Module_phantom_ping.asp`。
- 真机（RT-AX86U Pro / koolshare 官改，SSH `<SSH端口>`）：部署后 ASP 与本地（ASUSWRT 皮肤）md5 一致、
  `PHANTOM_UI_REV = '8'`、`/koolshare/webs/Module_phantom_ping.asp` 就位、状态 JSON 含
  `"last_act":"已启动 10-04 16:32:23"`、隧道 pid 4409 运行中（`gw=kernel-split`，ipset 138，fd 15/16384）。
- 真机探针回归：`curl -H 'Cookie: asus_token=bogus' http://<路由器IP>/Module_phantom_ping.asp` ×5 →
  全部 200 + 登录跳转，httpd 崩溃计数 **0 增长**。

### 明确不做

- 不修固件：httpd 的 SIGSEGV 是 ASUS 的缺陷，不做内核/固件补丁，只做预检与止损。
- 不新增 dbus 键、不改数据面、不改 `phantom://` URI 与分流语义。
- 不自动延长 ASUS 的 `http_autologout`（那是用户的安全策略），只把「会话过期」这件事做成可见、
  可恢复的状态。

## 遗留

- 现场若崩溃计数仍在增长，说明还有**旧版页面/其它 App** 抓着失效会话在轮询：重新登录路由器 Web、
  Ctrl+F5 刷新插件页（拿到 rev 8）即收敛；`live-check.sh` 的 3.5 节可直接看到计数与最近一次崩溃。
- 浏览器自动化验收（点提交看状态卡转「运行中」）仍需在登录后的真实浏览器里跑一次：
  修好后服务端链路已验证可用，剩下的只是「不再被 httpd 崩溃掐断」。
