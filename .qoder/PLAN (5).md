# Phantom 鸿蒙客户端：网络切换自愈 + Google 应用修复 + 首页视觉重构

## 摘要
分四块：①WiFi⇄移动数据切换后自动重连（隧道自愈，用户无感）；②Google Earth/Maps 走不通——已定位到手机侧 TUN 栈（Mac 侧 SOCKS5 打 kh.google.com:443 TLS1.3 正常，服务器侧每次投递 24–30KB，手机只发 905B ClientHello 后挂 10s 重试），先加可开关追踪复现、再按证据修；③DNS/分流日志改成可读、可过滤；④首页按鸿蒙规范整体重排，并合并上一轮的扫码方向/全屏/历史/图标/二维码分享。服务端与部署不变。

## 关键改动

**A. 网络切换自动重连（扩展进程 + Rust）**
- `PhantomVpnExtensionAbility` 订阅 `connection.on('netAvailable'|'netLost'|'netCapabilitiesChange')`，1s 去抖；命中后重新 `protectProcessNet()`、调用新增 NAPI `phantomHarmonyOnNetworkChange()`、状态写 `Starting` + `reconnect` 标记；UI 顶部显示"网络已切换，正在自动重连…"且**不停隧道**。
- Rust 新增 `android_notify_network_change()`：丢弃共享 DNS 的 UDP 隧道流（下次查询自动重建）、清空 QUIC 池、重置 failover 健康计数、把在途 TCP 流标记失效让 relay 立即结束（不再挂 10s+）；隧道 socket 开 TCP keepalive（15s/3 次）。
- 兜底：扩展进程被杀（心跳文件 >5s 未更新）时，UI 进程按 3s/10s/30s 退避自动重启，最多 3 次，之后转为"手动重连"按钮。
- 验收：切换网络后 ≤15s 内新连接可用，不出现永久"连接已中断"。

**B. Google Earth/Maps：先诊断、再按证据修**
- 诊断：新增 NAPI `phantomHarmonySetTrace(path)` + Rust 追踪写入器，输出到独立文件 `<filesDir>/phantom_tun_trace.log`（限 5000 行，每流最多 8 条注入记录），记录对端 SYN 选项（MSS/wscale/SACK/TS）、我们 SYN-ACK 选项、对端通告窗口采样、每个注入分段的 seq/len/所遵从窗口、RTO 重传、流结束原因与上下行字节；详情面板加"记录 TUN 追踪"开关（默认关，重启隧道生效），追踪行不进 UI 日志。
- 复现：真机打开 Google Earth + Google Maps 各 30s，同时拉取 trace、手机日志与服务器日志做三段对账。
- 修复（按实测结果择一执行，判定条件写死）：对端窗口小于 8KB/未协商 wscale → 删除 `MIN_PEER_WINDOW` 下限，严格按通告窗口注入并加零窗口 persist 探测（500ms/1 字节）；重复 ACK 未触发快速重传 → 补 3-dup-ACK 快速重传与 RTO 序列修正；注入 seq/len 正确却无 ACK → 修 `drain` 唤醒与重传监督器的配合；应用重传被丢弃 → 修 RCV.NXT 裁剪以接受重叠分段；出现分片丢包 → `TCP_MSS` 1400→1360 并同步 TUN MTU。
- 验收：Earth 地图瓦片可加载、Maps 可搜索出结果；同时回归浏览器大文件下载与 YouTube 播放。

**C. 日志与分流可读性**
- DNS 行改用与路由一致的词汇：`route douyin.com:53 -> Direct (dns local)` / `route kh.google.com:53 -> Proxy (dns tunnel)`，使现有"显示直连"开关能一并过滤国内 DNS 噪音。
- 5s 内同域名同判定只保留一条并追加 `×N`，抑制抖音这类高频查询刷屏。
- 日志卡加过滤 Chip（仅隧道 / 全部），默认"仅隧道"；保留暂停、清空与 200 行上限。

**D. 首页信息架构与视觉重构（合并上一轮待办）**
- 结构自上而下：①大标题 `Phantom` + 右上仅详情图标；②**服务器卡片**（节点名 + 状态徽标 + `203.0.113.10:443 · TCP · AES-256` + 实时↓↑与分流统计，整卡可点开详情；未配置时显示"添加连接"）；③通栏胶囊主按钮（启动/停止 + 一行授权说明）；④`SegmentButtonV2` 模式段控件（全局/智能/直连）**移到主按钮下方**，解决按钮上方拥挤；⑤连接输入卡默认折叠（"已保存 N 个连接 · 手动输入或扫码"），展开为 `TextInput` + 扫码图标 + 历史 ▾ 图标；⑥日志卡；⑦详情面板（bindSheet）承载地址/协议/加密/密钥指纹/时长/速率/流量/分流统计 + **分享二维码**（二维码 + 复制 + 系统分享，分享从主界面移到这里）+ 测延迟/测速。
- 视觉：优先系统组件默认样式（Button 胶囊、TextInput、SegmentButtonV2、ChipGroupV2、ComposeListItemV2、SubHeader、bindSheet/bindMenu），卡片用 `uiMaterial.ImmersiveMaterial`（`.systemMaterial(...)`）景深材质 + 16vp 圆角 + 12/16 间距；不再硬编码 `#007DFF/#999999`，状态色走语义色（主题 API 无 palette getter，故依赖系统组件默认色 + 材质）。
- 图标用项目内 SVG（`ic_scan/ic_share/ic_history/ic_info/ic_pause/ic_clear`），不依赖 `sys.symbol.*`。
- 扫码页（合并上一轮）：`display.on('change')` + `previewOutput.setPreviewRotation(getPreviewRotation(displayRotation))` 跟随重力转正（降级 PixelMap 路径同步 `pixelMap.rotate()`）；`onPageShow/onPageHide` 切换 `setWindowLayoutFullScreen(true)`+`setWindowSystemBarEnable([])` 实现仅扫码页沉浸全屏，根 Stack 加 `expandSafeArea`，消除上下黑边。
- 连接历史（合并上一轮）：`phantom_ui` 新增 `serverHistory`（行式 `uri\tlastUsedMs\tverifiedMs`，新→旧、去重、上限 20）；点 Start 或扫码写入 `lastUsedMs`，状态变为 Running 时写 `verifiedMs`（✓=成功连接过）；输入行 ▾ 打开 `bindMenu` 下拉（节点名 + 地址 + 相对时间 + ✓ + 行尾删除 + 清空历史），点条目只填入不自动连接。

## 接口与契约变更
- 新增 NAPI：`phantomHarmonyOnNetworkChange()`、`phantomHarmonySetTrace(path: string)`。
- 新增 preferences 键：`serverHistory`（行式文本）、`logFilter`、`tunTrace`；现有 `serverUri/proxyMode/showDirectLogs` 不变。
- 新增文件产物：`<filesDir>/phantom_tun_trace.log`（仅调试期存在）。
- Rust 行为变更：DNS 隧道流可被网络变化主动丢弃并重建；TUN TCP 注入严格遵从对端通告窗口（移除 8KB 下限）。
- 扫码页会切换窗口沉浸式状态（进入开启、退出恢复）。

## 测试与验收
1. 构建：`./scripts/build-harmony.sh` → `DEVECO_SDK_HOME=/Applications/DevEco-Studio.app/Contents/sdk hvigorw assembleHap -p module=entry@default -p product=default -p buildMode=debug --no-daemon`；UI 相关先在模拟器 `Pura X Max` 验证（`Emulator -start/-screenshot/-rotation`），签名不通过则直接真机 HOP-AL00。
2. 网络切换（真机）：WiFi→移动数据、再切回，各检查 ≤15s 内浏览器可打开 google.com、日志出现 `network changed` 与 `dns tunnel flow established`、不残留失效流。
3. Google 应用（真机）：开追踪复现 → 拉 trace + 两侧日志 → 按判定条件修复 → Earth 瓦片/ Maps 搜索通过；浏览器大文件与 YouTube 回归。
4. 日志（真机/模拟器）：抖音类国内应用运行 1 分钟，默认只显示隧道相关行；同域名重复查询合并为 `×N`；切"全部"能看到 `dns local` 明细。
5. UI：模拟器截图核对卡片/材质/段控件/折叠输入/图标，旋转模拟器确认扫码页预览正立且无黑边；真机补 0/90/180/270 实拍扫码。
6. 历史与分享：导入两个串 → 下拉排序/去重/仅成功项带 ✓、重开 App 仍在；点条目只填入；分享面板二维码用 `cv2.QRCodeDetector` 解码截图，断言等于当前串。
7. 回归：`cargo test -p phantom-client --lib`、`-p phantom-core --lib`，以及既有 e2e（`full_link_udp`/`socks5_udp`）。

## 假设与默认
- 已确认选择：Google 先诊断再修、网络切换自动重连、首页整体重排。
- 本工作区没有交互/视觉/配色类 skill（仅有 harmonyos-build-deploy、harmonyos-device-automation、rust-async-patterns、rust-best-practices）；视觉规范改为直接使用 SDK 主题/材质与系统组件默认样式。
- 视觉不做单独设计稿，直接在 ArkUI 实现并通过模拟器截图迭代确认。
- 网络切换、Google 应用、DNS 日志、VPN 相关的验证必须在真机完成（模拟器缺 `com.huawei.hmos.vpndialog`，无法建 TUN）；模拟器只承担 UI/扫码页布局与旋转验证。
- 需要切换网络时，优先用 `hdc shell` 控制；若设备不支持脚本切换，则由你手动切换并告知时间点，我据此对账日志。
- ✓ 仅表示本机成功连接过；历史上限 20 条、不做跨设备同步。
