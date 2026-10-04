# 鸿蒙真机签名有效期与启动管控排查（6.x / 7.x）

> 本文记录 2026-10-04 在 **HOP-AL00 / HarmonyOS 7.0.0.109**（真实设备标识已脱敏）
> 上定位到的问题：应用不是代码崩溃，而是 **DevEco 自动签名材料过期**，被
> AppGallery 应用管控在启动前拦截。

## 1. 症状

点桌面图标无反应，或 `hdc` 启动直接失败：

```text
$ hdc shell aa start -a EntryAbility -b co.phantom.harmony
error: failed to start ability.
Error Code:10106105  Error Message:The target application is under control.
Error cause: The application is suspected of malicious behavior and is
              restricted from launching by the appStore
  Try the following:
  > It is recommended to uninstall the application
```

手机屏幕上同时弹出 AppGallery 的处置弹窗：

```text
App unavailable
This app is no longer available and can't be used. Uninstall?
```

这不是 ArkTS 崩溃、不是 TUN/NAPI 初始化失败，日志里也看不到 `phantom` 进程
起来之后的异常——进程根本没有被创建。

## 2. 真机证据（2026-10-04）

| 检查项 | 实测值 | 结论 |
|---|---|---|
| 设备 | HOP-AL00，HarmonyOS 7.0.0.109，API 26 | 7.x 真机 |
| `aa start` | `10106105 The target application is under control` | 启动前被系统/应用市场拦截 |
| `hilog` | `control rule caller:com.huawei.hmsapp.appgallery` + `DisposeAlertAbility` | AppGallery 管控弹窗，不是应用崩溃 |
| 已安装包 | `bundle=co.phantom.harmony`，`appProvisionType=debug`，`appIdentifier=<本机证书的 app id>` | 用的是 DevEco 自动签名材料 |
| 签名 Profile | `~/.ohos/config/default_harmony_*.p7b`，`type=debug` | DevEco 自动签名（约 14 天） |
| Profile 有效期 | **2026-09-12 16:26 → 2026-09-26 16:26** | 今天已过期约 8 天 |
| 已签名 HAP | `hap-sign-tool.jar verify-app` 报告叶子证书有效期同上 | 证书本体也已过期 |
| Profile `device-ids` | 与本机 `bm get -u` 完全一致（具体值不入库） | 设备绑定没问题 |
| 安装时间 | `firstInstallTime/updateTime = 2026-09-12` | 安装时材料有效，之后才过期 |

结论：**UDID 匹配、代码可编译、SDK 兼容；唯一的失效点是签名 Profile 过期。**

## 3. 为什么"升级后"才出现

- 9 月 12 日安装时，自动签名材料是有效的，应用能正常用。
- **9 月 26 日 16:26 调试 Profile 到期**。
- 手机升级到 HarmonyOS 7.0.0.109 后，7.x 的 AppGallery 应用管控会主动校验
  应用的签名/来源状态；对"已失效但还装在机器上"的应用，直接标记为
  unavailable，并由 AppGallery 弹窗引导卸载。
- HarmonyOS 6.x 对同一场景的拦截策略相对宽松，所以表现为"6.x 能跑、7.x
  升级后突然打不开"。

`Uninstall` 按钮只是 AppGallery 给出的处置建议。卸载能清掉管控状态，但如果不
换一份有效的签名材料，重装仍然会被拦。

## 4. 6.x / 7.x 兼容性结论

当前 `client/harmony/build-profile.json5`：

| 字段 | 值 | 含义 |
|---|---|---|
| `compatibleSdkVersion` | `6.0.0(20)` | 最低兼容 HarmonyOS 6.0（API 20） |
| `targetSdkVersion` | `6.1.1(24)` | 面向 6.1.1 行为，7.0 设备进入兼容模式 |
| 实际 `compileSdkVersion` | `26.0.0.105`（由 DevEco 26 编译） | 用 7.0 SDK 编译，产物可装进 7.0 |

7.0 真机能完成解析、安装并走到 BMS 启动门禁，说明**二进制/API 兼容没有问题**。
本次要修的是签名有效期，不是改 `compatibleSdkVersion`，也不要为了 7.x 盲目
抬高 `targetSdkVersion`。

## 5. 签名能签多久（最长方案）

| 路径 | 证书本体有效期 | Profile 有效期 | 用途 |
|---|---|---|---|
| DevEco「自动签名」 | 约 14 天 | 约 14 天 | 临时真机调试，**最容易过期** |
| AGC「调试证书」+ 调试 Profile | **1 年（需实名；未实名 14 天）** | 随证书（绑定设备 UDID） | 长期真机调试 |
| AGC「发布证书」+ 发布 Profile | **3 年（当前官方上限）** | 以 AGC 申请页显示为准，到期前续期 | AppGallery 上架/分发；**不授权本地 hdc 安装** |

结论：**自己用、走 `hdc install` 的上限是 1 年调试证书（实名后）**。
发布证书虽然 3 年，但发布 Profile 只授权应用商店分发，本地安装会被系统拒绝；
只有把应用上架/内测到 AppGallery 才能用上 3 年。另注意：

1. 没有哪张 HarmonyOS 证书是永久有效的，发布 Profile 仍需按 AGC 提示续期。
2. HarmonyOS 7.x 上，非应用市场来源的应用会触发 AppGallery 管控；调试证书 +
   调试 Profile 是官方允许的本地调试通道，本次已验证可用。

### 绑定 1 年调试证书（AGC 手动申请，推荐）

AGC 调试证书的有效期由账号实名状态决定：**已实名 1 年 / 未实名 14 天**。
本机已经生成一份独立密钥与证书请求：

| 文件 | 路径 | 说明 |
|---|---|---|
| 私钥库 | `client/harmony/signing/phantom-debug.p12` | gitignored；keyAlias `phantom_debug`，口令在本机 `signing/passwords.env`（不入库） |
| CSR | `client/harmony/signing/phantom-debug.csr` | 上传到 AGC 申请调试证书 |
| 设备 UDID | 用 `hdc shell bm get -u` 取本机值（本文不落具体串） | 调试 Profile 必须绑定它 |

操作步骤：

1. 在 AGC 完成**实名认证**（未实名只能申请 14 天证书）。
2. 我的项目 → 应用（bundle `co.phantom.harmony`）→ 证书、Profile 管理 →
   证书 → 新增证书 → **调试证书** → 上传 `phantom-debug.csr` → 下载 `.cer`，
   另存为 `client/harmony/signing/phantom-debug.cer`。
3. Profile 管理 → 新增 Profile → **调试** → 选择应用 `co.phantom.harmony`
   + 刚创建的证书 → 添加设备 UDID → 下载 `.p7b`，另存为
   `client/harmony/signing/phantom-debug.p7b`。
4. 用 `scripts/check-harmony-signing.sh --profile client/harmony/signing/phantom-debug.p7b`
   复核，应显示 `status: OK` 且剩余约 365 天；之后重新打包安装即可。

下载完成后，本地只需要一条命令（自动签名 + 验签 + 可选安装）：

```bash
scripts/sign-harmony-personal.sh --install
```

### 3 年发布证书（仅当你要上架 AppGallery）

`client/harmony/signing/phantom-release.csr` 已就绪。发布材料即使签到 3 年，
Release 类型的 Profile 也不包含设备白名单、只授权应用商店分发，`hdc install`
会被系统拒绝。因此"自己用"不建议走这条路；若未来要上架，再按
"申请发布证书 → 申请发布 Profile（类型选发布）"下载 `phantom-release.cer/p7b`，
用 `scripts/sign-harmony-personal.sh --release` 签名。

## 6. 立即恢复（最短路径，约 5 分钟）

1. 手机解锁并连接 USB，确认设备在线：

   ```bash
   hdc list targets
   ```

2. DevEco Studio 打开 `client/harmony`，登录华为开发者账号，勾选：

   **File → Project Structure → Signing Configs → Automatically generate signature**

   点 Apply/OK。IDE 会重新申请一份调试证书 + Profile（新的约 14 天有效期），
   并把设备 UDID 写进去。该改动只落本机 `build-profile.json5`（仓库里是
   `skip-worktree`，不会提交）。

3. 确认新材料的有效期和 UDID：

   ```bash
   scripts/check-harmony-signing.sh
   ```

4. 重新打包带签名的 HAP（或直接在 DevEco 点 Run）：

   ```bash
   cd client/harmony
   ./hvigorw assembleHap -p module=entry@default -p product=default \
     -p buildMode=debug --no-daemon
   ```

5. 覆盖安装并启动：

   ```bash
   hdc install -r entry/build/default/outputs/default/entry-default-signed.hap
   hdc shell aa start -a EntryAbility -b co.phantom.harmony
   ```

6. 如果仍报 10106105：先卸载清掉 AppGallery 的旧管控状态，再全新安装。
   **会删除应用内配置（含已保存的 URI），操作前先备份。**

   ```bash
   hdc uninstall co.phantom.harmony
   hdc install entry/build/default/outputs/default/entry-default-signed.hap
   ```

7. 个别 7.x 机器还要在手机上放行一次（路径随版本略有差异）：

   - AppGallery → 我的 → 应用管控 → 找到 Phantom → 解除管控；
   - 或 设置 → 系统和更新 → 纯净模式 → 退出纯净模式。

## 7. 长期方案（不再每两周翻车）

1. 在 AGC 申请 **发布证书（3 年）+ 发布 Profile**，把它配置成 release
   signingConfig；日常开发仍可保留自动签名。
2. 每次出包前跑 `scripts/check-harmony-signing.sh`；CI 里用 `--json` 把
   `remaining_days` 写进构建日志，过期直接失败。
3. 通过 AppGallery 上架 / 内测渠道分发，绕开"非市场来源"管控。
4. 发版时递增 `AppScope/app.json5` 的 `versionCode`；覆盖安装用
   `hdc install -r`，全新安装用 `hdc install`。

## 8. 复发预防（仓库已接入）

- `scripts/check-harmony-signing.sh`：解析 p7b 内嵌 JSON，打印证书类型、
  有效期、剩余天数、当前设备 UDID 是否匹配；过期返回 1。
- `scripts/build-harmony.sh`：每次构建 Rust `.so` 后自动跑一次预检，过期时
  打印醒目 WARNING（不阻断纯 `.so` 构建）。
- `scripts/sign-harmony-hap.sh`：明确标注仅用于 OpenHarmony / 模拟器自签，
  真机请走 DevEco 自动签名或 AGC 材料。

## 9. APMS 跟这件事有关系吗

没有直接关系。CSDN 那篇《鸿蒙 App 上线后有问题？APMS 给应用装了台"行车记录仪"》
介绍的 **APMS（Application Performance Management Service）** 是应用性能/稳定性
监控：启动耗时、崩溃、ANR、卡顿、网络性能等。

- 它不参与安装、签名、Profile 或应用市场管控，不会导致 `10106105`；
- 接入它也不能修好本次启动失败；
- 等应用恢复启动并上架后，可以把它当作"上线后的行车记录仪"接入，用来观测
  启动成功率与崩溃率。

## 10. 验证清单

```bash
scripts/check-harmony-signing.sh                 # status: OK
hdc list targets                                 # 设备在线
hdc shell bm dump -n co.phantom.harmony | grep -E 'versionCode|appProvisionType'
hdc shell aa start -a EntryAbility -b co.phantom.harmony
hdc shell pidof co.phantom.harmony               # 有进程
```

离线核对签名（证书有效期必须覆盖今天）：

```bash
java -jar /Applications/DevEco-Studio.app/Contents/sdk/default/openharmony/toolchains/lib/hap-sign-tool.jar \
  verify-app -inFile client/harmony/entry/build/default/outputs/default/entry-default-signed.hap \
  -outCertChain /tmp/verified.cer -outProfile /tmp/verified.p7b
```

手机上确认：能进首页、能启动 VPN、无 AppGallery "App unavailable" 弹窗。
