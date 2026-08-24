# QUIC 慢因诊断与加密矩阵测试

## 背景

上轮 E2E 实测：TCP 隧道 95.2 MB/s，QUIC 隧道 40.1 MB/s（同为 1 vCPU 容器）。用户要求：查明 QUIC 慢因；测试常用加密算法（含鸿蒙端）的吞吐、弱网、闪断表现，选出最接近实际使用的组合。

现状盘点（已核实）：
- `core/src/transport/quic.rs:188` `build_transport_config` 只配置拥塞控制/max_streams/keep_alive，其余全用 quinn 0.11.11 默认值（无 GSO/MTU/ack 显式调优）
- 两条路径加密实现不同：TCP = 应用层 AEAD（`core/src/crypto/aead_state.rs`，aes-gcm/chacha20poly1305/ascon crate）；QUIC = quinn-hyphae 的 Noise 包保护
- `tests/bench/benches/aead_throughput.rs`（divan）：4 cipher 加密微基准已有，仅 encrypt、仅 macOS 跑过
- `tests/tests/weak_network.rs`：ThrottledProxy（用户态节流）只覆盖 TCP 路径、只用 AES-256-GCM；无 QUIC 弱网
- 无闪断/failover e2e 测试（`client/src/failover.rs` 有实现无 e2e）

## 阶段 A：QUIC 慢因分层诊断

三个嫌疑变量：gvproxy UDP 逐包转发、用户态 QUIC 栈 vs 内核 TCP 栈、加密实现差异。

1. **L1 绕过 gvproxy**：恢复上轮 podman 环境（rootful，脚本见 `tests/e2e/`），把 musl CLI 客户端（base64 通道传 VM，方法见记忆 bdcc3e9f）在 VM 内直连 phantom-server-quic 容器 IP，跑 10MB 吞吐。与宿主机经 gvproxy 的 40 MB/s 对照——若 VM 内显著回升（预期 >80MB/s），证明 gvproxy UDP 转发是主要放大因素。
2. **L2 传输栈开销**：VM 内 TCP 直连同跑一轮（对照 L1 QUIC），得到「无 gvproxy 时 QUIC/TCP 真实比值」。
3. **L3 加密实现**：审查 quinn-hyphae 使用的 cipher（Cargo.lock + 源码定位），与 `aead_throughput` 微基准对齐比较；确认 QUIC 包保护是否支持 AES-GCM 或仅 ChaCha。
4. **代码审查**：quinn-udp 0.5.14 GSO 生效条件（日志级确认）、MTU 探测行为、`TransportConfig` 可调项清单。
5. 产出结论：慢因归因表（gvproxy 占比 / 用户态栈占比 / cipher 占比）+ 优化建议清单（只建议，不改代码）。

## 阶段 B：加密算法吞吐矩阵

1. **新增跨平台 cipher 微基准**：`core/examples/cipher_bench.rs`（std + phantom-core，手写计时循环，不用 divan 以便交叉编译；encrypt+decrypt 双向，64KB 块 × N 次）。三个目标编译：`cargo build --release`（macOS）、`--target aarch64-unknown-linux-musl`（容器）、`--target aarch64-unknown-linux-ohos`（模拟器）。
2. **微基准执行**：
   - macOS 宿主机（M 系列 ARMv8 + crypto extensions）
   - Linux ARM：musl 二进制在 podman 临时容器内跑（`--cpus 1` 与上轮一致）
   - 鸿蒙：ohos 二进制 base64 推到模拟器 `/data/local/tmp/`（PTY 二进制损坏坑见记忆），`hdc shell` 执行
3. **链路级吞吐**（真实协议栈）：复用上轮 phantom-web + 双服务端容器架构，URI 的 `cipher=` 参数驱动 4 cipher × TCP(443)/QUIC(8443) 共 8 组合 × 10MB 下载测速（服务端日志核对 `Client connected (cipher=...)` 与所选一致）。
4. 产出：cipher × 平台 × 协议吞吐矩阵表。

## 阶段 C：弱网测试（内核级 netem）

比现有用户态 ThrottledProxy 更真实——作用于真实 veth，覆盖 TCP+UDP 双协议。

1. rootful podman VM 内对服务端容器 veth 加 netem（`podman machine ssh` + `tc qdisc add dev <veth> root netem ...`，veth 名经 `podman inspect` 定位）。
2. 场景矩阵（贴近实际）：
   - 跨境优链路：`delay 100ms loss 1%`
   - 恶劣移动网：`delay 300ms loss 5%`
   - 限速：`delay 50ms rate 10mbit`
3. 每场景跑 TCP/QUIC × 代表性 cipher（AES-256-GCM + ChaCha20 或按阶段 B 结果选快慢两端）× 1MB 传输，记录吞吐与完成时间。
4. 测完 `tc qdisc del` 清理。
5. 产出：弱网下 QUIC（丢包恢复优势 vs 用户态开销）与 TCP 的真实差距、cipher 是否影响弱网表现（预期不显著，验证之）。

## 阶段 D：闪断测试

1. **服务端重启**：curl 持续经隧道下载（循环小文件）中途 `podman restart phantom-server(-quic)` → 记录：失败窗口时长、客户端是否自动重连（客户端日志）、恢复后吞吐。
2. **网络闪断**：传输中途 VM 内 `tc` 插入 3 秒 100% 丢包再恢复 → 观察两端重连行为（QUIC idle timeout vs TCP 断连语义差异）。
3. **多服务器 failover**：起第二组服务端容器（不同端口），客户端 `client.toml` 配 `[[servers]]` 两项 → 停主服务器 → 验证 `client/src/failover.rs` 切换（客户端日志的切换事件 + 恢复时间 + `graceful_migration` 行为）。
4. 产出：闪断恢复时间表 + failover 实测结论；如有缺陷记录为 issue 清单。

## 阶段 E：鸿蒙端链路验证

1. **模拟器当服务端**（真实鸿蒙服务端场景）：模拟器启动 app → Server Tab 启动服务端 → `hdc fport tcp:PORT tcp:PORT` 转发到宿主机 → 宿主机 CLI 连接跑吞吐（URI 从 ServerPage 经 dumpLayout 抓取）。每种 cipher 一轮（ServerPage 的 cipher 选择为 UI 控件，uitest 驱动）。
2. **客户端链路**：定制测试包（上轮验证过的 @State 注入法，测后还原）对快/慢两端 cipher 各打一包，观察 Hello verification 耗时与稳定性。
3. 局限性注明：模拟器跑在 M 芯片上，性能数据趋势可参考、绝对值不等于真机麒麟芯片。

## 产出与收尾

1. 测试报告 `tests/E2E_CIPHER_REPORT.md`：QUIC 慢因归因、cipher×平台×场景推荐矩阵（含「最接近实际使用」结论）、弱网/闪断结论、QUIC 优化建议 backlog。
2. 新增资产：`core/examples/cipher_bench.rs`、netem/闪断脚本归档 `tests/e2e/`。
3. 测完释放全部资源（容器/网络/镜像/netem 规则/模拟器/machine/临时目录），恢复现场。

## 关键风险与对策

- `tc` 需在 rootful VM 内操作（上轮已切 rootful，直接复用）
- netem 作用于 veth 方向需实测确认（ingress/egress），先单场景验证再跑矩阵
- QUIC 弱网测试若因 podman UDP 转发失真，以 VM 内直连结果为准并注明
