# Phantom 全 Rust 化改造、握手加固与平台扩展

## 一、静态验证结论（本轮发现的实现缺口）

| 问题 | 证据 | 严重度 |
|---|---|---|
| QUIC 多路复用是死代码 | `client/src/mux.rs` 的 `MuxSession`/`open_stream` 全仓库无调用方；真实路径 `client/src/socks5.rs:70` 每连接新建 QUIC 连接+完整 Noise 握手 | 高 |
| QUIC 的 TLS 层零认证且构成双重加密 | `core/src/transport/quic.rs:263` `NoVerifier` 无条件接受证书；`:275` 每次随机自签证书；两端 `with_no_client_auth()`。真正认证在 Noise IK | 高 |
| Prometheus 统计仅 TUN 模式生效 | metrics server 在 `client/src/tun.rs:577` 的 `TunProxy::run()` 内；`socks5.rs` 无 `TrafficStats` 调用 | 中 |
| 6 个配置字段零消费点 | `zero_copy`/`disguise`/`graceful_migration`/`quic.max_streams`/`quic.keep_alive_interval`/`hello.targets` | 中 |
| SOCKS5 仅支持 CONNECT | `socks5.rs:153` 对非 `0x01` 返回 `0x07` | 中 |

已确认的技术前提：
- `snow` 已是 100% 纯 Rust（curve25519-dalek/aes-gcm/chacha20poly1305/sha2），**TCP 路径已零 C 依赖**；`ring` 唯一来源是 QUIC 栈（`quinn-proto`/`rustls`/`rustls-webpki`/`rcgen`）
- snow 0.9.6 支持 psk 修饰符（`apply_psk_modifier`、`set_psk(location, key)`），当前 pattern 为 `core/src/crypto/noise.rs:7` 的 `Noise_IK_25519_ChaChaPoly_SHA256`
- HarmonyOS 6.0.0 = API 20；本机 Swift 6.3.3 / macOS 26.5.2，`.macOS(.v26)` 可用
- `xtask` 的 `build_server` 无 `--target`，Linux ARM 服务端目前无交叉编译产物

## 二、已确认的决策

1. 100% 纯 Rust，不依赖底层 C；QUIC 保留但改用 Noise 替代 TLS
2. 握手升级为 `Noise_IKpsk2`：线下 PSK **叠加**在 ephemeral DH 之上，保留前向保密
3. 版本下限抬高，不兼容旧设备（鸿蒙 6.0+、macOS 26+）
4. 鸿蒙 VPN 共享先做可行性调研，兜底为代理共享
5. wire 格式不兼容旧版，服务端与客户端需同时升级

---

## 阶段 0：可行性验证（结论决定后续走向）

1. **`quinn-hyphae` PoC**，验证四点：与当前 quinn 版本能否编译共存；能否指定 `IKpsk2` pattern；Noise 消息能否携带自定义 payload（承载现有 `CipherOffer`/`CipherAccept`）；一次握手后能否开多条 bi-stream。任一不满足则回退 `oxiquic-crypto`（纯 Rust rustls provider，文档声明含 QUIC packet/header protection）并单独修 mux 死代码。
2. **鸿蒙透明网关可行性调研**。查明 HarmonyOS 6.0 (API 20) 的 `VpnExtensionAbility` 能否捕获热点转发流量，`@ohos.net.sharing` / `@ohos.net.policy` 等是否对第三方应用开放。判定标准：不 root、不需系统级签名的前提下能否让热点客户端流量进入 TUN。已知不利先例：Android `vpn_hotspot` 依赖 root、iOS 需越狱、鸿蒙 NEXT 已移除 USB 网络共享。兜底见阶段 4.3。

## 阶段 1：握手升级为 Noise_IKpsk2

1. `core/src/crypto/noise.rs:7` 常量改为 `Noise_IKpsk2_25519_ChaChaPoly_SHA256`；`NoiseInitiator`/`NoiseResponder` 的两处 `snow::Builder` 增加 `.psk(2, &psk)`。
2. PSK 生成与存储：`server/src/bootstrap.rs` 生成 32 字节随机 PSK，写入 `./server.key` 第 3 行（沿用现有 600 权限的单文件，第 1 行公钥、第 2 行私钥）。
3. PSK 分发：`core/src/uri.rs` 的 `parse_phantom_uri`/`build_phantom_uri` 增加 `psk=<base64>` 参数；同时支持 `client.toml` 的 `servers[].psk` 覆盖 URI 值。缺失 PSK 时明确报错，不静默降级到无 PSK 握手。
4. 文档需强调：URI 现在是**完整凭据**（服务端公钥 + PSK），必须通过安全渠道传递。
5. 全客户端同步：CLI、macOS、Android、鸿蒙、路由器 `deploy/router/phantom.conf` 的 URI 均自动继承，无需逐个改代码，但需回归验证解析。
6. 收益记入文档：抗主动探测（无 PSK 无法构造首条握手消息，服务端静默丢弃）、抗量子过渡（对称 PSK 抗量子，比 P3 的 ML-KEM 路线立刻可用且便宜）。
7. 测试：`core/src/uri.rs` 补 psk 往返用例；`tests/tests/config_effect.rs` 补「PSK 不匹配必须握手失败」与「缺失 PSK 必须报错」。

## 阶段 2：QUIC 传输层改为 Noise-over-QUIC

一次性解决去 C 化、双重加密、mux 死代码三个问题。

1. `core/src/transport/quic.rs` 重写：删除 `NoVerifier`、`generate_self_signed_cert()` 与 rustls 配置路径，改用 `quinn-hyphae` 以阶段 1 的 `IKpsk2` 作为 quinn 的 crypto backend；`CipherOffer`/`CipherAccept` 迁入 Noise 握手 payload。
2. 删除已无用组件：`client/src/mux.rs` 整体删除（quinn 原生提供「一次握手 + N stream」）；`core/src/crypto/session.rs` 的 `split_for_stream` 删除（唯一调用点 `server/src/handler.rs:127`）；移除 `rcgen` 依赖；QUIC stream 不再叠加 `SessionReader`/`SessionWriter`，直接跑帧协议。
3. 新增 `client/src/quic_pool.rs`：按 `server.address` 缓存已认证的 `quinn::Connection`，`tokio::sync::OnceCell` 去重并发首建，`close_reason()` 非空时剔除重建。`client/src/socks5.rs` 的 QUIC 分支改为取连接后 `open_bi()`。
4. `server/src/handler.rs` QUIC 分支简化：连接级已认证，客户端公钥白名单校验从 stream 级提到连接级，每个 bi-stream 直接进帧协议循环。
5. TCP 路径保持不变（`split_after_handshake` + 自研 AEAD 会话层仍必要，TCP 无内置加密）。
6. 验收：`cargo tree -i ring` 无输出；新增 `tests/tests/quic_mux.rs` 断言 N 条 SOCKS5 连接只触发 1 次 Noise 握手且 stream 间不串流；`tests/bench` 补握手开销前后对比。

## 阶段 3：统计与配置契约对齐

1. metrics 提层：把 `tun.rs:577` 的 metrics HTTP server 移到 `client/src/tunnel.rs`，`run()` 与 `run_tun()` 统一启动；地址改为可配置 `client.metrics_listen`（默认 `127.0.0.1:9150`）。
2. `handle_socks5_connection` 增加 `stats: &Arc<TrafficStats>`，在 relay 双向拷贝处记账连接数与上下行字节。
3. 死配置逐项处置：
   - 实现 `quic.max_streams` → quinn `TransportConfig::max_concurrent_bidi_streams`
   - 实现 `quic.keep_alive_interval` → `TransportConfig::keep_alive_interval`
   - 实现 `hello.targets` → 客户端随 Hello 帧下发候选 URL，服务端 `handler.rs:436` 优先使用，缺省回落 `verification_url`
   - 实现 `failover.graceful_migration` → `false` 时切服广播 shutdown 主动断连
   - 删除 `performance.zero_copy`（语义被 io_uring 覆盖）与 `tls.disguise`（阶段 2 后 QUIC 无 TLS，该字段彻底失去语义）
4. 补全 ARCHITECTURE.md §5 契约表遗漏的三行；重写 §6.1/§6.4 与 README「协议设计」以反映 IKpsk2 与 Noise-over-QUIC。

## 阶段 4：入站协议能力与局域网共享

1. **UDP ASSOCIATE**：`socks5.rs` 增加 `CMD=0x03`。绑定本地 UDP socket 回 BND.ADDR/PORT；解析 `RSV[2] FRAG[1] ATYP ADDR PORT DATA`，`FRAG != 0` 丢弃；TCP 控制连接 EOF 时销毁关联。把 `tun.rs` 的 `spawn_udp_proxy_flow` 抽到 `client/src/udp_relay.rs` 与 TUN 路径共享。
2. **HTTP 代理入站**：新增 `client/src/http_proxy.rs`，支持 `CONNECT host:port` 与绝对 URI 的 GET/POST。采用**同端口协议嗅探**：peek 首字节，`0x05` 走 SOCKS5，否则走 HTTP。
3. **局域网代理共享**（鸿蒙/Android/macOS 共用，兜底方案）：监听地址可配置 `0.0.0.0`，新增 `client.proxy_auth`（SOCKS5 RFC1929 + HTTP Basic）。默认仅监听 `127.0.0.1`，开启共享需显式配置以避免误暴露。各端 UI 展示内网 IP、端口、凭据与二维码。若阶段 0 证明鸿蒙可做透明网关则追加该路径。
4. 测试：`tests/tests/socks5_udp.rs`、`tests/tests/http_proxy.rs`（CONNECT + 明文 GET + 嗅探分流 + 认证）。

## 阶段 5：平台扩展

1. **鸿蒙 6.0 客户端**：`client/harmony/build-profile.json5` 的 `compatibleSdkVersion` 改 `"6.0.0(20)"`。前置需确认本机 DevEco SDK 含 API 20。
2. **鸿蒙服务端（局域网自用）**：
   - `server/src/bootstrap.rs` 的 `AutoOptions` 增加 `work_dir: Option<PathBuf>`，把基于 CWD 的 `server.key`/`server.toml` 路径改为基于该目录（默认仍 CWD，CLI 行为不变）。这是鸿蒙沙箱的前置条件。
   - `client/harmony/Cargo.toml` 增加 `phantom-server` 依赖；需实测 ohos target 可编译（`target_os = "linux"` 在 ohos 为真，重点验证 `server/src/lib.rs:228` 的 Linux 分支）。
   - `rust/src/lib.rs` 新增 napi：`phantom_harmony_server_start(work_dir, port, cipher, proto) -> String`（返回含 PSK 的 URI）、`..._server_stop()`、`..._server_status()`。
   - `module.json5` 增加 `ohos.permission.GET_NETWORK_INFO` 与 `ohos.permission.KEEP_BACKGROUND_RUNNING`。
   - ArkTS 新增服务端页面：端口输入、启停、内网 IP 与 URI 展示、二维码。
3. **macOS 26**：`client/mac/Package.swift:14` 改 `platforms: [.macOS(.v26)]`；`Info.plist` 补 `LSMinimumSystemVersion = 26.0`（当前缺该字段）。
4. **Linux ARM 服务端**：`xtask` 新增 `server-arm64`，triple `aarch64-unknown-linux-musl`（静态、零部署依赖），支持透传 `--features io-uring`。Ubuntu 24.04+ 内核满足要求。
5. **armv7 路由器**：`xtask` 新增 `router-armv7`，triple `armv7-unknown-linux-musleabihf`（`Cross.toml` 已有条目）。
6. **构建链路简化**（去 C 化的直接收益）：阶段 2 完成后全仓库无 C 依赖，删除 `.cargo/config.toml` 中为 ring 准备的 `CC_*`/`CFLAGS_*`/`AR_*` 覆盖，以及 `xtask` 的 `find_router_cc`/`router_ar` 探测逻辑（约 80 行）。交叉编译只需 `rustup target add`，`Cross.toml` 亦可删除。
7. RT-AX86U Pro 由现有 `router`（aarch64 musl）目标覆盖，做回归确认。

## 阶段 6：依赖与镜像源

1. `.cargo/config.toml` 切换到字节跳动 rsproxy：
   ```toml
   [source.crates-io]
   replace-with = 'rsproxy-sparse'
   [source.rsproxy-sparse]
   registry = "sparse+https://rsproxy.cn/index/"
   ```
2. 升级 quinn / tokio / snow 等至最新兼容版本，跑全量测试确认无回归。
3. 确认 `CongestionAlgorithm::Bbr` 真实下发到 quinn 的 `TransportConfig`。

## 顺带修复

- `cli_system` 未声明对 `target/debug/phantom` 的构建依赖，并发重链接时假失败：测试内先断言二进制存在并给出明确提示。
- PROJECT_PLAN.md §3.2 已记录的两个先存问题（`correctness::tcp_aes256gcm_echo_large` 永久挂起、`http_tunnel` 空响应体）：二者共用 `connect_tunnel` + `handler` 裸 relay 路径，怀疑 EOF/收尾竞态。先加测试超时避免卡死，本轮排查根因。

## 验证方式

- 每阶段跑 `cargo test --lib` + `cargo test --bins` + `cargo test -p phantom-e2e --test cli_system`
- 100% Rust 验收：`cargo tree -i ring` 无输出；删除 clang 探测后各交叉目标仍能构建
- 各交叉目标 `cargo xtask build <target>` 后用 `file` 确认架构与静态链接
- PSK 验收：PSK 不匹配必须握手失败；无 PSK 的扫描流量被静默丢弃
- 鸿蒙/macOS 需真机安装验证（版本下限已按要求抬高，旧设备无法安装）

## 待确认的外部依赖

1. `quinn-hyphae` 与当前 quinn 版本的兼容性及 `IKpsk2` 支持（阶段 0 验证，不通则回退 `oxiquic-crypto`）
2. 本机 DevEco Studio 是否已含 HarmonyOS SDK API 20
3. 鸿蒙真机是否为 6.0+

---

## 执行状态总览（2026-08-21 更新）

阶段 0–6 全部完成，仅剩 p2e（鸿蒙 ArkTS 构建验证）收尾。已验证：Rust .so 编译链接成功（ELF aarch64）、hvigor ArkTS 编译通过、签名出包；已修 `build-profile.json5` 的 `targetSdkVersion: "6.1.1(24)"`。

**遗留缺陷**：HAP 内无 `libphantom_harmony.so`（unzip 实证 9 文件/184KB；hvigor `intermediates/libs` 与 `stripped_native_libs` 均为空目录）。根因：hvigor 从模块根 `entry/libs/<abi>/` 拾取 native libs，xtask 错误复制到 `entry/src/main/libs/arm64-v8a/`。

## 阶段 7：p2e 收尾

1. `xtask/src/main.rs`：L550 `entry/src/main/libs/arm64-v8a` → `entry/libs/arm64-v8a`（L541 注释同步）；L809 clean_all `client/harmony/entry/src/main/libs` → `client/harmony/entry/libs`。
2. `client/harmony/.gitignore`：`entry/src/main/libs/` → `entry/libs/`；补 `/*-signed.hap`（构建产物当前未被忽略）。
3. 删除旧目录 `entry/src/main/libs/`，重跑 `cargo xtask build harmony`。
4. 验收：`unzip -l entry-default-signed.hap` 必须含 `libs/arm64-v8a/libphantom_harmony.so`（约 8.7MB）。
5. 可选模拟器验证：`hdc install` + 启动，确认 NAPI 模块加载无报错。
6. 记忆修正：bb5d2ef0 / c1e6ce20 / 9f1e1119 三条记忆中的 libs 路径描述改为 `entry/libs/arm64-v8a/`。

## 阶段 8：Skill 采用（依据 skills-research-harmonyos-rust.md）

推荐安装 4 个（精益原则，避免上下文稀释）：

| Skill | 理由 |
|---|---|
| `fadinglight9291117/arkts_skills@harmonyos-build-deploy` | 对口 hvigor 打包/签名问题域 |
| `web-infra-dev/midscene-skills@harmonyos-device-automation` | 模拟器 UI 自动化验证 |
| `wshobson/agents@rust-async-patterns` | tokio 异步模式，对口 quic_pool/udp_relay/failover |
| `apollographql/skills@rust-best-practices` | Rust 通用基线 |

不装：`android-to-harmonyos-migration-workflow`（无迁移需求）、`rust-mcp-server-generator`（无关）、`rust-testing`/`rust-patterns`/`m15-anti-pattern`/`rust-engineer`（与已装 `rust-coding-guidelines` 重叠）。

安装命令：`npx skills add <id> --directory ~/.qoder/skills -y`

## 阶段 9：eBPF 评估与性能路线结论

**结论：不引入 eBPF。**

1. 数据路径：端到端 AEAD 加密代理，流量必须在用户态完成 crypto；eBPF 核心加速场景（sockmap 内核直通、XDP/TC 内核态转发）只对明文直通流有效，进出内核边界各一次是协议下限，无法消除。
2. 平台矩阵：仅 Linux 服务端理论可用（收益面 1/5）。RT-AX86U Pro 内核 4.19 无 BTF 不可用；macOS 无 eBPF；HarmonyOS NEXT 非 Linux 内核；Android 需 root。
3. 工程约束：aya 需 bpf-linker + nightly + bpf target，与「100% Rust 零 C、构建链简化」（阶段 5.6）冲突。

「减少内核切换 / 零拷贝」路线现状：

| 技术 | 状态 | 剩余工作 |
|---|---|---|
| io_uring | 已实现（`server/src/linux_ext.rs`，gnu-only） | musl 下 no-op 为已知取舍；可选 gnu 动态链接 server 变体 |
| AEAD in-place | 已实现（`aead_state.rs`，reader/writer 全用） | 无 |
| QUIC GSO/GRO | quinn-udp Linux 默认自动探测 | bench 验收时确认 |
| BBR / max_streams / keep_alive | 已实现（`build_transport_config`） | 无 |
| `core/src/buf_pool.rs` BufferPool | 死代码（无消费点） | 建议删除；保留则需接入 codec 读写路径 |

未来可选（不进当前计划）：服务端 XDP 抗端口扫描（纵深防御；Noise IKpsk2 已提供首包静默丢弃）。
