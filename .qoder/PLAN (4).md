# 鸿蒙扫码页修复 + 连接历史 + 二维码分享

## 摘要
修 5 件事：扫码页跟随重力自动转正、扫码页沉浸式全屏、连接串历史（下拉 + ✓）、分享/扫码改图标、分享改为出示二维码。全部是 ArkTS/UI 与本地 preferences 改动，不涉及 Rust、服务端与部署；验证顺序为**先模拟器（Pura X Max）→ 再真机 HOP-AL00**。

## 关键改动

**A. 扫码页方向与全屏（`pages/ScanPage.ets`）**
- 方向：页面存活期间 `display.on('change')`，在会话启动后与每次屏幕旋转时执行 `previewOutput.setPreviewRotation(previewOutput.getPreviewRotation(display.getDefaultDisplaySync().rotation))`，让 XComponent 硬件预览在任何姿态都正立；`aboutToDisappear` 里 `display.off('change')`。
- 降级预览（receiver-only 的 PixelMap 路径）同样按显示角度 `pixelMap.rotate()`；解码路径仍把 NV21 原样交给 Scan Kit（二维码识别本身抗旋转），**仅当真机横屏实测解不出码时**再在解码前做 90/270 旋转。
- 全屏：`onPageShow` 用 `window.getLastWindow` 打开 `setWindowLayoutFullScreen(true)` + `setWindowSystemBarEnable([])`，`onPageHide` 恢复 `(false)` + `(['status','navigation'])`；根 Stack 加 `.expandSafeArea([SafeAreaType.SYSTEM], [SafeAreaEdge.TOP, SafeAreaEdge.BOTTOM])`，取景框/提示/按钮按避让区留边，消除上下黑边。

**B. 连接串历史（新增 `common/ConnectionHistory.ets` + `pages/Index.ets`）**
- 存储：`phantom_ui` 新增键 `serverHistory`，纯文本按行 `uri \t lastUsedMs \t verifiedMs`（新→旧、按 uri 去重、上限 20），与现有文件桥风格一致，避免 ArkTS 的 JSON 类型摩擦。
- 写入时机：点 Start Tunnel 或扫码导入时写 `lastUsedMs`；当状态变为 Running 且 URI 匹配时写 `verifiedMs`（✓ 的语义 = 该串至少成功连接过一次）。
- 交互：输入框右侧新增 ▾ 图标 → `bindMenu` 下拉：顶部「历史连接」，每行显示节点名 + host:port/proto/cipher + 相对时间，已验证行前显示绿色 ✓，行尾 ✕ 删除，底部「清空历史」；空列表显示「暂无历史」。**点条目只填入输入框并保存，不自动连接**。
- 菜单内容在展开时构建，列表 `@State` 随现有 500ms 轮询刷新；若实测下拉内容不刷新，改用已验证可响应的 `bindSheet` + 子组件面板（与 `ServerInfoPanel` 同模式）。

**C. 图标与分享（`pages/Index.ets` + 资源）**
- 新增本地 SVG 资源 `ic_share.svg` / `ic_scan.svg` / `ic_history.svg` / `ic_info.svg`，替换「分享」「扫码」「ⓘ」文本按钮，统一为 40×40 圆形图标按钮（`Image().fillColor()` 适配深浅色，保留 `accessibilityText` 便于自动化点击）。
- 分享改为打开 `bindSheet` 子组件 `ShareQrPanel`：`QRCode(当前串)` + 节点名/地址 + 说明「让另一台设备扫码导入」，操作按钮为**复制连接串 / 系统分享 / 关闭**（系统分享沿用现有 `systemShare`）。

## 接口与契约变更
- preferences 新增 `serverHistory`（行式文本，字段如上），`serverUri`/`proxyMode`/`showDirectLogs` 不变。
- 新增 ArkTS 模块与组件：`common/ConnectionHistory.ets`、`ShareQrPanel`、4 个 SVG 媒体资源。
- 副作用：扫码页会切换窗口沉浸式状态（进入开启、退出恢复），Index 布局不受影响。

## 测试与验收
1. 构建：`./scripts/build-harmony.sh` → `DEVECO_SDK_HOME=/Applications/DevEco-Studio.app/Contents/sdk hvigorw assembleHap -p module=entry@default -p product=default -p buildMode=debug --no-daemon`。
2. 模拟器优先：`Emulator -start "Pura X Max"` → `hdc install -r <signed.hap>`；若因签名/UDID 安装失败，直接切真机 HOP-AL00（按你的选择）。
3. 方向：用 `Emulator -instance "Pura X Max" -rotation left|right` 切换姿态并截图，断言预览画面与 UI 同向正立（模拟器 `Camera.properties` 已声明后置相机，可走预览路径）；真机再补 0/90/180/270 实测。
4. 全屏：截图断言上下无黑边、状态栏/导航栏隐藏，点「取消」返回后恢复原样。
5. 历史：依次导入/连接两个串 → 下拉按新旧排序、无重复、仅成功连接过的带 ✓；杀进程重开仍在；单条删除与「清空」生效；点条目只填入。
6. 分享：截图后用 `cv2.QRCodeDetector` 解码二维码区域，断言内容等于当前连接串；再扫码导入到另一台设备验证。
7. 回归：主页状态/速率/日志正常，Start/Stop Tunnel 正常（本轮无 Rust 改动，`cargo test` 不受影响）。

## 假设与默认
- 已确认选择：跟随重力转正、仅扫码页全屏、历史用下拉菜单、点历史只填入、分享面板含二维码+复制+系统分享。
- ✓ 仅表示"本机用该串成功连接过"，不做服务端校验；历史上限 20、按原始串去重、不做跨设备同步。
- 图标一律用项目内 SVG，不依赖 `sys.symbol.*`（SDK 未提供符号名清单，写错只能在运行时暴露）。
- 二维码识别默认抗旋转；真机横屏实测失败才在解码前加旋转。
- 模拟器装不上（签名/UDID）时不阻塞，改用真机验证。
