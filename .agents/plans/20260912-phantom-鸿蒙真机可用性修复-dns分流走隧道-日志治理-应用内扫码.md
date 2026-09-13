# Phantom 鸿蒙真机可用性修复：DNS 分流走隧道 + 日志治理 + 应用内扫码

## 摘要

鸿蒙端 VPN 通道已打通（vpn-tun 有流量、Hello 通过、状态 2），但**应用流量一条都没进服务器**：根因是 TUN 模式的 DNS 完全不工作（明文 UDP 打到 853、且不分流），导致浏览器拿不到 IP，TUN 拿不到域名只能走 direct 被墙。本计划修 DNS（白名单域名经隧道解析、其余本地解析）、修日志（去 ANSI 乱码 + 只显示 200 行 + 路由/DNS 决策可见）、加应用内自绘扫码（CameraKit 预览 + HMS 解码）+ URI 持久化加固，并在 Mac 端同步日志治理。服务端无需改动。

## 关键改动

### A. DNS 分流（本次核心，修「google 打不开 + 日志无 google 记录」）

- `DnsProxy` 从「单一 UDP 上游」改为**双上游 + 按分流决策**：
  - Smart/Auto：域名命中内置白名单（或用户 `rules`）→ **经隧道**解析；其余 → **本地**解析
  - Proxy 模式全部经隧道；Direct 模式全部本地；Reject 直接回 REFUSED
- 隧道解析复用现成的 `udp_relay`（`establish_udp_flow_tcp`，首包随 SYN 走，零额外 RTT；服务端 `handler.rs::udp_relay` 已实现并有 `full_link_udp`/`socks5_udp` e2e 覆盖）。按 resolver 地址懒建立、失败重建、复用同一条流；响应回到同一 `pending(ID)` 表并按应用原始 DNS 事务改写回 TUN。
- 本地解析用进程内直连 UDP socket 到 `client.dns_direct`（默认 `223.5.5.5:53`，备 `119.29.29.29`）；鸿蒙侧进程已 `protectProcessNet()`，走物理网卡不吃隧道。
- **自环守卫**：来自本地 DNS socket 源端口的 53 端口包不再拦截（Android 进程未 protect 时防止无限回灌）。
- DNS 应答的 A 记录继续写入 IP→域名缓存，供 TUN 的 TCP 分流命中白名单。
- 配置：`client.dns` 默认由 `tls://8.8.8.8:853` 改为 `8.8.8.8:53`（隧道内）；新增 `client.dns_direct`（默认 `223.5.5.5:53`）。`tls://` 前缀仍可解析但等价明文，文档标注 DoT 未实现。
- 鸿蒙 `VpnConfig.dnsAddresses` 收敛为 `8.8.8.8`，并为 `dns_direct` 解析器 IP 追加 `isExcludedRoute` /32（API 20+，兼容无 protectProcessNet 的 6.0 设备），`protectProcessNet()` 保留。

### B. 日志治理（鸿蒙 + Mac）

- Rust 侧 `android.rs` / `macos.rs` 的 `tracing_subscriber::fmt()` 统一加 `.with_ansi(false).with_target(false)` → 根治 `[2m[32m` 乱码。
- 路由决策从 `debug` 提升到 `INFO`：TCP 每个新流一行（已有，仅改级别）；UDP 每流首包一行（新增，避免刷屏）。
- 新增 DNS 结果 INFO 日志：`dns www.google.com -> 142.250.x.x via tunnel (whitelist)` / `via local`，让"日志里有没有 google"一眼可查。
- 鸿蒙 UI：桥接层按行环形缓冲，只渲染**最后 200 行**，新增「清空」「暂停」按钮，日志文件仍限 32 KiB；Mac 面板已限 200 行，只做 ANSI 修复。

### C. 鸿蒙应用内扫码 + 持久化加固

- 新增 `pages/ScanPage.ets`（注册进 `main_pages.json`，`router.pushUrl` 进入）：XComponent 相机预览（CameraKit `previewOutput`）+ `imageReceiver` 抓帧 + 自绘取景框/激光线/手电筒，帧按 3–5 fps 节流送解码。
- 解码首选：`imageReceiver` JPEG → ImageSource/PixelMap（下采样 ≤1280px）→ `readPixelsToBuffer` → 转 NV21 → `detectBarcode.decodeImage(ByteImage)`（HMS Scan Kit，API 12+）。
- 解码降级阶梯（按真机实测取用，接口不变）：① 若 `decodeImage` 在真机不可用 → JPEG 落 `cacheDir` 后走 `detectBarcode.decode({uri:'file://…'})`；② 若 HMS 解码整体不可用 → 切 Rust 侧 `rqrr`+`zune-jpeg` 在本机 `libphantom_harmony.so` 里解码（NAPI 传帧）。
- 权限：`module.json5` 增 `ohos.permission.CAMERA`（含 reason/usedScene），运行时 `requestPermissionsFromUser`；被拒时提示并回退手输/相册。
- 相册兜底：`photoAccessHelper.PhotoViewPicker` → 同一条 decode 路径。
- 结果处理：仅接受 `phantom://` 前缀；写入 `phantom_ui` preferences 后返回 VPN 页；`Index` 在 `onPageShow` 重新读取；**不自动连接**。
- 持久化加固：URI 输入变化即防抖保存（不再只在点 Start 时保存），启动仍从 preferences 读取（已验证当前 `phantom_ui` 文件确实写入了 URI）。

### D. 接口与契约变更

- `ClientSettings`：`dns` 默认改 `8.8.8.8:53`，新增 `dns_direct` 默认 `223.5.5.5:53`。
- `DnsProxy` 新形态：双传输 + `handle_query(datagram, ctx, route)` / `handle_response(payload)`；`TunProxy` 负责懒建 DNS 隧道流并驱动响应循环。
- 日志级别契约：INFO 含每流首包 `route` 决策与 DNS 结果；细节仍在 debug。
- 鸿蒙新增页面路由 `pages/ScanPage`；preferences 键名不变。服务端零改动。

## 测试与验收

- 单测：DNS 分流决策（`google.com`→tunnel、`v.youku.com`→local、Proxy/Direct/Reject 三模式）；`parse_dns_addr`（`tls://`、裸 IP、ip:port）；本地 DNS 自环守卫；既有 `full_link_udp`/`socks5_udp` 保持通过；`cargo test -p phantom-client --lib`、`-p phantom-core --lib`。
- 真机（HOP-AL00，已连电脑）：重建 `.so` + 签名 HAP → 安装 → Start Tunnel → 授权弹窗与状态栏 VPN 图标（顺带确认图标归属，排除你看到的"OpenVPN 图标"是别的应用）。
- 真机分流：App 日志出现 `route www.google.com:443 -> PROXY (whitelist)` 与 `route v.youku.com:443 -> DIRECT (final)`；手机浏览器打开 google.com 正常；VPS 侧 `tail -f /var/log/phantom.log` 出现 `SYN → www.google.com:443` + `Relay done`，访问 youku 时 VPS 无对应连接；`vpn-tun` 计数持续增长但 VPS 只看到白名单目标。
- 真机 DNS：hilog 不再出现 `dnsStatus:3` / 8 秒超时；App 日志能看到 `dns … via tunnel/local`。
- 真机扫码：用终端 `phantom server` 打印的二维码扫码 → 填入并保存 → 杀掉 App 重开仍在；相册识别路径同样可用。
- 真机日志：连续运行 10 分钟界面不卡顿、无乱码、只保留 200 行。
- Mac：`cargo xtask build mac` 后日志面板无 ANSI 乱码、仍保留 200 行上限与颜色分级。

## 假设与默认

- DNS 分流转发按你的选择：白名单域名经隧道由 `8.8.8.8` 解析，其余走本地 `223.5.5.5`；隧道内 DNS 用 UDP relay 到 `8.8.8.8:53`，不实现 DoT。
- 扫码按你的选择：应用内自绘预览 + HMS `detectBarcode` 识别，失败时按上述阶梯降级；扫到即填入并持久化，不自动连接。
- 日志按你的选择：200 行上限 + 清空/暂停 + 路由/DNS 决策升到 INFO，鸿蒙与 Mac 同步。
- 本轮不做 SNI 嗅探、不做 DoH/Secure DNS 拦截；若某浏览器开启 Secure DNS 绕过我们的解析，按文档关闭该项处理（代码不改）。
- Android 真机不在本轮验证范围，但共享核心改动会一并生效；`client.dns` 语义变更会同步进 `deploy/README.md` 与鸿蒙 README。
