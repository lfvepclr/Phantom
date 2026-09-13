# Phantom HelloWorld 连接验证实现计划

## Task 1: 协议设计 — 新增 HELLO / HELLO_ACK 帧

**目标**：在现有 Frame 体系中增加一次加密的"握手后探测"，不破坏 v2 兼容。

**变更文件**：
- `core/src/protocol/frame.rs`

**具体变更**：
1. 在 `FrameFlags` 中新增两个 flag 位：
   - `const HELLO = 0x40` （与现有 PONG 冲突，需要重新分配；建议把 `PING/PONG` 改为 `0x20/0x40` 不变，新增 `HELLO = 0x04` 会冲突；最终建议新增 `HELLO = 0x40` 则需将 `PONG` 移到 `0x80` 并移除/重排 UDP。更安全的做法：使用保留位或扩展 flag 字节。）
   - 推荐方案：**不新增 flag 位**，复用 `SYN` + 特殊 `stream_id = 0` + payload 以 `"PHANTOM_HELLO"` 开头。这样现有 v2 服务端会把 HELLO 当作普通连接请求处理，会走错分支；因此更推荐扩展 flags。
   - 最终推荐方案：新增 `FrameFlags::HELLO = 0x04` 和 `FrameFlags::HELLO_ACK = 0x08`，同时把 `RST` 和 `ACK` 重排。**需要评估是否破坏已有 v2 客户端**。
   - 最安全的向前兼容方案：保留现有 flags 不变，使用 **data frame + 固定 payload magic** 在已建立的加密会话里做心跳式探测。服务端识别 magic 后返回 ACK + 探测结果，不走 SOCKS5 relay 流程。
2. 在 `Frame` 上新增构造方法：
   - `Frame::hello(stream_id, nonce_bytes)`
   - `Frame::hello_ack(stream_id, result_payload)`
3. 在 `core/src/constants.rs` 中新增常量：
   - `HELLO_PAYLOAD_MAGIC: &[u8] = b"PH/HELLO"`
   - `HELLO_TIMEOUT_SECS: u64 = 10`
   - 推荐外网探测目标常量（见 Task 2）。

**验收标准**：
- `cargo test -p phantom-core -- frame` 通过。
- 新旧 flag 位不与现有值冲突，或采用 magic-payload 方案完全不改动 flags。

---

## Task 2: 选择服务端外网探测目标

**目标**：找到一个几乎在全球任何地域都能访问、内容稳定、且能证明"服务端可上外网"的目标。

**候选评估**：

| 目标 | 优点 | 缺点 |
|------|------|------|
| `http://captive.apple.com/hotspot-detect.html` | iOS/macOS 原生使用，全球 CDN，返回固定 `<HTML><HEAD><TITLE>Success</TITLE></HEAD><BODY>Success</BODY></HTML>` | 在部分企业网络可能被屏蔽 |
| `http://detectportal.firefox.com/success.txt` | 内容简单固定（`success\n`），Firefox 原生使用 | 在中国大陆等地区可能不稳定 |
| `http://www.msftconnecttest.com/connecttest.txt` | Windows 原生使用，全球部署 | 内容 `Microsoft Connect Test` 较长，且企业环境可能限制 |
| `http://www.google.com/generate_204` | 内容为空 204，速度快 | 在中国大陆不可访问 |
| 服务端内置 HTTP `/hello` 端点 | 不依赖外网，可验证客户端→服务端 | 不能证明服务端→外网 |

**推荐方案**：
- **主目标**：`http://captive.apple.com/hotspot-detect.html`（全球可达，返回固定文本）。
- **Fallback 目标**：`http://detectportal.firefox.com/success.txt`。
- 同时支持服务端配置 `verification_url` 覆盖默认值，便于私有部署。
- 探测方式：HTTP GET，期望 200 OK + 返回体包含固定关键字（如 `Success` 或 `success`），这样即使被中间人篡改也能通过关键字匹配容错。

**验收标准**：
- 服务端在主流网络环境下 10 秒内完成探测。
- 支持配置覆盖默认 URL。

---

## Task 3: 服务端实现 HELLO 处理

**目标**：服务端收到 HELLO 后访问外网探测目标，把结果返回给客户端。

**变更文件**：
- `server/src/handler.rs`
- `server/src/lib.rs` 或新增 `server/src/hello.rs`
- `server/Cargo.toml`（可能需要 `reqwest` 或直接用 `tokio` HTTP 客户端）

**具体变更**：
1. 在 `handle_frame_stream` 中，读取第一个 frame 后判断：
   - 若 `flags.contains(HELLO)`：调用 `handle_hello(frame, frame_writer).await`。
   - 否则走原有 SYN relay 流程。
2. 新增 `async fn handle_hello(...)`：
   - 解析 payload 中的 nonce / 客户端版本。
   - 使用 `reqwest::Client`（或 `tokio::net::TcpStream` + 手写 HTTP/1.0）GET 探测目标。
   - 构造 `HELLO_ACK` frame，payload 包含 JSON/protobuf：
     - `nonce`（echo 回去）
     - `status: "ok" | "error"`
     - `message: String`（如外网目标响应摘要或错误原因）
     - `server_time: u64`
   - 发送 HELLO_ACK 后关闭该子流（FIN）。
3. 若探测失败（超时、非 200、内容不匹配），返回 `HELLO_ACK` 且 `status = "error"`，客户端据此标记连接失败。
4. 在服务端配置 `ServerConfig` 中新增可选字段 `verification_url: Option<String>`。

**验收标准**：
- 服务端启动后，CLI/macOS 客户端发起 HELLO，服务端返回 OK 或明确的 error message。
- 单元测试：模拟 HELLO frame，验证服务端返回 HELLO_ACK。

---

## Task 4: Rust 客户端核心 — 发起 HELLO 探测

**目标**：在客户端 SOCKS5 监听启动后，通过已建立的加密会话向服务端发起一次 HELLO 探测。

**变更文件**：
- `client/src/tunnel.rs`
- `client/src/socks5.rs`
- 新增 `client/src/hello.rs`

**具体变更**：
1. 新增 `client/src/hello.rs`：
   - `pub async fn verify_server_connection(config: &ClientConfig) -> Result<HelloResult>`
   - 选择第一个可用 server，建立 TCP/QUIC 连接，完成 Noise 握手。
   - 发送 HELLO frame，等待 HELLO_ACK（10 秒超时）。
   - 解析 ACK payload，返回 `HelloResult { success: bool, message: String, latency_ms: u64 }`。
2. 在 `client/src/tunnel.rs` 的 `PhantomClient::run()` 中：
   - SOCKS5 `TcpListener::bind` 成功后，先调用 `verify_server_connection(&self.config).await`。
   - 若失败：
     - 打印/记录错误，设置 tunnel 状态为 error，**不进入 accept loop**（CLI 直接退出；macOS 通过 FFI 返回错误）。
   - 若成功：记录日志 `"Hello verification passed: <message>"`，再进入 accept loop。
3. 在 `client/src/socks5.rs` 中：每条真实 SOCKS5 连接仍走原有 relay 流程，HELLO 只由 `tunnel.rs` 主动发起一次。

**验收标准**：
- 正确 URI + 正常服务端 → 验证通过，客户端开始接受 SOCKS5 连接。
- 错误 URI / 服务端不可达 / 服务端无外网 → 验证失败，客户端给出明确错误。

---

## Task 5: CLI 客户端集成 HELLO 结果

**目标**：CLI 启动后能看到真实的连接验证结果。

**变更文件**：
- `client/cli/src/main.rs`

**具体变更**：
1. 在 `client.run().await?` 前后捕获验证结果。
   - 由于 `run()` 内部先验证再监听，验证失败会返回 `Err`，直接打印错误并退出。
2. 成功时打印：
   ```
   [INFO] SOCKS5 proxy listening on 127.0.0.1:1080
   [INFO] Hello verification passed: captive.apple.com reachable (latency 42ms)
   ```
3. 失败时打印：
   ```
   [ERROR] Hello verification failed: server unreachable / server cannot reach internet: ...
   ```

**验收标准**：
- `phantom client --server <正确URI>` 输出成功信息。
- `phantom client --server <错误URI>` 输出失败信息并退出非零。

---

## Task 6: macOS 客户端 FFI 与状态机集成

**目标**：macOS UI 只有在 HELLO 验证通过后才显示"已连接"，否则显示错误。

**变更文件**：
- `client/src/platform/macos.rs`
- `client/mac/Sources/PhantomMac/Bridge.swift`
- `client/mac/Sources/PhantomMac/PhantomTunnel.swift`
- `client/mac/Sources/PhantomMac/PhantomMacApp.swift`

**具体变更**：
1. Rust FFI 层（`client/src/platform/macos.rs`）：
   - 在 `start_with_config` 中，创建 Runtime 后、进入 SOCKS5 accept loop **之前**，调用 `verify_server_connection`。
   - 验证失败时设置 `TUNNEL_STATUS = 3 (error)` 和 `LAST_ERROR`，并返回非 0。
   - 由于 macOS 版把 SOCKS5 accept loop 放在 background task 中，需要把同步验证放在 background task 最开始：先验证，再 `set_status(2)`（running），再启动 SOCKS5 listener。
   - 新增 FFI 函数：
     - `phantom_macos_verify_status() -> i32`（可选，用于轮询）
     - 或复用已有的 `phantom_macos_get_status()` + `phantom_macos_get_last_error()`。
2. Swift Bridge（`Bridge.swift`）：
   - 已有 `phantom_macos_get_status()` 和 `phantom_macos_get_last_error()`，无需新增 FFI 声明即可满足。
3. `PhantomTunnel.swift`：
   - 在 `start()` 启动 Rust runtime 后，通过 status timer 轮询真实状态。
   - 状态流转：
     - `idle` → 点击 Start → `starting` → Rust 正在验证 → `running`（验证成功）或 `error`（验证失败）。
   - 验证成功后再设置 `isRunning = true`、启用系统代理。
   - 验证失败时显示 `LAST_ERROR` 内容。
4. `PhantomMacApp.swift`：
   - 确保状态 pill 在 `starting` 时显示"Connecting..."，在 `running` 时显示"Connected"，在 `error` 时显示错误摘要。

**验收标准**：
- 输入随机不可达 URI → Start 后状态变成 error，显示类似 `Server unreachable`。
- 输入正确 URI → Start 后经过短暂验证变成 Connected，日志区显示 `Hello verification passed`。

---

## Task 7: 配置与默认值

**目标**：让探测目标可配置，且默认开箱即用。

**变更文件**：
- `core/src/config.rs`（客户端配置）
- `core/src/config.rs` 或 `server/src/config.rs`（服务端配置）
- `config/server.toml` 示例

**具体变更**：
1. 客户端 `ClientConfig` 新增：
   - `hello_timeout_secs: u64 = 10`
   - `hello_targets: Vec<String>`（可选，覆盖默认探测目标）
2. 服务端 `ServerConfig` 新增：
   - `verification_url: Option<String>`（服务端用于主动探测外网的 URL）
3. 默认值放在 `core/src/constants.rs`：
   - `DEFAULT_HELLO_TARGETS: &[&str] = &["http://captive.apple.com/hotspot-detect.html", "http://detectportal.firefox.com/success.txt"]`
4. 更新 `config/server.toml` 示例，添加 `# verification_url = "..."` 注释。

**验收标准**：
- 不配置时使用默认目标。
- 配置后可覆盖。

---

## Task 8: 测试计划

**变更文件**：
- `tests/tests/hello_protocol.rs`（新增集成测试）
- `core/src/protocol/frame.rs` 已有单元测试基础上补充 HELLO 帧测试

**具体变更**：
1. 单元测试：
   - `Frame::hello(...).encode()` / `decode()` 往返正确。
   - flag 位不冲突。
2. 集成测试：
   - 启动一个本地 server（使用 test fixture）。
   - 调用 `phantom_client::hello::verify_server_connection`。
   - 验证返回 `success = true` 且消息包含 `Success`/`success`。
   - 测试错误场景：服务端指向一个无效 verification_url，验证失败。
3. macOS 手动测试：
   - 正确 URI → UI Connected + 日志区 `Hello verification passed`。
   - 错误 URI（如 `@2222:443`）→ UI Error + 日志区显示具体错误。

**验收标准**：
- 新增集成测试通过 `cargo test --test hello_protocol`。
- macOS .app 手动验证两种场景。

---

## Task 9: 文档更新

**变更文件**：
- `client/mac/README.md`
- `README.md` 或 `ARCHITECTURE.md`

**具体变更**：
1. 在 `client/mac/README.md` 的构建说明后增加"连接验证"小节，说明 HELLO 机制。
2. 在根 README 或 ARCHITECTURE 中增加协议说明：HELLO/HELLO_ACK 用于启动时验证 client→server→internet。

**验收标准**：
- 文档描述与实现一致。

---

## 推荐实现顺序

1. Task 1（协议帧设计）
2. Task 2（探测目标选择）
3. Task 3（服务端 HELLO 处理）
4. Task 4（Rust 客户端 hello 核心）
5. Task 5（CLI 集成）
6. Task 6（macOS FFI + UI 状态机）
7. Task 7（配置项）
8. Task 8（测试）
9. Task 9（文档）

---

## 关键设计决策摘要

- **协议层**：推荐采用"magic payload in encrypted data frame"方案，避免改动 `FrameFlags` 造成 v2 不兼容；若确认只服务 v3 可新增 `HELLO/HELLO_ACK` flags。
- **探测目标**：默认主目标 `captive.apple.com`，fallback `detectportal.firefox.com`，服务端可配置覆盖。
- **验证时机**：客户端 SOCKS5 监听启动**之前**完成验证，验证失败不监听，防止用户误以为已连接。
- **状态机**：macOS UI 从 `starting` 到 `running` 必须等待真实 HELLO 成功，错误状态显示服务端返回的错误信息。