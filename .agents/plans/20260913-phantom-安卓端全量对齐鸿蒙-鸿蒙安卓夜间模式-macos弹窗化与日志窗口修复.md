# Phantom：安卓端全量对齐鸿蒙 + 鸿蒙/安卓夜间模式 + macOS 弹窗化与日志窗口修复

## 摘要

三件事：① 把安卓端从「骨架」补到与鸿蒙功能对等（服务器信息、历史记录、分享二维码、应用内扫码、测延迟/测速、日志治理、切网自动重连、内嵌服务器页）；② 鸿蒙与安卓新增**跟随系统/浅色/深色**三态夜间模式，入口统一为独立「设置」面板；③ macOS 主界面从独立窗口改回菜单栏弹窗（分层折叠），并修掉日志窗口被其它 App 盖住后点不开的 bug。服务端与隧道协议零改动。

## 关键改动

### A. 安卓端对齐鸿蒙

**分层**：平台专用代码进安卓模块，公共能力才进公共包。

- 新增 `client/android/rust`（包 `phantom-android`，产物 `libphantom_android.so`，镜像 `client/harmony/rust` 的结构）：把现有 6 个 `Java_co_phantom_android_RustBridge_*` JNI shim 从 `client/src/platform/android.rs` 迁入，并新增 JNI 出口 `getStatsJson`、`notifyNetworkChange`、`setTracePath`、`setLogPath`、`clearLogs`，以及内嵌服务器的 `serverStart/serverStop/serverStatus/serverLastError/serversUri`。实现直接委托 `phantom_client::platform::android::*` 与 `phantom_server::bootstrap::*`（服务器部分照搬 `client/harmony/rust` 的已有实现）。Kotlin 侧 `System.loadLibrary` 改为 `phantom_android`，`scripts/build-android.sh` 与 `cargo xtask build android` 同步改 crate/产物名；`phantom-client` 的依赖图不变（不引入 `phantom-server`）。
- 公共层（`client/src/platform/android.rs`，安卓与鸿蒙共用）：新增 `android_set_log_path(Option<&str>)`（把环形缓冲日志同时落盘，超 1 MiB 保留尾部）与 `android_clear_logs()`；新增 `net_tune::protect_fd(fd)`——安卓经 JNI 调 `VpnService.protect()`，其它平台 no-op，`dns.rs` 建直连 DNS socket 后调用它，保证 `223.5.5.5` 这类本地解析器不被自己的 TUN 吞掉（鸿蒙侧走 `protectProcessNet`/排除路由，行为不变）。
- Compose UI 重构（`MainActivity` 拆分，用状态切换 + `BackHandler`，不引 navigation 依赖）：Dashboard（状态头 + 服务器卡 + 启停 + 模式段 + 折叠连接卡 + 日志卡）、设置面板、连接信息面板、全屏日志、扫码页、分享面板、内嵌服务器页；卡片圆角/品牌色 `#0A59F7` 与鸿蒙一致，用 Material 3 组件。
- ViewModel/Service：1s 轮询统计 JSON（`up/down/udp_*/conns/route_direct/route_proxy`）驱动速率与分流计数；延迟/测速走本机 SOCKS5（与鸿蒙同款用例）；`ConnectivityManager.registerDefaultNetworkCallback` → `notifyNetworkChange()` 并触发与鸿蒙一致的自动重连（倒计时/失败次数/耗尽提示 + 顶部提示条）；`VpnService` 注册网络回调、下发日志文件路径、`Builder` 增加直连 DNS 排除路由（API 33+）；
- 持久化：`SharedPreferences` 键名与鸿蒙一致（`serverUri`/`proxyMode`/`serverHistory`/`showDirectLogs`/`tunTrace`/`themeMode`），历史编码 `uri\tlastUsedMs\tverifiedMs` 与鸿蒙字节兼容，含「已验证 ✓ + 相对时间」下拉。
- 扫码：CameraX 1.3.1（core/camera2/lifecycle/view）+ ML Kit `barcode-scanning:17.3.0`；仅接受 `phantom://`，扫到即写入偏好并返回（不自动连接）；权限被拒 → 提示 + 相册选择（PickVisualMedia + ML Kit 静态图）与手输兜底。
- 分享：应用内二维码用 `com.google.zxing:core:3.5.3` 生成位图，另提供「复制连接串」与系统 `ACTION_SEND`。
- 日志：界面 200 行环形 + 重复行折叠 + 单行省略 + 「仅隧道/全部」过滤 + 暂停/清空/全屏 + 文件日志 + 「导出/分享日志」（含 TUN trace）。
- 内嵌服务器页与鸿蒙 `ServerPage` 对齐：端口/加密/协议选择、启停、状态与错误、展示 `phantom://` URI + 二维码 + 复制/分享。

### B. 夜间模式（鸿蒙 + 安卓）

- 鸿蒙：调色板从 `Theme.ets` 常量迁到 `resources/base/element/color.json` 与 `resources/dark/element/color.json`，`Theme` 字段改用 `$r('app.color.…')`（`ResourceColor`），110 处调用点基本不动；`statusColor()/statusBackground()` 等少数返回类型从 `string` 改 `ResourceColor`。三态由 `preferences` 的 `themeMode` + `applicationContext.setColorMode(COLOR_MODE_DARK/LIGHT/NOT_SET)` 实现，默认 `NOT_SET`（跟随系统），配置变更时自动重绘；扫码页的黑色取景覆盖层保持原样。
- 安卓：`PhantomTheme` 提供浅/深两套 ColorScheme（对齐鸿蒙语义色），三态由同一 `themeMode` 决定，切换即时生效并持久化。
- 两端新增独立「设置」面板（首页右上角齿轮）：主题三态、TUN 追踪开关、导出日志、白名单说明（内置清单已启用，不做 App 内编辑）、关于/版本。

### C. macOS 弹窗化 + 日志窗口修复

- 删除主窗口 `Window` 场景，改为 `MenuBarExtra` + `.menuBarExtraStyle(.window)` 的 `DashboardPopover`（宽 400、内容高度自适应并内部滚动）：状态头（含齿轮与 `⋯` 菜单）→ 服务器卡 → 通栏启停 → 模式段 → 日志卡（下限 140pt、占剩余高度、含放大/过滤/暂停/清空）→ 页脚（版本 + 显眼退出）。连接串、连接信息、白名单、设置改由 `.sheet` 承载，保持「分层折叠」的信息密度。
- 首次启动未配置：菜单栏图标显示提示角标，并弹一次性 `NSAlert`（说明点菜单栏图标即可配置，含「不再提示」，`UserDefaults` 记录）。
- 修日志窗口「被盖住后点不开」：新增窗口注册表（窗口内用 `NSViewRepresentable` 注册真实 `NSWindow`），`WindowBridge.showLogs()/showMain()` 在 `openWindow` 后于下一拍执行 `NSApp.activate(ignoringOtherApps: true)`（macOS 14+ 用 `NSRunningApplication.current.activate()`）、`deminiaturize()`、`makeKeyAndOrderFront(nil)`、`orderFrontRegardless()`，并给窗口加 `.moveToActiveSpace`，保证跨 Space、跨 App 前置都能唤起。
- mac 主题继续用系统语义色（已自动跟随系统深浅色），不新增开关。

### D. 工具链与构建

- 安装 NDK（`sdkmanager "ndk;26.1.10909125"`）与 `rustup target add aarch64-linux-android`；`scripts/build-android.sh` 补 `JAVA_HOME`（Android Studio 自带 JBR 17）、自动写 `local.properties` 的 `sdk.dir`、按新 crate 构建；依赖新增 CameraX 1.3.1、ML Kit barcode 17.3.0、zxing core 3.5.3（`dl.google.com`/mavenCentral/阿里云镜像均已确认直连可用）。
- 文档同步：`client/android/README.md`、`client/harmony/README.md`、根 `README.md` 的客户端章节补充夜间模式、设置面板与安卓构建/安装步骤。

## 接口与契约变更

- 新 crate/产物：`client/android/rust` → `libphantom_android.so`（Kotlin `System.loadLibrary("phantom_android")`）。
- 新 JNI：`getStatsJson(): String`、`notifyNetworkChange(): Long`、`setTracePath(path: String?)`、`setLogPath(path: String?)`、`clearLogs()`、`serverStart(workDir, port, cipher, proto): String`、`serverStop(): Int`、`serverStatus(): Int`、`serverLastError(): String`、`protectFd(fd: Int): Boolean`。
- 公共 Rust：`android_set_log_path`、`android_clear_logs`、`net_tune::protect_fd`。
- 偏好键新增 `themeMode`（`system|light|dark`），鸿蒙/安卓同名同义；历史编码与鸿蒙保持字节兼容。
- 服务端协议、隧道/分流语义、mac 系统代理行为不变。

## 测试与验收

- 自动化：`cargo test -p phantom-client --lib`、`cargo check --workspace --all-targets`（含新 crate，`--target aarch64-linux-android` 通过）、`swift test`（PhantomMacKit 现有用例 + 窗口注册表/缩放逻辑的可测部分）。
- 安卓真机（`22041216UC`，Android 14 / SDK 34）：`cargo xtask build android --debug` → `adb install -r` → 授权 VPN 出现系统钥匙图标 → google.com 可访问、`v.youku.com` 直连（服务器侧无对应连接、metrics 对账）→ 历史下拉显示 ✓ / 扫码导入 / 二维码分享 / 服务器信息 / 测延迟与测速 / 日志过滤与全屏 / 日志文件与导出 → Wi-Fi 与移动数据互切不中断（自动重连出现倒计时且恢复）。
- 安卓夜间模式：三态切换即时生效、重启保持、跟随系统时随系统变化。
- 鸿蒙真机（`HOP-AL00`）：浅/深/跟随系统三态 + 设置面板；首页、连接详情、内嵌服务器页颜色全部跟随，无浅色残留；TUN 追踪与日志导出仍可用；既有功能回归（连接、分流、扫码、分享）。
- mac：点击菜单栏直接弹窗（无独立主窗口）；连接串/信息/白名单在 sheet 中可用；日志窗口被 Chrome 等盖住后再点「日志窗口」能置顶并跨 Space 出现；首次启动出现引导弹窗且勾选后不再提示；⌘Q 退出后系统代理还原、无残留进程。

## 假设与默认

- 安卓沿用 `minSdk 28 / targetSdk 34`，本轮只在 Android 14 真机验收；内嵌服务器需要它自己的运行时实例，故放在新 crate 中（不动 `phantom-client` 依赖图）。
- 直连 DNS 优先用 JNI `VpnService.protect()`（全 API 可用），API 33+ 额外加排除路由作为冗余。
- 安卓白名单与鸿蒙一致：内置 FST 清单开箱即用，不做 App 内编辑。
- 鸿蒙夜间模式基于资源限定符 + `setColorMode`，不引入第二套手写色板对象。
- mac 只保留日志窗口一个真实窗口，其余 UI 全部收敛进菜单栏弹窗与 sheet。
