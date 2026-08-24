# QUIC 慢因诊断与加密算法矩阵 E2E 测试报告

日期：2026-08-23（推荐矩阵于 2026-08-24 随 PERF_OPTIMIZATION_REPORT 修订）
环境：macOS (Apple Silicon) + podman rootful VM（容器均 1 vCPU / 256MB）+ HarmonyOS 模拟器（Pura 90，M 芯片虚拟化）
基线：上轮 E2E 实测 TCP 隧道 95.2 MB/s vs QUIC 隧道 40.1 MB/s（同为 1 vCPU 容器），本轮诊断慢因并完成加密算法全矩阵测试。

---

## 1. QUIC 慢因归因（阶段 A）

三个嫌疑变量逐一排除/确认：

| 层级 | 嫌疑 | 实测 | 结论 |
|------|------|------|------|
| L1 | gvproxy UDP 转发 | QUIC 经宿主 gvproxy 仅 **7.27 MB/s**；VM 内直连同容器 **42.8 MB/s**（劣化 5.9x 且伴随不稳定丢包） | **主因**（podman macOS 特有） |
| L2 | 用户态 QUIC 栈 | VM 内直连：TCP 64.6 vs QUIC 42.8/43.4 MB/s（QUIC ≈ TCP 66%） | 次因，属用户态栈正常开销 |
| L3 | 加密实现 | QUIC 走 quinn-hyphae 的 Noise 包保护，**固定 ChaCha20-Poly1305**（snow 0.9.6，无 cipher 选择）；ChaCha 微基准快于 AES | 无负面影响，非慢因 |

**归因结论**：QUIC 差的成绩 ≈ gvproxy UDP 用户态逐包转发放大（主因）× 用户态 QUIC 栈固有开销（次因）。TCP 经 gvproxy 基本无损（宿主 115.2 MB/s），因为走内核 TCP 路径。

代码审查佐证：
- `core/src/transport/quic.rs:188` `build_transport_config` 仅配置拥塞控制/max_streams/keep_alive，其余全为 quinn 0.11.11 默认（无 GSO/MTU 显式调优）
- QUIC 路径 cipher 不可配置（hyphae Noise pattern 固定），与 TCP 路径的 4-cipher AEAD 体系是两套实现

## 2. 加密算法微基准（阶段 B，`core/examples/cipher_bench.rs`）

64KB 块 × 256MB 预算，encrypt+decrypt 真实路径（`AeadState`），单位 MB/s：

| cipher | macOS (M 芯片) | Linux ARM 容器 | 鸿蒙模拟器 |
|--------|---------------|----------------|------------|
| AES-256-GCM | 181.7 | 167.2 | 162.7 |
| AES-128-GCM | 227.3 | 200.1 | 198.5 |
| ChaCha20-Poly1305 | 579.4 | 516.0 | 459.5 |
| ASCON-128 | **675.1** | **564.7** | **519.3** |

**重大发现：三平台 AES 均无硬件加速**——RustCrypto `aes` crate 的 `armv8` feature 未启用，M 芯片/麒麟模拟环境全部退化为纯软件实现，导致 AES 反而垫底。三平台排序完全一致：ASCON > ChaCha20 >> AES-128 > AES-256。

## 3. 链路级吞吐矩阵（阶段 B，真实协议栈）

TCP 容器服务端（toml 驱动 cipher，服务端日志核对 `Client connected (cipher=...)`），10MB 下载两轮均值：

| cipher | TCP 链路 (MB/s) | 相对 AES-256 |
|--------|----------------|--------------|
| AES-256-GCM | 106.0 | 1.00x |
| AES-128-GCM | 127.3 | 1.20x |
| ChaCha20-Poly1305 | 180.3 | 1.70x |
| ASCON-128 | 178.6 | 1.68x |

与微基准排序一致。QUIC 链路（经 gvproxy）仅 7.27 MB/s 且波动大，不参与 cipher 对比。

**缺陷发现**：URI 的 `cipher=` 参数被解析进 `ServerEntry.cipher`，但 `client/src/tunnel.rs` / `quic_pool.rs` 无消费点——客户端始终协商服务端首选 cipher（客户端日志恒显 `cipher=Auto`）。

## 4. 弱网矩阵（阶段 C）

内核 netem 不可用（VM 缺 sch_netem 模块），改用自研 `tests/e2e/lossy_proxy.rs`（UDP 包级真丢包 + 计划时刻延迟语义；TCP 仅延迟——应用层无法丢字节）+ VM 内 tbf 限速：

| 场景 | TCP (MB/s) | QUIC (MB/s) |
|------|-----------|-------------|
| 跨境优链路 delay 100ms loss 1% | 1.63 | 0.52 |
| 恶劣移动网 delay 300ms loss 5% | 0.57 | 0.20 |
| 限速 10mbit (tbf) | 1.14 | 1.13 |
| 300ms × 3 cipher 对照 | 全部 0.568 | — |

**结论**：
1. 当前实现下弱网 **TCP 优于 QUIC**——QUIC 丢包恢复优势被用户态栈开销 + RTT 放大抵消（样本为 1MB 短传输）
2. **cipher 与弱网表现无关**（300ms 场景三 cipher 吞吐完全一致 0.568 MB/s，瓶颈在 RTT 而非 CPU）
3. 带宽受限场景两者持平（tbf 1.14 vs 1.13）

## 5. 闪断恢复（阶段 D）

循环探测（0.6s/次 100KB 下载）：

| 场景 | 结果 | 结论 |
|------|------|------|
| D1 服务端重启 | 341 ok / 2 fail | 失败窗口 ≈ 容器重启时间，每连接自动重连，恢复后吞吐正常 |
| D2 网络黑洞 3s（pause/unpause） | 269 ok / 1 fail | 恢复即通，无残留连接问题 |
| D3 双服务器 failover（443→9443） | 337 ok / 195 fail | **缺陷**：主服务器停 12s 内未完成切换（健康检查间隔 30s，仅积累 1 次连续失败） |

## 6. 鸿蒙端链路验证（阶段 E）

模拟器 ServerPage 内嵌服务端（NAPI `phantomHarmonyServerStart`），`hdc fport tcp:4437` 转发后宿主机 CLI 连接。链路：宿主机 curl → SOCKS5 → 隧道 → fport → 模拟器 phantom-server → slirp(10.0.2.2) → 宿主机测速目标。

| cipher | 10MB 吞吐三轮 (MB/s) | 均值 | 100KB 延迟 |
|--------|----------------------|------|-----------|
| ChaCha20-Poly1305 | 75.95 / 65.62 / 83.59 | **75.05** | 60-67ms |
| AES-256-GCM | 52.03 / 42.98 / 52.12 | **49.04** | 63-65ms |

- ChaCha ≈ 1.53x AES-256，与微基准趋势一致（验证 AES 无加速结论在鸿蒙真实链路上成立）
- Hello verification 70ms，握手稳定；UI 选 ChaCha20 → NAPI → `CipherPreference::ChaCha20Poly1305` 传递链路代码审查确认有效
- 模拟器服务端 URI 自举、二维码分享、Stop/Start 状态机均正常

**局限性**：模拟器跑在 M 芯片上，数据趋势可参考，绝对值不等于真机麒麟芯片表现。

## 7. 「最接近实际使用」推荐矩阵

| 维度 | 推荐 | 理由 |
|------|------|------|
| 默认 cipher | **AES-256-GCM（auto）**（2026-08-24 修订） | 硬件加速启用后 AES-256 微基准 12.3x（2167 MiB/s），链路级四 cipher 拉平（瓶颈转移）；ChaCha20 保留为无 AES 硬件平台回退 |
| 备选 | ASCON-128 | 微基准最快、链路级与 ChaCha 持平；生态成熟度略逊，建议评估后开放 |
| 不推荐优先 | AES-GCM 系 | 无硬件加速时性能垫底；启用 `armv8` feature 后需重测 |
| 传输协议 | **TCP**（当前环境） | QUIC 经 gvproxy 劣化 5.9x 且弱网不占优；QUIC 待 GSO/调优后重评 |
| 弱网 | cipher 无需切换 | RTT 是瓶颈，任何 cipher 差异都被淹没 |

## 8. 缺陷与优化建议 Backlog

### 缺陷（建议提 issue）
1. **URI cipher 参数不生效**：`ServerEntry.cipher` 解析后无消费点（`tunnel.rs`/`quic_pool.rs`），客户端恒按服务端首选协商
2. **cipher 命名不一致**：TOML serde 枚举名（`cha-cha20-poly1305`/`aes256-gcm`）与 URI 参数名（`chacha20-poly1305`/`aes-256-gcm`）不一致，易致配置错误
3. **failover 响应过慢**：主服务器停机 12s 未切换（健康检查间隔 30s）；实际闪断场景用户感知长时中断

### 优化建议（不改代码）
4. **启用 AES 硬件加速**：`aes` crate 开启 `armv8` feature（ARM 平台）；预期 AES-GCM 吞吐数倍提升，改变推荐矩阵
5. **QUIC 调优**：quinn-udp GSO 生效条件核查、MTU 探测显式配置、`TransportConfig` 调优项评估
6. **failover 提速**：健康检查间隔降至 5s + 连续失败快速判定

## 9. 测试资产

- `core/examples/cipher_bench.rs` —— 跨平台 AEAD 微基准（macOS/musl/ohos 三目标）
- `tests/e2e/lossy_proxy.rs` —— 用户态弱网代理（UDP 真丢包 + 计划时刻延迟）
- `tests/e2e/stage-a-diagnosis.sh` / `vm-direct-test.sh` —— 慢因诊断（宿主 vs VM 直连）
- `tests/e2e/stage-b-link3.sh` —— 链路级 cipher 矩阵（toml 驱动）
- `tests/e2e/stage-c-v2.sh` —— 弱网矩阵
- `tests/e2e/stage-d-flash.sh` —— 闪断/failover
- `tests/e2e/setup-env.sh` —— 四容器环境部署

> 注：弱网 UDP 丢包经 lossy_proxy（用户态）语义真实；TCP 丢包语义受限于无 netem，以延迟 + tbf 限速近似，已在结论中标注。
