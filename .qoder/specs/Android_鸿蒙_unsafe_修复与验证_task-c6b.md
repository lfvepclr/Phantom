# Phantom Android + HarmonyOS NEXT 客户端融合计划

## 背景与目标

在上一个阶段，我们已完成：
- Android 客户端 Rust FFI / JNI 桥接、Kotlin ViewModel、VpnService 前台服务、Material3 UI、自动构建脚本。
- HarmonyOS NEXT 工程骨架（NAPI Rust 模块、ArkUI 页面、VpnExtensionAbility 占位、构建脚本）。

本阶段目标：
1. **修复项目中的 `unsafe` 代码**，尽量减少裸指针、重复 JNI 桥接、`CString` 手动释放等高风险操作，避免内存泄漏与 UB。
2. **完成鸿蒙系统的控制面与数据面设计**：高性能、低能耗，借鉴 Android/macOS 经验。
3. **制定完整的验证方案**：主机静态检查、虚拟机/模拟器验证、CI 自动化、真机手动测试清单。
4. **补齐验证前置条件**：安装 Android NDK、模拟器、HarmonyOS DevEco 工具链等。
5. **补齐客户端 README 文档**：将编译方式、分层架构、框架、交互流程、功能模块、TODO 写入每个客户端 README.md。

---

## Task 0: 前置条件检查与补齐

### 0.1 当前环境检查结果（macOS 主机）

| 项目 | 状态 | 说明 |
|------|------|------|
| `cargo check --workspace` | ✅ 通过 | 现有 Rust 代码可编译 |
| `cargo-ndk` | ❌ 未安装 | 无法交叉编译 Android cdylib |
| Android SDK / NDK | ❌ 未安装 | `ANDROID_NDK_HOME` 为空 |
| Android 模拟器 / adb | ❌ 未安装 | 无法运行 instrumented 测试 |
| Android Rust targets | ❌ 未安装 | `aarch64-linux-android` 等缺失 |
| DevEco Studio | ✅ 已安装 | `DevEco Studio 26.0.0 Beta1` |
| HarmonyOS SDK | ✅ 已安装 | `/Users/spencer/Library/Huawei/Sdk` |
| HarmonyOS 模拟器 | ✅ 已部署 | `/Users/spencer/.Huawei/Emulator/deployed` |
| ohos-rs / cargo-ohos | ❌ 待确认 | 需验证是否已安装 |
| GitHub Actions CI | ❌ 未配置 | 无自动化 Android/HarmonyOS 构建 |

### 0.2 补齐步骤

1. **Android 工具链**
   - 安装 Android Studio → SDK Manager → 安装 NDK (r26b+)、Android 14 SDK、模拟器、平台工具。
   - 设置环境变量：`ANDROID_HOME`、`ANDROID_NDK_HOME`。
   - `cargo install cargo-ndk`。
   - `rustup target add aarch64-linux-android armv7-linux-androideabi i686-linux-android x86_64-linux-android`。

2. **HarmonyOS 工具链**
   - DevEco Studio `26.0.0 Beta1` 已安装，无需重复安装。
   - HarmonyOS SDK 已位于 `/Users/spencer/Library/Huawei/Sdk`，DevEco Studio 内确认 API 12+ 已下载。
   - 模拟器镜像已部署在 `/Users/spencer/.Huawei/Emulator/deployed`，支持命令行启动与调试。
   - 安装/验证 ohos-rs / cargo-ohos 工具链（用于 NAPI 编译）。
   - 命令行启动 HarmonyOS 模拟器（参考[官方文档](https://developer.huawei.com/consumer/cn/doc/harmonyos-guides/ide-emulator-command-line)）：
     ```bash
     # 进入 DevEco Studio 的 emulator 工具目录（示例路径，按实际安装位置调整）
     cd /Applications/DevEco-Studio.app/Contents/tools/emulator
     # 查看已部署的模拟器
     ./emulator -list-avds
     # 启动指定模拟器（name 替换为实际 AVD 名称）
     ./emulator -avd <avd_name>
     # 或后台启动 + 网络桥接（按需）
     ./emulator -avd <avd_name> -netdir /Users/spencer/.Huawei/Emulator/deployed -no-window
     ```
     也可通过 HDC 命令验证设备连接：
     ```bash
     /Users/spencer/Library/Huawei/Sdk/hmscore/TOOLCHAIN/bin/hdc list targets
     ```

3. **CI 准备**
   - 新增 `.github/workflows/mobile.yml`：
     - 在 Ubuntu/macOS runner 上安装 Android NDK、cargo-ndk。
     - 构建 `phantom-client` Android cdylib。
     - 运行 JVM unit tests。
     - 启动 Android 模拟器运行 instrumented tests（缓存 AVD）。
     - 构建 HarmonyOS NAPI 模块（若 runner 支持）。

---

## Task 1: `unsafe` 代码审计与修复

### 1.1 审计结果

| 文件 | 位置 | `unsafe` 模式 | 风险 | 修复策略 |
|------|------|---------------|------|----------|
| `client/src/platform/android.rs` | 121-148 | `unsafe extern "C"` + `from_raw_parts` | 输入指针合约依赖调用方 | 保留 `unsafe` 签名（FFI 边界不可避免），内部使用 `CStr`/`slice` 后立刻复制到 Rust 内存 |
| `client/src/platform/android.rs` | 396-426, 559-588 | `CString::into_raw` / `from_raw` | 调用方忘记释放导致泄漏；重复释放导致 UB | 引入 `SafeCString` RAII 包装器，封装 `into_raw` 与 `free` |
| `client/src/platform/android.rs` | 646-781 | 重复 legacy JNI 桥接，`JNIEnv::from_raw`、`JString::from_raw` | 与上方现代 JNI 桥接重复；raw env 转换容易生命周期出错 | **删除 legacy 模块**，统一使用 `JNIEnv<'local>` 现代 API |
| `client/src/platform/macos.rs` | 76-138, 422-451, 471-475 | 同上 FFI 指针/CString | 与 Android 一致 | 复用/对齐 `SafeCString`，统一 macOS 字符串返回接口 |
| `client/src/tun.rs` | 82-86 | `OwnedFd::from_raw_fd(fd)` | fd 无效时产生未定义行为 | 封装为 `TunDevice::from_fd(fd)`，在文档中声明安全前提；调用前由 Kotlin/HarmonyOS 保证 fd 有效 |
| `client/src/tun.rs` | 104-110, 137-143 | `libc::read` / `libc::write` | 裸 syscall，需保证 buffer 与 fd 有效 | 封装在 `AsyncFd` ready 之后调用，使用 Rust slice 边界；无法完全避免，但把 unsafe 集中在最小范围 |
| `client/harmony/rust/src/lib.rs` | 21-29, 36-38, 61, 85 | 调用 `unsafe extern "C"` ABI | NAPI 宏本身安全，但包装层仍暴露 unsafe | 在 `android.rs` 内新增 **safe wrapper**（非 unsafe），HarmonyOS 直接调用 safe wrapper |

### 1.2 具体修复

#### 1.2.1 删除 Android 重复 JNI 桥接

- **文件**：`client/src/platform/android.rs`
- **操作**：删除 `mod jni_bridge`（约 646-781 行）。
- **原因**：该模块使用 `*mut jni::sys::JNIEnv` + `JNIEnv::from_raw` + `JString::from_raw`，与上方使用 `JNIEnv<'local>` 的现代桥接功能完全重复。保留现代版本即可。

#### 1.2.2 引入 `SafeCString` RAII 包装器

- **新增位置**：`client/src/platform/android.rs` 顶部（或独立 `ffi_string.rs`）。
- **设计**：
  ```rust
  struct SafeCString(std::ffi::CString);
  impl SafeCString {
      fn new(s: impl Into<Vec<u8>>) -> Option<Self> { ... }
      fn leak(self) -> *mut c_char { self.0.into_raw() }
      unsafe fn reclaim(ptr: *mut c_char) {
          if !ptr.is_null() { let _ = CString::from_raw(ptr); }
      }
  }
  ```
- **替换范围**：`phantom_android_get_last_error`、`phantom_android_get_logs`、`phantom_macos_get_logs`、`phantom_macos_get_last_error`、`phantom_harmony_get_last_error`。

#### 1.2.3 Android FFI 增加 safe wrapper

- **文件**：`client/src/platform/android.rs`
- **操作**：在 `unsafe extern "C"` 函数之上，新增 pub safe 函数：
  - `pub fn android_start_with_uri(fd: RawFd, uri: &str, mode: &str) -> i32`
  - `pub fn android_start(fd: RawFd, config: &str) -> i32`
  - `pub fn android_get_logs(since: u64) -> (Vec<String>, u64)`
- 这样 HarmonyOS NAPI 和 Rust 内部测试都无需写 `unsafe` 块。

#### 1.2.4 统一 macOS 字符串接口

- **文件**：`client/src/platform/macos.rs`
- **操作**：复用 `SafeCString`，确保 `phantom_macos_get_logs` / `phantom_macos_get_last_error` / `phantom_macos_free_logs` 与 Android 接口行为一致。

#### 1.2.5 TUN fd 安全封装

- **文件**：`client/src/tun.rs`
- **操作**：
  - 保持 `TunDevice::from_fd(fd: RawFd)` 作为唯一入口，内部 `unsafe { OwnedFd::from_raw_fd(fd) }`。
  - 在文档中明确：调用方必须提供已 `detachFd()` 后的有效 fd；Rust 取得所有权，停止时由 `OwnedFd` Drop 关闭。
  - `libc::read/write` 保留，但封装在 `AsyncFd` ready 之后，使用 `&mut [u8]` / `&[u8]` 边界，避免越界。

### 1.3 验收标准

- `cargo clippy -p phantom-client -- -D warnings` 通过（unsafe 块必须有 SAFETY 注释）。
- 删除 legacy JNI 模块后，`cargo check -p phantom-client` 仍通过。
- `cargo test -p phantom-client` 通过。

---

## Task 2: 鸿蒙 HarmonyOS NEXT 控制面与数据面设计

### 2.1 总体架构

```text
┌─────────────────────────────────────────────────────────────┐
│  ArkTS UI (entry/src/main/ets/pages/Index.ets)              │
│  - 输入 phantom:// URI / 模式                                │
│  - 显示状态、日志、流量统计                                   │
│  - 点击 Start → 拉起 VpnExtensionAbility                    │
└──────────────┬──────────────────────────────────────────────┘
               │ NAPI (控制面：start/stop/status/logs/config)
               ▼
┌─────────────────────────────────────────────────────────────┐
│  Rust NAPI module (client/harmony/rust/src/lib.rs)          │
│  - 复用 phantom_client::platform::android 安全封装            │
│  - 控制面方法均用 #[napi] 暴露给 ArkTS                        │
└──────────────┬──────────────────────────────────────────────┘
               │ 调用 safe Rust wrapper
               ▼
┌─────────────────────────────────────────────────────────────┐
│  Rust tunnel core (phantom-client)                           │
│  - Hello 验证、SOCKS5、TUN proxy、DNS hijack、failover        │
│  - 所有加密/网络 I/O 全部在 Rust 内完成                        │
└──────────────┬──────────────────────────────────────────────┘
               │ TUN fd (仅一次传递)
               ▼
┌─────────────────────────────────────────────────────────────┐
│  HarmonyOS VpnExtensionAbility                                │
│  - 创建 TUN 接口、配置路由/DNS                                 │
│  - 将 fd 交给 Rust                                            │
└─────────────────────────────────────────────────────────────┘
```

### 2.2 控制面设计（低功耗）

| 方法 | 方向 | 频率 | 说明 |
|------|------|------|------|
| `phantomHarmonyStart(fd, uri, mode)` | ArkTS → Rust | 单次 | 启动隧道；fd 由 VpnExtensionAbility 提供 |
| `phantomHarmonyStop()` | ArkTS → Rust | 单次 | 停止隧道，释放 Runtime 和 fd |
| `phantomHarmonyGetStatus()` | ArkTS ← Rust | 轮询 500 ms | 状态机：0 idle / 1 starting / 2 running / 3 error |
| `phantomHarmonyGetLastError()` | ArkTS ← Rust | 状态为 3 时读取 | 错误信息 |
| `phantomHarmonyGetLogs(since)` | ArkTS ← Rust | 轮询 1000 ms | 批量返回新增日志行 + 新 cursor |
| `phantomHarmonyGetStats()` | ArkTS ← Rust | 轮询 2000 ms（可选） | 返回字节数、连接数等（低功耗场景可关闭） |

- **事件替代高频轮询（后续优化）**：Rust 侧维护一个 `ControlEventChannel`，ArkTS 订阅后状态变化即时推送；初始版本先用轮询降低复杂度。
- **批量日志**：Rust `LOG_BUFFER` 每次最多返回 50 条新增日志，避免 ArkTS 层频繁刷新 UI。
- **后台保活**：VpnExtensionAbility 作为系统 VPN 扩展，天然具有前台服务能力；ArkTS UI 切后台不影响 Rust 数据面。

### 2.3 数据面设计（高性能 + 低能耗）

1. **零拷贝/少拷贝**
   - TUN 读取使用 `BytesMut` 池化复用，避免每次分配 1500B。
   - 数据包在 Rust 内完成解析、规则匹配、加密、转发，不经过 ArkTS/NAPI。

2. **批量与批处理**
   - TUN read 每次读取一个完整 IP 包（受 MTU 限制）。
   - 当流量低时，`AsyncFd` 基于 epoll 边缘触发，无包时线程阻塞，不空转。

3. **Adaptive polling / 闲时降频**
   - 长连接（如 QUIC/TCP 隧道）使用 `tokio::select!` 等待可读事件，无需轮询。
   - 健康检查、日志刷新使用较长间隔（5s/10s）。
   - 可考虑在空闲时降低 `tokio` worker 线程数，但默认 `tokio::runtime::Runtime` 已使用 work-stealing，无需额外处理。

4. **DNS 优化**
   - 复用 Android 设计：UDP:53 被 TUN 拦截后，通过 `DnsProxy` 走 DoT/DoH 上游，缓存解析结果，减少重复查询能耗。

5. **流量统计**
   - 使用原子计数器（`TrafficStats`）记录上下行字节/包数，UI 低频拉取（≥2s），避免 per-packet NAPI。

### 2.4 文件变更

- `client/src/platform/android.rs`：新增 safe wrapper，使 HarmonyOS 无需 unsafe 调用。
- `client/harmony/rust/src/lib.rs`：改为调用 safe wrapper；删除内部 unsafe 块。
- `client/harmony/entry/src/main/ets/pages/Index.ets`：完善 UI（状态、日志、错误、统计）。
- `client/harmony/entry/src/main/ets/vpnextability/PhantomVpnExtensionAbility.ets`：实现 TUN 创建与 fd 传递。
- `client/harmony/entry/src/main/module.json5`：声明 `extensionAbility` 为 vpn。
- `client/harmony/README.md`：补充控制面/数据面设计说明。

### 2.5 验收标准

- `cargo check -p phantom-harmony` 通过（需先将 harmony 加入 workspace）。
- NAPI 模块不再包含手写 `unsafe` 块。
- ArkTS 侧无 per-packet 调用 Rust 的逻辑。

---

## Task 3: Android 验证计划

### 3.1 主机静态验证

| 检查项 | 命令 | 通过标准 |
|--------|------|----------|
| 编译 | `cargo check --workspace` | 无 error |
| 单元测试 | `cargo test -p phantom-client` | 全部通过 |
| Clippy | `cargo clippy -p phantom-client -- -D warnings` | 无 warning |
| 格式化 | `cargo fmt --check` | 无差异 |
| 依赖审计 | `cargo audit` | 无高危漏洞 |

### 3.2 Android 模拟器验证

1. **环境准备**
   - 安装 Android Studio + NDK + 模拟器。
   - 创建 API 34 arm64 AVD。
   - 运行 `scripts/build-android.sh` 生成 APK。

2. **安装与启动**
   ```bash
   adb install -r client/android/app/build/outputs/apk/debug/app-debug.apk
   adb shell am start -n co.phantom.android/.MainActivity
   ```

3. **功能测试矩阵**

| 场景 | 输入 | 预期结果 |
|------|------|----------|
| 正确连接 | 有效 phantom:// URI | 状态 Connected，日志 Hello verification passed |
| 错误端口 | URI 端口为 9999 | 状态 Error，日志 Connection refused |
| 错误 key | URI key 随机 | 状态 Error，日志 Handshake failed |
| 无外网验证 | 服务端 verification_url 无效 | 状态 Error，日志 Hello verification failed |
| 后台保活 | 连接后按 Home 键 5 分钟 | 通知仍在，VPN 图标仍在 |
| 停止 | 点击 Stop | 通知消失，状态 Idle |

4. **Instrumented tests**
   - `client/android/app/src/androidTest/java/co/phantom/android/RustBridgeInstrumentedTest.kt`
   - 运行：`./gradlew connectedAndroidTest`

### 3.3 真机验证

- 使用 Pixel / 小米 / 华为等主流机型（Android 12-14）。
- 重复 3.2 功能矩阵。
- 额外测试：锁屏、飞行模式切换、多网络切换（Wi-Fi ↔ 蜂窝）。

### 3.4 CI 自动化

- `.github/workflows/mobile.yml`：
  - 安装 cargo-ndk、Android NDK。
  - 构建 Android cdylib（arm64）。
  - 运行 `cargo test -p phantom-client`。
  - 启动 AVD 运行 `./gradlew connectedAndroidTest`（使用 reactivecircus/android-emulator-runner）。

---

## Task 4: HarmonyOS 验证计划

### 4.1 主机静态验证

- 将 `client/harmony` 加入 workspace `Cargo.toml`。
- `cargo check -p phantom-harmony` 通过。
- `cargo clippy -p phantom-harmony` 通过。

### 4.2 DevEco Studio 模拟器验证

#### 方式 A：DevEco Studio GUI

1. 用 DevEco Studio 打开 `client/harmony`。
2. 编译 Rust NAPI：`scripts/build-harmony.sh`。
3. 选择已部署的 HarmonyOS 模拟器（API 12）并运行。
4. 安装 HAP，启动应用。
5. 输入 phantom:// URI，点击连接。
6. 观察状态变化与日志。

#### 方式 B：命令行模拟器 + HDC（推荐 CI / 自动化）

参考[华为官方命令行文档](https://developer.huawei.com/consumer/cn/doc/harmonyos-guides/ide-emulator-command-line)：

1. **启动模拟器**
   ```bash
   # 查看可用 AVD
   /Applications/DevEco-Studio.app/Contents/tools/emulator/emulator -list-avds

   # 启动指定 AVD（替换 <avd_name>）
   /Applications/DevEco-Studio.app/Contents/tools/emulator/emulator -avd <avd_name>
   ```

2. **等待模拟器就绪**
   ```bash
   # 轮询直到设备出现
   /Users/spencer/Library/Huawei/Sdk/hmscore/TOOLCHAIN/bin/hdc list targets
   ```

3. **编译并安装 HAP**
   ```bash
   # 构建 HAP（Debug）
   cd client/harmony
   ./scripts/build-harmony.sh
   # 使用 hvigor 或 hdc 安装产物（示例）
   /Users/spencer/Library/Huawei/Sdk/hmscore/TOOLCHAIN/bin/hdc app install entry/build/default/outputs/default/entry-default-signed.hap
   ```

4. **启动应用**
   ```bash
   /Users/spencer/Library/Huawei/Sdk/hmscore/TOOLCHAIN/bin/hdc shell am start -a ohos.want.action.home -b com.phantom.harmony -m EntryAbility
   ```

5. **查看日志**
   ```bash
   # 实时抓取应用日志
   /Users/spencer/Library/Huawei/Sdk/hmscore/TOOLCHAIN/bin/hdc hilog | grep -i phantom
   ```

6. **验证矩阵**

| 场景 | 输入 | 预期结果 |
|------|------|----------|
| 正确连接 | 有效 phantom:// URI | 状态 Running，Hilog 输出 Hello verification passed |
| 错误端口 | URI 端口为 9999 | 状态 Error，日志 Connection refused |
| 错误 key | URI key 随机 | 状态 Error，日志 Handshake failed |
| 后台保活 | 连接后按 Home 键 5 分钟 | VpnExtensionAbility 仍在运行，通知/状态图标保持 |
| 停止 | 点击 Stop | 状态 Idle，TUN 接口移除 |

### 4.3 真机验证

- 使用 HarmonyOS NEXT 真机（需申请开发者签名）。
- 验证 TUN 创建、路由下发、DNS 设置。
- 验证后台保活、电量消耗。

---

## Task 5: 客户端 README 文档化

将每个客户端的**编译方式、分层架构、使用的框架、交互流程、功能模块、TODO**写入对应 README.md，结构对齐主工程 README.md，方便后续模型/开发者快速理解。

### 5.1 Android 客户端

- **目标文件**：`client/android/README.md`
- **需补充章节**：
  1. **编译方式**：`scripts/build-android.sh`、`cargo-ndk` 手动构建、Gradle 构建。
  2. **分层架构**：Kotlin VpnService → JNI → Rust tunnel core → TUN/网络 I/O。
  3. **使用的框架**：Jetpack Compose Material3、Lifecycle ViewModel、VpnService、Rust `jni 0.21`、tokio。
  4. **交互流程**：App 启动 → 输入 URI/模式 → 请求 VPN 权限 → VpnService 创建 TUN fd → JNI 启动 Rust 核心 → Hello 验证 → 状态/日志轮询 → Stop。
  5. **功能模块**：
     - `MainActivity.kt`：Compose UI、权限回调。
     - `PhantomTunnelViewModel.kt`：状态机 + 日志轮询。
     - `PhantomVpnService.kt`：前台服务 + TUN fd 传递。
     - `RustBridge.kt`：JNI 声明。
  6. **TODO**：真机/模拟器验证、instrumented tests、多 ABI 构建、CI 自动化。

### 5.2 HarmonyOS NEXT 客户端

- **目标文件**：`client/harmony/README.md`
- **需补充章节**：
  1. **编译方式**：`scripts/build-harmony.sh`、DevEco Studio 构建 HAP、命令行模拟器启动。
  2. **分层架构**：ArkTS UI → NAPI → Rust tunnel core → VpnExtensionAbility TUN fd。
  3. **使用的框架**：ArkUI、HarmonyOS NAPI、`napi-ohos`、tokio、Rust tunnel core。
  4. **交互流程**：App 启动 → 输入 URI/模式 → 拉起 VpnExtensionAbility → 创建 TUN fd → NAPI 启动 Rust 核心 → Hello 验证 → 状态/日志轮询 → Stop。
  5. **功能模块**：
     - `entry/src/main/ets/pages/Index.ets`：ArkUI 主界面。
     - `entry/src/main/ets/vpnextability/PhantomVpnExtensionAbility.ets`：VPN 扩展能力。
     - `rust/src/lib.rs`：NAPI 桥接。
  6. **TODO**：真机签名验证、VpnExtensionAbility TUN 创建、电量测试、NAPI 事件通道优化。

### 5.3 macOS 客户端

- **目标文件**：`client/mac/README.md`
- **需补充章节**：
  1. **编译方式**：`scripts/build-mac.sh`、Xcode + Swift Package Manager。
  2. **分层架构**：SwiftUI / MenuBarExtra → C FFI → Rust tunnel core → self-created TUN。
  3. **使用的框架**：SwiftUI、Swift Package Manager、Rust cdylib、tokio。
  4. **交互流程**：App 启动 → 输入 URI/模式 → C FFI 启动 Rust 核心 → Hello 验证 → 系统代理设置 → 状态/日志轮询 → Stop。
  5. **功能模块**：
     - `PhantomMacApp.swift` / `PhantomTunnel.swift`：SwiftUI 与状态管理。
     - `Bridge.swift`：C FFI 声明与封装。
     - `SystemProxy.swift`：系统代理切换。
  6. **TODO**：DMG 打包、ad-hoc 签名、菜单栏交互优化。

### 5.4 验收标准

- 三个 README.md 均包含上述 6 个章节。
- 章节标题、术语与主工程 `README.md` 保持一致。
- 代码路径使用相对路径，命令可直接复制运行。

---

## Task 6: 实施顺序

1. **Task 0**：补齐 Android/HarmonyOS 工具链与 CI 配置。
2. **Task 1.1**：删除 Android legacy JNI 桥接，验证编译。
3. **Task 1.2**：引入 `SafeCString` 并替换 Android/macOS/HarmonyOS 字符串分配。
4. **Task 1.3**：为 Android FFI 增加 safe wrapper。
5. **Task 1.4**：`cargo clippy -D warnings` 清理。
6. **Task 2.1**：将 `client/harmony` 加入 workspace，HarmonyOS NAPI 改为调用 safe wrapper。
7. **Task 2.2**：完善 HarmonyOS ArkUI 与 VpnExtensionAbility。
8. **Task 5**：更新 Android / HarmonyOS / macOS 客户端 README.md。
9. **Task 3-4**：在模拟器/真机上执行 Android 与 HarmonyOS 验证。
10. **Task 6**：CI 收尾。

---

## Task 7: 关键设计决策

- **unsafe 最小化**：FFI 边界（C ABI）不可避免需要 `unsafe extern "C"`，但内部逻辑、字符串分配、fd 包装全部使用 Rust 安全抽象封装。
- **删除重复 JNI 桥接**：现代 `JNIEnv<'local>` API 更安全、更符合 `jni 0.21` 最佳实践。
- **HarmonyOS 复用 Android 核心**：通过 safe wrapper 让 HarmonyOS NAPI 零 unsafe 调用 Rust 隧道核心。
- **控制面轮询而非回调**：初始版本使用 500ms-1000ms 轮询，降低 NAPI 线程模型复杂度；后续可迁移到事件通道。
- **数据面零 NAPI 交叉**：TUN fd 一次传递后，所有包处理在 Rust 内完成。
- **低功耗**：`AsyncFd` 事件驱动、批量日志、低频统计、DNS 缓存。

---

## Task 8: 风险与待确认事项

1. **Android 工具链缺失**：当前主机缺少 Android SDK/NDK/cargo-ndk，Android 端静态修复后可先使用 `cargo test` 验证；真机/模拟器验证需在补齐 Android 环境后进行。HarmonyOS 端 DevEco Studio、SDK、模拟器已安装，可直接进行命令行模拟器调试。
2. **HarmonyOS NAPI 成熟度**：`napi-ohos` 0.1 版本较新，需确认 `i32` fd、`String` 返回在真机上的行为。
3. **VpnExtensionAbility API**：HarmonyOS NEXT 的 VPN 扩展能力文档有限，需根据实际 SDK 调整 TUN fd 获取方式。
4. **电量测试**：需要真机长时间运行采集功耗数据，模拟器无法完全替代。
5. **CI 模拟器稳定性**：Android instrumented tests 在 GitHub Actions 上可能较慢，建议缓存 AVD 镜像。
