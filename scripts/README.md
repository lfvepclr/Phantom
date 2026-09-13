# 辅助脚本（`scripts/`）

一次性运维/构建脚本的集合。**优先用 `cargo xtask`**（见 [xtask/README.md](../xtask/README.md)），
这里只保留 xtask 不方便覆盖、或需要在 CI/真机上单独执行的脚本。

## 构建与签名

| 脚本 | 用途 |
|------|------|
| `build-mac.sh` | macOS 客户端（SPM + 自写 bundler），产出 `client/mac/.build/Phantom.app` 与 `.dmg` |
| `build-android.sh` | Android AAR/APK（gradle + cargo-ndk） |
| `build-harmony.sh` | HarmonyOS HAP（hvigor + NAPI 交叉） |
| `sign-harmony-hap.sh` | 鸿蒙 HAP 签名 |

## 部署与验证

| 脚本 | 用途 |
|------|------|
| `deploy-server.sh` | `deploy-server.sh <bundle.tar.gz> <host> [port] [proto] [public-host]` —— 上传发布包、安装、回显 URI |
| `verify-server.sh` | `verify-server.sh <pkg-dir> <arch> <engine>` —— 容器内离线验证发布包 |
| `speedtest.sh` | 吞吐 + 解锁断言 |

## 诊断与工具

| 脚本 | 用途 |
|------|------|
| `mac-sysproxy.sh` | macOS `networksetup` 系统代理手动设置/还原（排查客户端自动代理失效时用） |
| `tun-trace-report.py` | 解析 `client/src/tun_trace.rs` 产出的 TUN 轨迹，生成报告 |
| `harmony-bench.sh` | 鸿蒙真机批量测速 |

## 约定

- 脚本应幂等、失败即非零退出，不 silently continue。
- 新脚本如果属于"常规构建/打包/部署"，应该进 `xtask/src/`，不在这里重复实现。
