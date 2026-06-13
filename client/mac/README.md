# Phantom macOS Client

Native macOS menu-bar client.  The tunnel engine runs entirely in Rust; SwiftUI is a thin control shell.

## Architecture

```
SwiftUI (Menu Bar)  <->  C FFI  <->  Rust cdylib (phantom-client)
                                      ├─ TUN device (utun)
                                      ├─ Packet parser / NAT
                                      ├─ SOCKS5 proxy
                                      └─ QUIC/TCP tunnel
```

## Build

使用根目录的自动化构建脚本（推荐）：

```bash
cd <repo-root>
scripts/build-mac.sh              # 默认 release (Apple Silicon)
scripts/build-mac.sh --debug      # debug 构建
```

脚本会依次执行：

1. `cargo build -p phantom-client --lib` — 产出 Rust cdylib
2. 复制 dylib 到 `client/mac/PhantomLibs/`
3. `swift build -c release` — SPM 编译 Swift 代码 + 链接 dylib
4. `swift run PhantomMacBuilder` — 把产物打成 `client/mac/Phantom.app`

### 前置条件

- Rust toolchain (>= 1.85, edition 2024)
- macOS 13.0+（MenuBarExtra 要求）
- Xcode Command Line Tools（`xcode-select --install`），含 `swift` + `codesign` + `plutil`

不需要 Xcode 工程文件：整个构建流程走 Swift Package Manager。

## Launch

```bash
# TUN 设备创建需要 root：
sudo open client/mac/Phantom.app
# 不加 sudo 也可启动，但 TUN 部分会失败，只剩 SOCKS5 代理可用
```

菜单栏出现闪电图标；点击 → 输入 `phantom://` URI → 选 Global / Auto / Direct → Start。

## 项目结构

```
client/mac/
├── Package.swift                  # SPM manifest
├── Sources/
│   ├── PhantomMac/                # 主程序（4 个 swift 文件）
│   │   ├── PhantomMacApp.swift
│   │   ├── PhantomTunnel.swift
│   │   ├── Bridge.swift
│   │   └── SystemProxy.swift
│   └── PhantomMacBuilder/         # 打包工具（仿 mytime DMGBuilderExec）
│       └── main.swift
├── PhantomLibs/                   # Rust dylib 暂存（gitignored，build 脚本填充）
├── Info.plist                     # 拷贝进 .app/Contents/
├── .build/                        # SPM 产物（gitignored）
└── Phantom.app                    # 最终产物（gitignored）
```

## Notes

- 改完 Rust 代码后必须重跑 `scripts/build-mac.sh`（dylib 会重新生成 + 复制）。
- `LSUIElement = true` 已在 `Info.plist` 中声明，菜单栏 app 不在 Dock 显示。
- TUN 设备创建需要 root 或 `com.apple.vm.networking` entitlement。
- 构建流程参考 `qoder/mytime` 项目的 `Package.swift` + `DMGBuilderExec` 模式。