# Phantom 服务端 QR 码 + macOS 客户端自动化构建

## Context

Phantom 已实现 `phantom server` 命令行的零配置自举：启动时打印启动摘要并输出 `phantom://...` URI 链接。但当前分享 URI 只能从终端复制粘贴，手机端接入门槛较高。本计划解决两个问题：

1. **服务端**：在 `phantom server` 启动摘要中增加 QR 码渲染（unicode 块字符），手机扫码即可导入 URI，无需复制粘贴。覆盖 auto / interactive / load 三种入口。
2. **macOS 客户端**：`client/mac/README.md` 当前要求手动建 Xcode 工程才能运行；现有 `client/Cargo.toml` 没有声明 `crate-type = ["cdylib"]`，产物只有 `rlib`，Swift 端无法通过 dylib 解析 Rust 符号。本计划新增 `scripts/build-mac.sh` 一键构建 `.app` Bundle，并补齐 cdylib 产物类型。

用户已确认的设计点：
- QR 码始终打印（不判断 TTY，不分模式）
- QR 码紧跟 `URI link` 行下方
- macOS 客户端走自动化构建脚本路线（不是仅文档说明）

---

## 改动清单

### 任务 1：服务端启动摘要 QR 码

| 文件 | 改动类型 | 说明 |
|---|---|---|
| [Cargo.toml](file:///Users/spencer/workspace/qoder/phantom/Cargo.toml) | 编辑 | `[workspace.dependencies]` 段加 `qr2term = "0.3"` |
| [server/Cargo.toml](file:///Users/spencer/workspace/qoder/phantom/server/Cargo.toml) | 编辑 | `[dependencies]` 段加 `qr2term = { workspace = true }` |
| [server/src/bootstrap.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/bootstrap.rs) | 编辑 | 新增 `pub fn print_qr_code(uri: &str)`；改 `print_summary` 在 URI 行后调用 |
| [server/src/lib.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/lib.rs) | 编辑 | `run()` 函数捕获公钥、构造 URI、调用 `print_qr_code`（load 模式也要 QR） |

### 任务 2：macOS 客户端自动化构建

| 文件 | 改动类型 | 说明 |
|---|---|---|
| [client/Cargo.toml](file:///Users/spencer/workspace/qoder/phantom/client/Cargo.toml) | 编辑 | **前置**：加 `[lib] crate-type = ["rlib", "cdylib"]`，否则 dylib 产不出来 |
| [scripts/build-mac.sh](file:///Users/spencer/workspace/qoder/phantom/scripts/build-mac.sh) | 新建 | 自动化脚本（cargo + swiftc + Bundle + ad-hoc 签名） |
| [client/mac/README.md](file:///Users/spencer/workspace/qoder/phantom/client/mac/README.md) | 编辑 | Build 段改写，推荐脚本方式，保留手动备选 |
| [README.md](file:///Users/spencer/workspace/qoder/phantom/README.md) | 编辑 | macOS 客户端一句话前补构建指引段 |

### 任务 3：测试

| 文件 | 改动类型 | 说明 |
|---|---|---|
| [server/src/bootstrap.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/bootstrap.rs) `tests` 段 | 编辑 | 加一个 `print_qr_code_does_not_panic_on_valid_uri` 单测 |
| [tests/tests/cli_system.rs](file:///Users/spencer/workspace/qoder/phantom/tests/tests/cli_system.rs) | 编辑 | 追加断言 QR 字符（`\u{2588}` 全角方块）出现在 stdout |

---

## 关键实现细节

### 1.1 workspace + server Cargo 依赖

在根 [Cargo.toml](file:///Users/spencer/workspace/qoder/phantom/Cargo.toml#L17-L74) 的 `[workspace.dependencies]` 段追加：

```toml
# Terminal QR code rendering (used by server bootstrap)
qr2term = "0.3"
```

在 [server/Cargo.toml](file:///Users/spencer/workspace/qoder/phantom/server/Cargo.toml#L18-L26) `[dependencies]` 末尾追加：

```toml
qr2term = { workspace = true }
```

### 1.2 bootstrap.rs：新增 QR 打印函数 + 改 print_summary

在 [bootstrap.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/bootstrap.rs#L16-L30) `use` 块加：

```rust
use qr2term::print_qr;
```

新增 `pub fn print_qr_code`（放在 `print_summary` 上方或下方）：

```rust
/// Render the URI as a Unicode-block QR code on stdout.
///
/// Failure is non-fatal: a broken QR never blocks server bootstrap. We log
/// the error and let the operator copy the text URI from the line above.
pub fn print_qr_code(uri: &str) {
    if let Err(e) = print_qr(uri) {
        tracing::warn!("failed to render QR code: {e}");
    }
}
```

改 `print_summary`（[bootstrap.rs 第 504-521 行](file:///Users/spencer/workspace/qoder/phantom/server/src/bootstrap.rs#L504-L521)）：

```rust
fn print_summary(info: SummaryInfo<'_>) {
    let mode = if info.allowed_count == 0 {
        "OPEN (no whitelist)"
    } else {
        "WHITELIST"
    };
    println!();
    println!("=== Phantom server bootstrapped ===");
    println!("  key file     : {}", info.key_path.display());
    println!("  config file  : {}", info.toml_path.display());
    println!("  whitelist    : {} ({})", info.toml_path.display(), mode);
    println!("  bind address : {}", info.bind);
    println!("  URI link     : {}", info.uri);
    println!();
    println!("Scan the QR code below with a Phantom client to import the URI:");
    print_qr_code(info.uri);
    println!();
    println!("Or copy it manually:");
    println!("  phantom client --server \"{}\"", info.uri);
    println!();
}
```

不改 `SummaryInfo` 结构体（URI 已存在）。

### 1.3 lib.rs：load 模式也加 QR

`phantom_server::run` 当前把公钥丢弃为 `_public_key`（[lib.rs 第 43 行](file:///Users/spencer/workspace/qoder/phantom/server/src/lib.rs#L43)）。改为：

```rust
let (public_key, secret_key) = config.load_key_pair()?;
```

在该函数内 `BootstrapOptions` 构造之前，加一段 QR 输出（独立打印不复用 `print_summary`，因为 load 模式没有 key_path / allowed_count 语义）：

```rust
let uri = phantom_core::build_phantom_uri(
    &public_key,
    &config.bind,
    config.cipher,
    protocol,
    Some("default"),
);
println!();
println!("=== Phantom server bootstrapped (load mode) ===");
println!("  config file  : {}", config_path);
println!("  bind address : {}", bind);
println!("  URI link     : {}", uri);
println!();
println!("Scan the QR code below with a Phantom client to import the URI:");
crate::bootstrap::print_qr_code(&uri);
println!();
println!("Or copy it manually:");
println!("  phantom client --server \"{}\"", uri);
println!();
```

### 2.1 client/Cargo.toml 加 cdylib（前置）

[client/Cargo.toml](file:///Users/spencer/workspace/qoder/phantom/client/Cargo.toml) 末尾追加：

```toml
[lib]
name = "phantom_client"
crate-type = ["rlib", "cdylib"]
```

说明：
- `rlib` 保留以满足 [client/cli/Cargo.toml](file:///Users/spencer/workspace/qoder/phantom/client/cli/Cargo.toml) 内部 `phantom_client::PhantomClient` 调用
- `cdylib` 让 Swift 侧 dylopen 解析生效
- `name = "phantom_client"` 对齐 `Bridge.swift` 的符号前缀

### 2.2 新建 scripts/build-mac.sh

脚本结构（关键步骤）：

1. **参数解析**：默认 `release` + `aarch64-apple-darwin`；支持 `--debug` / `--arch x86_64` / `-h`
2. **cargo 编译 dylib**：
   ```bash
   cargo build --release --target aarch64-apple-darwin -p phantom-client --lib
   DYLIB_SRC="$ROOT/target/aarch64-apple-darwin/release/libphantom_client.dylib"
   ```
3. **swiftc 编译**：`swiftc -O -target arm64-apple-macos13.0 -framework SwiftUI -framework AppKit -framework Foundation -Xlinker -rpath -Xlinker '@executable_path/../Frameworks' -parse-as-library -o Phantom client/mac/*.swift`
4. **Bundle 组装**：
   ```
   Phantom.app/
   ├── Contents/
   │   ├── Info.plist                # 拷贝自 client/mac/Info.plist
   │   ├── MacOS/Phantom             # swiftc 产物
   │   └── Frameworks/libphantom_client.dylib  # cargo 产物
   ```
5. **ad-hoc 签名**：
   ```bash
   codesign --force --sign - "$FRAMEWORKS_DIR/libphantom_client.dylib"
   ```
6. **打印启动提示**：TUN 需要 root → `sudo open Phantom.app`

关键点：
- cargo 目标三元组是 `aarch64-apple-darwin`，swiftc 是 `arm64-apple-macos13.0`，脚本里做映射
- macOS 13.0 是 `MenuBarExtra` 最低要求（README 已声明）
- `--universal` 暂不实现（需要 lipo 合并两个架构的 cargo + swiftc 产物，超出本期范围）

### 2.3 改 client/mac/README.md

把现有 Build 段（[第 15-58 行](file:///Users/spencer/workspace/qoder/phantom/client/mac/README.md#L15-L58)）替换为：

````markdown
## Build

Use the helper script (recommended):

```bash
cd <repo-root>
scripts/build-mac.sh                # Apple Silicon release
scripts/build-mac.sh --debug        # debug build
scripts/build-mac.sh --arch x86_64  # Intel-only
```

Output: `Phantom.app` in the repo root.

### Prerequisites

- Rust toolchain (>= 1.85, edition 2024)
- macOS 13.0+ (required by `MenuBarExtra`)
- Xcode Command Line Tools (`xcode-select --install`) for `swiftc`

The script performs:

1. `cargo build -p phantom-client --lib` — produces `libphantom_client.dylib`
2. `swiftc` compiles all four `.swift` files into `Contents/MacOS/Phantom`
3. Bundles `Info.plist` and the dylib into `Phantom.app/Contents/`
4. Ad-hoc codesigns the dylib

### Launch

```bash
# TUN device creation requires root:
sudo open Phantom.app
# or, without sudo (no TUN, only SOCKS5):
open Phantom.app
```

The lightning-bolt icon appears in the menu bar; click it to enter a
`phantom://` URI and pick Global / Auto / Direct mode.

### Manual build (if you must)

[保留原 swiftc 命令对照版，供排错]
````

### 2.4 改根 README.md

在根 [README.md 第 48 行](file:///Users/spencer/workspace/qoder/phantom/README.md#L48) `macOS 原生客户端启动后...` 一句之前，插入：

````markdown
### macOS 客户端构建

SwiftUI 菜单栏客户端位于 `client/mac/`，依赖 `phantom-client` 的 cdylib。
项目自带 `scripts/build-mac.sh` 自动化整个流程：

```bash
scripts/build-mac.sh              # 默认 Apple Silicon release
```

产物为仓库根下的 `Phantom.app`。TUN 需要 root，请用 `sudo open Phantom.app` 启动。
完整说明见 `client/mac/README.md`。

````

### 3. 测试

#### 单元测试

在 [bootstrap.rs tests 段](file:///Users/spencer/workspace/qoder/phantom/server/src/bootstrap.rs#L595-L843) 末尾追加：

```rust
#[test]
fn print_qr_code_does_not_panic_on_valid_uri() {
    let uri = "phantom://YWJjZGVmZ2hpamtsbW5vcHFyc3R1dnd4eXoxMjM0NTY=@example.com:443?cipher=auto&proto=tcp#test";
    super::print_qr_code(uri);
}
```

只覆盖不 panic；QR 视觉正确性靠终端实测 + 手机扫码验证。

#### 集成测试（可选）

在 [tests/tests/cli_system.rs](file:///Users/spencer/workspace/qoder/phantom/tests/tests/cli_system.rs) 追加一个用例：跑 `phantom server --port 0`，断言 stdout 包含 `URI link` 和 `\u{2588}`（全角方块字符）。如果已有端到端断言模板，直接复用。

---

## 执行顺序

1. **服务端 QR**：先改 [Cargo.toml](file:///Users/spencer/workspace/qoder/phantom/Cargo.toml) + [server/Cargo.toml](file:///Users/spencer/workspace/qoder/phantom/server/Cargo.toml) 加依赖 → [bootstrap.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/bootstrap.rs) 加 `print_qr_code` + 改 `print_summary` → [lib.rs](file:///Users/spencer/workspace/qoder/phantom/server/src/lib.rs) load 模式补 QR → `cargo build --release -p phantom-server` 验证 → 跑 `phantom server` 肉眼检查终端
2. **macOS 前置**：[client/Cargo.toml](file:///Users/spencer/workspace/qoder/phantom/client/Cargo.toml) 加 `crate-type` → `cargo build --release -p phantom-client --lib` → 检查 `target/release/libphantom_client.dylib` 存在且 `nm` 能看到 `phantom_macos_*` 符号
3. **macOS 脚本**：新建 `scripts/build-mac.sh` → `chmod +x` → 在 macOS 上跑 `scripts/build-mac.sh` → 检查 `Phantom.app/Contents/{Info.plist,MacOS/Phantom,Frameworks/libphantom_client.dylib}` 都存在 → `sudo open Phantom.app` 启动看菜单栏图标
4. **文档**：改 `client/mac/README.md` + 根 `README.md`
5. **测试**：追加单测到 bootstrap.rs

---

## 端到端验证

```bash
# === 服务端 QR 码 ===
cd /tmp && mkdir phantom-qr-test && cd "$_"
/Users/spencer/workspace/qoder/phantom/target/release/phantom server --port 14443
# 预期：看到 URI link 行 + 下方 QR 码（unicode 块字符）+ 客户端命令
# 用手机扫码应能解析出 URI 文本
# Ctrl+C 退出

# === load 模式 QR ===
/Users/spencer/workspace/qoder/phantom/target/release/phantom server -c /Users/spencer/workspace/qoder/phantom/config/server.toml
# 预期：load mode 摘要 + URI + QR 码

# === macOS 客户端构建 ===
cd /Users/spencer/workspace/qoder/phantom
cargo build --release -p phantom-client --lib
ls -la target/release/libphantom_client.dylib          # 必须存在
nm -gU target/release/libphantom_client.dylib | grep phantom_macos_   # 至少 3 个符号

bash scripts/build-mac.sh
ls -la Phantom.app/Contents/MacOS/Phantom
ls -la Phantom.app/Contents/Frameworks/libphantom_client.dylib
plutil -lint Phantom.app/Contents/Info.plist
otool -L Phantom.app/Contents/MacOS/Phantom | grep phantom_client  # 应指向 Frameworks/libphantom_client.dylib

sudo open Phantom.app
# 预期：菜单栏出现闪电图标；点击 → 粘贴 phantom:// URI → Start

# === 单元测试 ===
cargo test -p phantom-server
cargo test -p phantom-e2e --test cli_system --release
```

---

## 风险与待确认点

1. **qr2term 体积**：依赖 `qair`，增量编译 ~几秒；`phantom-server` binary 体积小幅增加。可接受。
2. **终端 QR 渲染**：`qr2term::print_qr` 默认用 upper-half block + lower-half block + full block，macOS Terminal / iTerm2 / VS Code 终端都能正常显示。不支持 Unicode 的老终端会乱码但不影响 URI 文本行。
3. **load 模式 URI name 字段**：自举默认 `Some("default")`，load 模式也用 `Some("default")` 保持一致。若希望从 TOML 读 `name`，需要改 `ServerConfig`，不在本期范围。
4. **cargo ↔ swiftc 目标三元组映射**：脚本里手工做，universal 构建需要 lipo 合并两套产物，本期仅支持单架构（默认 arm64）。
5. **ad-hoc 签名强度**：仅本地运行用，公证 / Developer ID 在分发阶段再做。
6. **`client/Cargo.toml` 加 `cdylib` 的副作用**：CLI（`client/cli/Cargo.toml`）依赖 phantom-client 的 rlib，保留 `rlib` 不受影响；CLI 自身不会输出 dylib（因为 `[bin]` 类型固定）。需要 `cargo build --release` 全量验证一次。