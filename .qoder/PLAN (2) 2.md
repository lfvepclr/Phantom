# Phantom：白名单代理分流（默认直连）+ macOS 图标状态 + 鸿蒙真机打包

## 摘要

三件事：① 分流语义改为**默认直连、仅白名单走代理**，内置被墙域名清单用**预构建 FST 索引**（零解析加载、27k 条目约数百 KB），并支持用户维护；② macOS 菜单栏换成可辨识的极简幽灵图标 + 四态反馈，App 图标重做为极简版；③ 安装 DevEco Studio，构建/签名 HarmonicOS HAP 并装真机测试。

## 关键改动

### A. 分流：默认直连 + 白名单代理

- **语义**：Smart/Auto 模式下 用户规则 → 白名单（域名后缀命中）→ **Proxy**；其余一律 **Direct**。Global 模式仍全量代理，Direct 模式全直连。`RulesConfig.final_action` 默认值由 `proxy` 改为 `direct`（行为变更写入文档）。
- **规则决策接入 SOCKS5/HTTP CONNECT 路径**（当前只有 `tun.rs` 用规则引擎，所以系统代理模式等于全量代理）：直连时由客户端本机建连（本地解析），代理时维持现状（域名交给服务器解析）。
- **DNS 策略（关键）**：走代理的域名**从不做本地解析** → 不污染系统 DNS 缓存、不泄漏被墙域名查询；直连域名按常规走本地 DNS。TUN 路径（mac TUN/鸿蒙/Android/路由器）的 DNS 劫持改为**按白名单选择性劫持**：白名单域名走 DoT（经隧道），其余走本地解析器，避免国内域名被解析成海外 CDN 节点。
- **内置清单**：`cargo xtask rules update` 从 jsDelivr 拉 `Loyalsoldier/clash-rules@release/proxy.txt`（实测 27,089 条），归一化（小写、去 `+.`/通配符、去重、剪掉已被父域覆盖的子域）→ 排序 → 生成 FST 二进制；同时写 `.meta.json`（来源 URL、生成时间、条数、sha256）。生成物入库，**编译与运行都不联网**。
- **索引方案（回答"快且省内存"）**：用 `fst` crate 在生成期构建索引，客户端 `include_bytes!` + `Set::new(&bytes)` 零拷贝加载；查询时枚举域名后缀（≤4 次 `contains`），复杂度 O(域名长度)，发生在每条新连接上（微秒级）。不再用 `DomainSuffixTrie` 承载大清单（它每节点一个 HashMap，27k 条会占几十 MB、构建耗时秒级）；该结构保留给用户自定义的少量规则。
- **用户可维护**：mac 客户端弹窗新增「分流白名单」编辑区（每行一个域名，含子域，持久化到 UserDefaults，与内置清单合并，支持一键重新连接生效）；CLI/高级用户可用 `~/.config/phantom/proxy_domains.txt`；`[rules]` 用户规则优先级最高。自定义条目同样走 FST/后缀匹配，但数量小，直接在启动时并入索引。
- **IP 类白名单**：少量"按 IP 被墙"的服务（如 Telegram CIDR）走既有 LC-trie 的 IP-CIDR 规则，与域名白名单并行生效。
- **可观测**：日志 `route <target> -> DIRECT|PROXY (<reason: user|whitelist|final>)`；metrics 增加 `phantom_route_direct_total` / `phantom_route_proxy_total`，便于用"VPS 侧连接数是否增长"对账。

### B. macOS 图标与状态

- 菜单栏改为**代码绘制的幽灵 Shape**（约 16×16pt、模板渲染自动适配深浅色），删除 `MenuBarIcon.png/@2x` 及 `Package.swift` 对应资源项：idle=空心灰、connecting=空心黄、running=实心绿、error=红+感叹号角标；`.help("Phantom — <状态>")`，弹窗头部保留 App 图标与 "Phantom" 名称。
- App 图标用 imagegen 生成 1024×1024 极简幽灵（深色底 + 单一渐变，无网格/节点/六边形），覆盖 `appicon.png` 后经 `cargo xtask icons` 生成 `Icon.icns` 与 Android/鸿蒙图标资产。
- 开关状态文案统一由 `PhantomState` 提供（popover 与菜单栏一致）。

### C. 鸿蒙客户端打包（真机 HarmonyOS NEXT 6.x）

- **环境**：安装 DevEco Studio 6.x（含 HarmonyOS SDK API 20/24、hvigorw、ohpm、hdc、hap-sign-tool）并登录华为开发者账号；按实际 SDK 布局修正 `.cargo/config.toml` 的 ohos clang 链接器路径与 `scripts/build-harmony.sh`、`xtask` 中写死的 `/Applications/DevEco-Studio.app/...`。
- **构建**：`cargo xtask build harmony` → `libphantom_harmony.so` 落到 `entry/libs/arm64-v8a/` → `hvigorw assembleHap -p module=entry@default -p product=default -p buildMode=debug --no-daemon` → DevEco 勾选「自动签名」（生成 p12/cer/p7b 并把设备 UDID 写入调试 profile）。
- **安装与验证**：`hdc list targets` → `hdc install <signed.hap>` → `hdc shell aa start -a EntryAbility -b co.phantom.harmony` → 粘贴服务器 URI → 系统 VPN 授权弹窗 → 连接。
- **版本对齐**：先 `hdc shell param get const.ohos.apiversion`；若低于工程 `compatibleSdkVersion 6.0.0(20)`，下调 `compatibleSdkVersion`/`targetSdkVersion` 后重建。
- `client/harmony/README.md` 用「自动签名 + hdc 安装」替换现有手工 hap-sign-tool 章节。

## 接口与契约变更

- xtask 新增：`cargo xtask rules update [--mirror jsdelivr|gitee|file]`、`cargo xtask rules verify`（条数/抽查/哈希/幂等）。
- 数据文件：`client/data/proxy_domains.fst`（预构建索引）、`client/data/proxy_domains.meta.json`、`client/data/proxy_cidrs.txt`（可选 IP 白名单）。
- 配置：`[rules] builtin_proxy_whitelist = true`（默认开）；`final_action` 默认改为 `direct`；新增 `~/.config/phantom/proxy_domains.txt` 用户白名单；mac UI 白名单编辑区。
- 指标：`phantom_route_direct_total`、`phantom_route_proxy_total`。

## 测试与验收

- 规则单测：白名单域名（`www.google.com`、`youtube.com`）→ PROXY；非白名单（`v.youku.com`、`www.baidu.com`）→ DIRECT；用户自定义条目 → PROXY；`final_action=proxy` 仍可全量代理；内置清单开关关闭后回到全量直连。
- 索引：`rules verify` 断言 FST 条数与来源一致、抽查 google/youtube 命中、youku 未命中、blob 大小与 sha256 固定；启动路径只做 `Set::new(bytes)`（基准：10 万次查询 < 50ms，内存增量 < 1MB）。
- 端到端（本机 + 真服务器 203.0.113.10）：mac 系统代理访问 `v.youku.com` 时 VPS 侧连接计数不增长且流量为 0；访问 `google.com/generate_204` 返回 204 且 VPS 侧计数增长；`route …` 日志与 metrics 对账一致。
- DNS 验证：代理目标连接过程中 `log stream --predicate 'process == "mDNSResponder"'` 无对应域名查询（证明未本地解析）；直连目标正常本地解析。
- 图标：四态可辨（灰/黄/绿/红）、浅/深色菜单栏均清晰、`codesign --verify --strict --deep` 通过、Dock/Finder 显示新 App 图标。
- 鸿蒙：HAP 安装成功、VPN 弹窗出现、连接后手机浏览器可访问 google.com、`hdc shell hilog` 无 panic；手机访问 youku 时 VPS 侧无对应连接。
- 回归：`cargo test -p phantom-client --lib`、`cargo test -p phantom-core --lib` 全绿；既有 xtask 目标不回归。

## 假设与默认

- 白名单默认开启、默认直连；清单随发版更新（不做运行时下载），用户可自行增补——列表未覆盖的被墙站点会走直连并失败，这是该模式的已知代价。
- 首选数据源 Loyalsoldier/clash-rules `proxy.txt`（jsDelivr 可达；raw.githubusercontent 在本机不通），保留 gitee/本地文件后备方案。
- mac 端白名单可编辑；鸿蒙 v1 仅用内置清单 + 配置文件（不做 App 内编辑器）。
- 鸿蒙只做 debug 签名 + 真机测试；正式发布签名不在本次范围。
- App 图标用 imagegen 生成位图后走既有 icons 流程；菜单栏图标保持代码绘制（便于按状态着色与适配深浅色）。
- 需要你本人完成的只有：DevEco 登录华为账号并信任设备（自动签名）、真机上确认 VPN 授权弹窗；其余构建、安装、验证我来做。
