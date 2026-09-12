# 鸿蒙客户端 + Linux 服务端：传输吞吐与弱网优化（最大化）

## 现状锚点（已完成，数据保留为基线）

| 阶段 | 内容 | 关键结果 |
|---|---|---|
| p0 | 基线 + bench-suite.sh | TCP 102 MB/s、AES-256 微基准 177 MiB/s |
| p1 | AES/NEON 硬件后端 cfg（三 target rustflags）+ auto_detect 缺陷修复 | AES-256 macOS 177→1510 MiB/s（8.5x） |
| p2 | LTO fat + cgu=1 + panic=abort | AES-256 累计 11.8x（2083 MiB/s） |
| p3 | 帧 16KB→65504 + 协议 v3 + writev | 链路 TCP 102→265 MB/s（2.6x）；鸿蒙新协议栈 Hello 已通 |

当前未闭环项：鸿蒙数据面 TUN（UI 传占位 fd=-1）、Phase 4/5 优化、完整测试矩阵、真机验证。

## 总目标（锚定历史最差数据）

| 维度 | 历史最差 | 目标 | 手段 |
|---|---|---|---|
| 容器 TCP 链路（1vCPU） | 95-106 MB/s | ≥400 MB/s | p3 已达 265；BDP 缓冲+多核 |
| QUIC 链路（VM 内直连口径） | 42.8 MB/s | ≥100 MB/s | QUIC 窗口/MTU/GSO 调优 |
| 鸿蒙客户端→容器服务端 | 未通过（TUN 占位） | 打通且 ≥100 MB/s | 完整 VpnExtensionAbility |
| 鸿蒙链路（模拟器服务端口径） | 75 MB/s | ≥150 MB/s | 硬件加速+64KB 帧复测 |
| 弱网 300ms+5% | 0.57 MB/s | ≥5 MB/s（10x） | BDP 窗口+BBR+pacing |
| failover 切换 | 12s 不切换（缺陷） | ≤5s | 健康检查 30s→5s、阈值 3→2 |

## Phase A：鸿蒙完整 VPN 数据面打通（最高优先级）

目标：模拟器浏览器/app 流量经真实 TUN → phantom 客户端 → 容器服务端 → phantom-web，返回 PHANTOM-E2E-OK。

1. **ArkTS UI 侧**（`client/harmony/entry/src/main/ets/pages/Index.ets`）：Start Tunnel 改为——先把 serverUri/mode 写入 `@kit.ArkData` preferences（`dataPreferences.getPreferencesSync`），再 `vpnExtension.startVpnExtensionAbility({bundleName:'co.phantom.harmony', abilityName:'PhantomVpnExtensionAbility'})`；处理首次系统授权弹窗（uitest dumpLayout 定位「允许」按钮点击）。
2. **VPN Ability**（`entry/src/main/ets/vpnextability/PhantomVpnExtensionAbility.ets`）：onCreate 读 preferences → `vpnClient.createVpnConnection(this.context)` → `establish()` 配置：
   - addresses: `10.8.0.2/32`；mtu 1500；dnsAddresses 按 client 配置
   - routes: `0.0.0.0/1` + `128.0.0.0/1`（两半覆盖全网）——**防回环设计**：模拟器内服务端 10.0.2.2 属本地直连网段 10.0.2.0/24，最长前缀匹配优先于 /1，隧道 socket 天然绕行 TUN，无需 protect 回调（公网服务端场景的 protect 列为已知限制）
   - establish 回调拿 tunFd → `phantomLib.phantomHarmonyStart(tunFd, uri, mode)`
3. **状态回传（简化）**：VPN Ability 侧 ArkTS 起 1s 定时器读 `phantomHarmonyGetStatus()/GetLastError()` 写 preferences；UI 轮询 preferences 更新状态与日志区。详细日志经 `hilog` 断言。
4. **正向断言**：app 内 ArkTS `@kit.NetworkKit` http 模块请求 `http://<phantom-web容器IP>:8080/`（容器名无法经系统 DNS 解析，TUN 模式必须用容器 IP，服务端在 phantom-net 内直连）——返回含 PHANTOM-E2E-OK；服务端容器日志出现 `SYN →` + `Relay done` 字节对账。
5. **负向断言**：VPN 关闭时同请求必失败（对照组）。
6. **鸿蒙链路性能**：app 内下载 `http://<phantom-web-IP>:8080/10mb.bin` 计时测吞吐 ×3 轮；同时复测「模拟器当服务端 + hdc fport + 宿主机 CLI」口径（历史 75 MB/s 锚点对比）。

## Phase B：传输栈参数与并发（原 Phase 4）

1. 引入 `socket2`：TCP 侧按 BDP 设 SO_SNDBUF/SO_RCVBUF（`core/src/transport/tcp.rs`），默认值可配置；弱网 300ms×100Mbps 按 3.75MB 设。
2. QUIC（`core/src/transport/quic.rs`）：补 `stream_receive_window`/`receive_window`/`send_window`（按 BDP）、`initial_mtu` 1500 + `mtu_discovery_config`、`initial_rtt`；QUIC 吞吐以 **VM 内直连** 口径测量（绕过 gvproxy 劣化，历史锚点 42.8 MB/s）。
3. quinn-udp GSO/GRO 平台生效核查（日志确认）。
4. 并发：服务端容器放开 `--cpus 4` 对照 1vCPU；验证 workers 配置；单流是否 CPU 饱和（`podman stats` 压测观察）。
5. io_uring：`aarch64-unknown-linux-gnu` + `io-uring` feature 构建对照 musl 版（仅容器可用，路由器内核 4.19 不支持）。

## Phase C：弱网优化（原 Phase 5）

1. BDP 自适应：按实测 RTT 动态调整接收窗口。
2. 默认拥塞控制 Cubic→BBR（`core/src/config.rs` 默认值），QUIC 启用 pacing。
3. **failover 缺陷修复**：健康检查 30s→5s、阈值 3→2；重测闪断场景断言切换 ≤5s（历史缺陷：12s 不切换）。
4. FEC（已授权）：帧层可选 XOR/Reed-Solomon 冗余，仅 QUIC/UDP 路径；lossy_proxy 300ms+5% 真丢包验证；限速场景验证不反向恶化。
5. 多路径（WiFi+有线聚合）：先出可行性结论再决定是否实现。

## Phase D：完整测试矩阵执行（最大化覆盖）

扩展 `tests/e2e/bench-suite.sh` 为全矩阵驱动（每格记录 BENCH[tag] 结构化输出）：

1. **cipher 矩阵**：AES-256-GCM / AES-128-GCM / ChaCha20-Poly1305 / ASCON-128 × 平台（macOS 本机 / 容器 1vCPU / 容器 4vCPU / 鸿蒙模拟器）× 口径（微基准 + TCP 链路 + QUIC 链路）。
2. **弱网矩阵**：场景（100ms+1% 跨境优链 / 300ms+5% 恶劣移动 / tbf 10mbit / tbf 1mbit / jitter 50ms±25）× TCP/QUIC × 快慢两端 cipher（AES-256 + ChaCha）；优化前后各一轮对比。
3. **重联/闪断矩阵**：服务端重启恢复窗口、网络黑洞 3s（pause/unpause）、双服务器 failover 切换时间、QUIC vs TCP 恢复语义差异。
4. **真实场景**：mock web 正负断言（CLI + 鸿蒙双端）、三方字节对账（curl size ≈ 服务端 Relay done ≈ 客户端 metrics，误差 <5%）、安全边界（篡改 PSK/公钥必失败）。
5. 每阶段跑 `cargo test --release --workspace --exclude phantom-harmony` 全绿门槛。

## Phase E：真机与路由器（低影响）

1. **第二台 Mac**（`user@<内网主机IP>`，密码不写入任何文档）：首连部署公钥免密 → 探测机型/芯片/网口速率（1GbE→118 / 2.5GbE→295 MB/s 封顶）→ 源码构建服务端 → LAN 线速对测（本机 phantom client ↔ 对端 phantom server，打满网口）。
2. **路由器**（<路由器IP>，硬约束：承载真实流量）：仅只读探测 `cat /proc/cpuinfo`（确认 A53 是否含 aes/pmull，决定路由器侧 cipher 推荐）、`/proc/version`、`nproc`、`free -m`；零写入。如后续要跑服务端：静态 musl 二进制放 /tmp + nice 19 + 单核 + 内存上限 + 测完即删。
3. 鸿蒙链路在真机 LAN 路径下复测（模拟器 → 路由器 → 第二台 Mac 服务端）。

## Phase G：路由器客户端实测 + WiFi 极限 + 全面清理（收尾）

现状：路由器 musl CLI 已部署（/tmp/phantom）；server 模式 cipher 矩阵已测（aes-256-gcm 56.7 / aes-128-gcm 57.0 / chacha20 55.1 MB/s，A53 硬件 AES 非瓶颈）；gateway 模式已证实有路由环路缺陷（隧道自身连接被策略路由吞 → 全网瘫，弃用并记为已知问题）；SOCKS5 隔离模式 client 已在路由器运行（tests/e2e/router-client.toml，listen 0.0.0.0:1080 + proxy_auth，Hello 通过）。

1. **G1 路由器 client 吞吐实测**：本机 `curl -x socks5h://router:***@<路由器IP>:1080` 拉 50.20:9090/testfile.bin ×3，同时采样路由器 CPU%（top）+ VmRSS（/proc/PID/status）。基线对比：Mac client 双跳 20.3 MB/s、路由器 server 单跳 56.7 MB/s。
2. **G2 最小资源验证**：记录吞吐/CPU%/RSS 三元组（含 idle 稳态），cipher 锁定 aes256-gcm（硬件加速 CPU 最省），给出量化结论；无需改代码，默认参数已最优。
3. **G3 WiFi 极限（DFS/160MHz）**：运行时切换 `wl -i eth7 down; chanspec 36/160; up`（不写 nvram，重启自回 149/80）。风险：WiFi 断 ~60-90s（含 CAC）、SSH master 需重建、雷达误判强跳频。切换后重测路由器 server 单跳吞吐（预期 56.7 → 80-100+ MB/s，本机 M3 Pro 支持 160MHz）；不稳则回退 149/80。
4. **G4 报告**：PERF_OPTIMIZATION_REPORT.md 补路由器 client 章节（SOCKS5 隔离模式 vs gateway 缺陷记录、cipher 推荐、资源占用、DFS 收益）。
5. **G5 全面清理**：路由器（kill phantom、删 /tmp 测试文件与日志、SSH master 退出、删测试公钥）；对端 Mac（~/phantom-lan/、server、http.server、caffeinate）；本机（client、tcp_relay.py、模拟器 fport/app）；仓库根目录误建文件（"10.0.2.2:8800…"、"SOCKS5"、"pid"、"模拟器服务端"、"隧道"）删除；router-client.toml 作为测试资产归档。

## Phase F：报告与归档

1. `tests/PERF_OPTIMIZATION_REPORT.md`：逐 Phase 收益归因表、各维度最终倍数（vs 历史最差基线）、物理天花板说明、剩余瓶颈。
2. 更新 `tests/E2E_CIPHER_REPORT.md` 推荐矩阵（硬件加速后 AES 预期反超 ChaCha，以实测为准）。
3. 资产归档：bench-suite.sh 全矩阵版、鸿蒙签名材料重建脚本（`client/harmony/signing/` 新材料已生成，hap-sign-tool 命令序列归档为脚本）、路由器回滚脚本。
4. 释放全部测试资源（容器/网络/镜像/tc 规则/模拟器/machine/临时文件），`tc qdisc show` 复核干净。

## 关键风险与对策

- **模拟器 VPN 授权**：系统弹窗可能被模拟器限制——对策：uitest dumpLayout 定位按钮；若模拟器禁 VPN，降级为「SOCKS5-only 模式」（rust 侧 fd<0 跳过 TUN，保留为 fallback 分支）并明确标注。
- **公网服务端 protect**：本期路由拆分法只保证模拟器/服务端在本地网段场景；公网 IP 服务端会回环——标注为已知限制，后续用 NAPI ThreadsafeFunction 回调 ArkTS `vpnConnection.protect(fd)` 解决。
- **双进程状态**：UI 与 VPN Ability 不同进程，rust 状态经 preferences 桥接轮询，允许 1s 级延迟。
- **FEC 带宽放大**：限速场景可能反向恶化——矩阵中含 tbf 对照验证。
- **64KB 帧内存放大**：1GB 内存目标设备按 64KB×连接数×2 评估，受限容器先验证并发上限。

## 执行顺序

A（鸿蒙数据面）→ B（传输栈）→ C（弱网）→ D（全矩阵）→ E（真机）→ G（路由器收尾）→ F（报告）。A-D 已完成，E 基本完成，当前执行 G。