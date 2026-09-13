# Phantom 三端计划收尾：安卓扫码兜底与服务器 URI、鸿蒙设置面板、构建脚本与文档对齐

## 摘要

对《Phantom：安卓端全量对齐鸿蒙 + 鸿蒙/安卓夜间模式 + macOS 弹窗化与日志窗口修复》做完成度核查后的**收尾补齐计划**。服务端与隧道协议仍零改动。

核查结论：四块主体（A 安卓对齐鸿蒙 / B 三态夜间模式 / C macOS 弹窗化 / D 工具链与文档）**绝大部分已落地**，本计划只处理未落地与部分落地的缺口：

| 组 | 缺口 | 证据 |
|---|---|---|
| A | `serversUri` JNI 缺失，页面重建后取不回内嵌服务器 URI | 全仓 0 命中；`ServerScreen.kt:57` 仅 `remember` |
| A | 扫码「从相册选择」被 `if (hasPermission)` 包裹，权限被拒时不可达 | `ScanScreen.kt:247` vs 拒绝态 `:200-216` |
| A | 扫码页无「手动输入连接串」兜底 | `ScanScreen.kt` 全文无 `TextField` |
| A | 内嵌服务器页加密/协议是自由文本框（鸿蒙是预设胶囊），workDir 未加 `/server` | `ServerScreen.kt:107-122, 132` |
| B | 鸿蒙设置面板缺「导出日志」与「版本」行；齿轮是文本字形 `⚙` | `Index.ets:1550-1626, 1655` |
| B | 安卓深色模式状态栏/系统栏未跟随主题（无 themes.xml / WindowCompat） | `AndroidManifest.xml:23,28` |
| C | 弹窗高度固定 620 而非自适应（`.sheet` 与日志窗口置顶已用更优方案替代，不回退） | `Theme.swift:20` |
| D | `JAVA_HOME` 仅硬编码 Android Studio jbr 一条路径；无 NDK 最低版本校验；README 的 `ALL_TARGETS=1` 分支不存在 | `build-android.sh:125-127`；`android/README.md:224` |
| D | 根 README 缺「夜间模式 / 设置面板」，缺安卓构建前置与 `adb install` 步骤 | `README.md:34, 595` |
| D | `client/mac/README.md:5` 残留「日志放在独立窗口」旧描述，与 `:14`、`:23-25` 矛盾 | — |
| E | 安卓插桩测试 `RustBridgeInstrumentedTest.kt` 仍 TODO，CI 未接 | `android/README.md:345, 347` |

## 关键改动

### A. 安卓端

- **A-1 `serversUri`**：`client/android/rust/src/lib.rs` 新增 `static SERVER_URI: Mutex<String>`，`serverStart` 成功时写入、`serverStop` 时清空、退出分支一并清空；新增 JNI `Java_co_phantom_android_RustBridge_serversUri` 幂等 getter。`RustBridge.kt` 同步 `external fun serversUri(): String`。`ServerScreen.kt` 的 URI 改 `rememberSaveable`，并在进入页面/状态变为 RUNNING 时从 `RustBridge.serversUri()` 恢复，解决「重建后 URI 丢失」。
- **A-2 服务器页预设化**：`ServerScreen.kt` 的 cipher/proto 从 `OutlinedTextField` 改为与鸿蒙 `ServerPage.ets:145-153` 同款的胶囊按钮组（cipher: `auto`/`aes-256-gcm`/`chacha20-poly1305`；proto: `tcp`/`quic`），把非法值挡在 UI 层；workDir 改为 `filesDir/server`，与鸿蒙一致。
- **A-3 扫码兜底**：`ScanScreen.kt` 把「从相册选择」移出 `if (hasPermission)`（相册不需要 CAMERA 权限）；权限拒绝态在「授予相机权限」之外新增「手动输入连接串」入口，复用 `ServerLink.parse` 校验 `phantom://`，写入偏好后返回（不自动连接）。
- **A-4 深色系统栏**：新增 `res/values/themes.xml` + `res/values-night/themes.xml`（DayNight 主题），`AndroidManifest.xml` 引用自定义主题；`MainActivity` 用 `WindowCompat` + `isAppearanceLightStatusBars` 跟随 `ThemeMode`，判定源与 `PhantomTheme.resolveDarkMode` 一致。

### B. 鸿蒙端

- `Index.ets` 的 `settingsSheet()` 增加「导出日志」（复用 `systemShare`，日志文件路径与 Rust 侧 `setLogPath`/`setTracePath` 写入路径一致，避免分享不存在的文件）与「版本」行（`bundleManager.getBundleInfoForSelf()` 取 `versionName`）。
- 齿轮入口从 `Text('⚙')` 换成图标资源，与安卓 `Icons.Filled.Settings` 视觉对齐。

### C. macOS

- 仅修 `client/mac/README.md:5` 的矛盾描述。**不回退**现有实现：`PopoverPage` 弹窗内推页与 `ExpandedLogPage` 已在代码注释与 README 中说明理由（弹窗失焦自关会把 sheet 困在已消失宿主窗口上；日志窗口置顶机制已连同窗口一起删除）。
- 弹窗高度自适应列为可选改进（当前固定 620 + 内部分区滚动）。

### D. 工具链与文档

- `scripts/build-android.sh`：`JAVA_HOME` 探测链 `JAVA_HOME` → `/usr/libexec/java_home` → Android Studio jbr → `java -XshowSettings` 推导 → Linux `/usr/lib/jvm`，首个可用即停；NDK 增加最低版本比较（`26.1.10909125`）并给出可复制的 `sdkmanager` 安装命令；补齐或删除文档中的 `ALL_TARGETS=1` 分支。
- 根 `README.md`：补安卓构建前置（NDK / `rustup target add aarch64-linux-android` / `local.properties` / `JAVA_HOME`）、`adb install`、产物路径；新增「夜间模式 / 设置面板」能力说明。
- `client/android/README.md`：同步新 JNI、NDK 硬性要求、扫码兜底路径。
- `.cargo/config.toml` 补 `[target.aarch64-linux-android]` 段列为**可选**（脚本以环境变量注入已能工作）。

## 接口与契约变更

- 新 JNI：`serversUri(): String`（幂等，未启动时返回空串）。
- 鸿蒙新增资源：齿轮图标、版本字符串读取（不改变既有 preferences 键）。
- 安卓新增资源：`values/themes.xml`、`values-night/themes.xml`。
- 偏好键、隧道/服务端协议、分流语义、mac 系统代理行为**全部不变**。

## 测试与验收

- 自动化：`cargo check --workspace --all-targets`、`cargo test -p phantom-client --lib`、`./gradlew :app:test`、`swift test`。
- 安卓真机（`adb devices` 在线机，Android 14 / SDK 34）：`cargo xtask build android --debug` → `adb install -r` → 授权 VPN 后出钥匙图标 → google.com 可访问、`v.youku.com` 直连 → 内嵌服务器页：预设胶囊启停、URI + 二维码 + 复制/分享、退出页面再进入仍能取回 URI → 扫码页：拒绝相机权限后相册与手输均可用 → 深色模式下状态栏跟随。
- 鸿蒙真机：设置面板新增项可用；浅/深/跟随三态无浅色残留。
- mac：二级页与日志放大仍正常；README 无矛盾描述。

## 假设与默认

- macOS 的 `.sheet` 与日志窗口置顶视为「以更优方案达成目标」，不回退实现。
- `serversUri` 只解决进程内恢复，不做跨进程持久化（内嵌服务器本就随进程生命周期）。
- 鸿蒙导出日志复用现有 `systemShare`，不引入新依赖。
- `.cargo/config.toml` 安卓段为可选项，不阻塞主流程。
- 本轮不动 `core/` 与 `server/`，不改协议字段；`themeMode` 三端保持同名同义。
