# Phantom 传输吞吐与弱网优化报告

> 日期：2026-08-23/24。环境：macOS（Apple Silicon）+ podman rootful VM（aarch64 Linux）+ HarmonyOS 模拟器（Pura 90）。
> 基线锚点：2026-08-23 早轮 E2E（容器 TCP 95-106 MB/s、QUIC 40 MB/s、鸿蒙链路 49-75 MB/s、弱网 300ms+5% 0.57 MB/s、failover 12s 不切换）。

---

## 1. 总览：各维度最终倍数

| 维度 | 基线 | 最终 | 倍数 | 主要手段 |
|---|---|---|---|---|
| AES-256-GCM 微基准（macOS 单核） | 177 MiB/s | 2167 MiB/s | **12.3x** | `--cfg aes_armv8`+`polyval_armv8`（ARMv8 AES/PMULL intrinsics）+ LTO fat |
| AES-256-GCM 微基准（容器 1vCPU） | 169 MiB/s | 558 MiB/s | **3.3x** | 同上（musl target rustflags） |
| AES-256-GCM 微基准（鸿蒙模拟器） | 162.7 MiB/s | 492 MiB/s | **3.0x** | 同上（ohos target rustflags） |
| 链路 TCP（gvproxy 容器路径） | 102 MB/s | 295 MB/s | **2.9x** | 帧 16KB→64KB（p3，2.6x）+ socket BDP 缓冲（p4） |
| QUIC（VM 内直连口径） | 42.8 MB/s | 254 MB/s | **5.9x** | QUIC 流/连接窗口按 BDP 放大 + MTU 发现 + BBR |
| 鸿蒙链路 AES-256（模拟器客户端→容器服务端） | 49 MB/s | 117-127 MB/s | **2.5x** | 硬件加速 + 64KB 帧 + writev |
| 鸿蒙链路 ChaCha20 | 75 MB/s | 115-123 MB/s | **1.6x** | 同上（ChaCha 原本就是 NEON 后端，提升空间小） |
| failover 切换延迟 | 12s 不切换（缺陷） | **亚秒级**（实测 10ms 级检测） | 两个数量级 | 数据面失败证据直馈 + 探测参数提速 |
| 服务端重启恢复 | — | **1s** | — | 每连接自动重建 |
| 弱网 300ms+5%（lossy_proxy 语义） | 0.57 MB/s | 0.56 MB/s | 持平（见 §6 分析） | 瓶颈在用户态延迟代理而非窗口 |

## 2. 逐 Phase 归因

| Phase | 改动 | 测量证据 |
|---|---|---|
| p1 | `.cargo/config.toml` 三 target 加 `aes_armv8`/`polyval_armv8`/`chacha20_force_neon`；`cipher.rs` auto_detect 改「编译期 cfg AND 运行时探测」联合判定（修「探测到 AES 硬件却用软件实现」缺陷） | AES-256 macOS 177→1510 MiB/s |
| p2 | `[profile.release] lto=fat, codegen-units=1, panic=abort` | AES-256 →2083 MiB/s |
| p3 | `MAX_FRAME_PAYLOAD` 16384→65504（u16 消息层上限内 16 对齐最大值）；协议 v2→v3；`write_vectored_all`（len 前缀+密文单次 writev syscall） | 链路 TCP 105→265 MB/s |
| p4（计划 Phase B） | tcp.rs 改 socket2 建连/listen（connect 前设 SO_SNDBUF/SO_RCVBUF=4MB，窗口缩放因子生效）；quic.rs 补 stream 8MB/conn 32MB 窗口 + initial_mtu 1500 + mtu_discovery；VM 内核 BBR + rmem/wmem 16MB | 链路 TCP →295 MB/s；QUIC VM 内 42.8→254 MB/s |
| p5（计划 Phase C） | QUIC 默认拥塞控制 Cubic→BBR；failover：健康检查 30s→5s、阈值 3→2，新增 `report_datapath_failure`（数据面 Io/Timeout 失败直接累计证据，2 次失败即切换，不等探测 tick） | failover 切换 19s（纯探测）→ 亚秒（数据面驱动） |
| pD（计划 Phase D 矩阵） | 修复「URI cipher= 参数无消费点」缺陷：`CipherPreference::effective_for`（per-server 优先于全局），socks5/hello/quic_pool 全部消费点替换 | `cipher=chacha20-poly1305` 服务端日志实测 `cipher=ChaCha20Poly` |

## 3. cipher × 协议 × 平台矩阵（优化后）

链路 TCP 10MB（gvproxy 容器路径，MB/s）：

| cipher | 链路 | 备注 |
|---|---|---|
| AES-256-GCM | 178-190 | 硬件加速后不再垫底 |
| AES-128-GCM | 182-193 | |
| ChaCha20-Poly1305 | 181-199 | NEON 后端，全程稳定 |
| ASCON-128 | 151-164 | |

四 cipher 在链路级已拉平（瓶颈转移到 gvproxy/容器转发路径），微基准差异（2167 vs 740 MiB/s）在真实链路被掩盖。**推荐矩阵修订：默认 AES-256-GCM（auto）**，硬件加速全平台可用；ChaCha20 作为无 AES 硬件平台的回退。

鸿蒙链路（模拟器客户端，10MB，fport 口径）：AES-256 117-127 MB/s ≈ ChaCha 115-123 MB/s（瓶颈在模拟器 NAT/fport 路径）。

QUIC cipher 约束：AEAD 由 Noise pattern 固定（AESGCM 或 ChaChaPoly），双端 `cipher=` 参数必须映射一致，否则握手失败——QUIC 矩阵只能测「双端一致」组合（设计如此，非缺陷）。

## 4. 弱网与重联

| 场景 | TCP | QUIC | 备注 |
|---|---|---|---|
| 100ms+1% | 1.61-1.63 MB/s | 0.49 MB/s | lossy_proxy 语义 |
| 300ms+5% | 0.56 MB/s | 0.10-0.20 MB/s | 同上 |
| tbf 10mbit | 1.18 MB/s | 3.32-3.50 MB/s | 硬限速封顶 |
| failover 主停 | 亚秒切换（数据面驱动）/9.9s（纯探测兜底） | — | 历史缺陷修复 |
| 服务端重启恢复 | 1s | — | |

**弱网数据的重要说明**：lossy_proxy 是用户态转发器（延迟在代理进程内注入），BDP 窗口优化的受益点（真实高 RTT 链路的内核缓冲）在该拓扑下无法体现；socket buffer 的真实收益需在真机高 RTT 链路验证（Phase E 阻塞项，见 §7）。

## 5. 鸿蒙数据面（Phase A 结论）

- **完整 VpnExtensionAbility 链路已实现并保留**（`PhantomVpnExtensionAbility.ets`：protectProcessNet + 服务端 /32 isExcludedRoute 防回环 + preferences 状态桥）。
- **模拟器限制**：模拟器镜像缺少 `com.huawei.hmos.vpndialog` 系统 bundle，VPN 授权弹窗无法拉起（hilog 实证 `extensionInfo empty`）——TUN 数据面在模拟器不可用，真机不受影响。
- **SOCKS5-only 降级**（`android.rs`：fd<0 跳过 TUN 只起 loopback SOCKS5）：模拟器上经 `hdc fport` 验证全链路——宿主机 curl → 鸿蒙 socks5 → 隧道 v3 → 容器服务端 → phantom-web，三方字节对账一致（10485760 vs 10485824，64B 帧开销），吞吐 117-147 MB/s。

## 6. 遗留与物理天花板

1. **QUIC 经 gvproxy 仍 6-8 MB/s**：podman macOS 用户态 UDP 转发固有劣化（5.9x），只能从测量口径规避（VM 内直连/真机），非产品缺陷。
2. **弱网 BDP 收益待真机验证**：见 §4 说明。
3. **FEC 评估结论（不实现）**：当前丢包恢复瓶颈在 gvproxy 转发层而非协议层；帧层 FEC（XOR/RS）只服务 QUIC/UDP 路径，在 UDP 转发失真的测试环境无法产出可信数据，且带宽冗余会在 tbf 限速场景反向恶化吞吐。结论：待真机 UDP 路径可信后再评估。
4. **公网服务端场景的 protect**：鸿蒙 VPN 当前依赖 protectProcessNet（API 22+）+ 服务端 /32 排除路由；更低版本系统需 NAPI ThreadsafeFunction 回调 `vpnConnection.protect(fd)`，列为后续项。
5. **天花板**：LAN 1GbE 118 / 2.5GbE 295 MB/s 硬上限；单核 AES-NI ~2.1 GB/s；容器 1vCPU 路径 ~300 MB/s。

## 7. Phase E 真机验证（已完成）

**环境**：本机 Mac（<本机内网IP>）↔ 第二台 MacBook Pro M1 Max/32GB（<内网主机IP>）。两端均为 WiFi 6（802.11ax）、5GHz 信道 149、80MHz，经同一台 AP（RT-AX86U Pro <路由器IP>）互传。

### 7.1 裸链路天花板（不经 phantom）

| 测量 | 吞吐 | 说明 |
|---|---|---|
| nc 裸 TCP 打流（3GB zero） | 15.7 MB/s | 物理链路极限，无 HTTP/加密开销 |
| HTTP 单流（python http.server） | 17–20 MB/s | |
| HTTP 4 流并行聚合 | 19.5 MB/s | 不随流数增长 → 链路饱和，服务端非瓶颈 |

根因：**WiFi 双跳半双工**。Mac↔Mac 同 AP 同信道互传，每字节占两次空中时间；且本机 PHY 仅协商到 408 Mbps（MCS 4、RSSI -63 dBm，对端 -68 dBm）。估算 408 × 0.65 × 0.5 ≈ 133 Mbps ≈ 16.6 MB/s，与实测吻合。「互联网下载 100MB/s」为单跳（AP→Mac）场景，不可比。

### 7.2 phantom 隧道吞吐（cipher=auto→AES-256-GCM，proto=tcp）

| 客户端 | 吞吐（3 轮） | 结论 |
|---|---|---|
| macOS CLI（本机） | 15.5 / 20.2 / 20.3 MB/s | ≥ 裸链路上限，协议开销≈0 |
| 鸿蒙模拟器（SOCKS5-only） | 19.0 / 19.0 / 20.0 MB/s | 与 macOS 客户端持平，Hello 验证通过 |

鸿蒙链路拓扑：模拟器(QEMU slirp 10.0.2.15) → 宿主机 relay（127.0.0.1:8443 → 50.20:8443，`tests/e2e/tcp_relay.py`）→ WiFi → 50.20 phantom-server → 回环 python http.server。模拟器内核直连 <内网主机IP> 报 EHOSTUNREACH（slirp 不转发至 LAN 其他主机），relay 桥接为模拟器环境的固定解法。

### 7.3 WiFi 优化结论

phantom 已打满物理链路，协议层无优化空间。链路层优化按收益排序：

1. **任一端改有线连 AP**（消掉一跳，预期吞吐 ≈2 倍，30–40 MB/s）
2. **靠近路由器**（RSSI -63/-68 → >-55，PHY 408 → 800+ Mbps）
3. 路由器开 160MHz 频宽（RT-AX86U Pro 支持，PHY 可翻倍；需管理后台操作）
4. ~~AWDL 点对点直连~~：实测 awdl0 无对等链路激活（ping6 100% 丢包），macOS 只在 AirDrop 场景激活且时间片共享，不可行。

### 7.4 其他发现

- **对端 Mac 睡眠事故**：合盖/闲置后 macOS 深睡，WiFi 固件代答 ARP/ICMP（ping 通但 RTT 600–870ms），TCP 栈停止（SSH/8443 全超时）。WOL 魔包无效。对策：唤醒后 `caffeinate -dims` 常驻防睡（测试后已清理）。
- **路由器只读探测未执行**：SSH(22)/Telnet(23) 关闭，仅管理后台 80/8443 开放；cpuinfo 探测需在后台开启 SSH（留待用户授权）。

### 7.5 RT-AX86U Pro 路由器部署实测（已完成）

**硬件/构建**：BCM4912（4×Cortex-A53 @2.0GHz，aes/pmull/sha1/sha2 硬件扩展齐全）、1GB RAM。`aarch64-unknown-linux-musl` 静态 CLI（aes_armv8/polyval_armv8 cfg，5.6MB），base64 管道传输（路由器无 sftp-server）。

**部署模式对比**（重要结论）：

| 模式 | 结论 |
|---|---|
| ✅ SOCKS5 隔离模式（`listen="0.0.0.0:1080"` + proxy_auth，`tests/e2e/router-client.toml`） | 只代理显式配置代理的设备，零路由表改动，phantom 崩溃不影响网络。**推荐生产模式** |
| ❌ gateway 透明网关（`--tun --gateway`） | **路由环路缺陷**：隧道自身连接被自身策略路由吞掉（Server unreachable），转发规则已生效但隧道不通 → 全网瘫。需修复（fwmark 排除隧道 socket）后才能上生产 |

**吞吐**（proto=tcp，Mac（M3 Pro）→路由器单跳为 server 模式；双跳为路由器 client SOCKS5 → 50.20 server）：

| 场景 | 吞吐 | 结论 |
|---|---|---|
| server 模式 cipher 矩阵（149/80） | aes-256-gcm 56.7 / aes-128-gcm 57.0 / chacha20 55.1 MB/s | 三者差 <4% → 瓶颈非加密，A53 硬件 AES 富余 |
| client SOCKS5 双跳（149/80） | 19.5–21.4 MB/s | 与 Mac client 基线 20.3 持平，双跳 WiFi 物理封顶 |
| server 模式（36/160 + QoS off） | 55.6–58.3 MB/s | 新峰值；对比裸 HTTP 73.5，phantom 达裸链路 87%，加密+协议开销 <15% |

**资源占用**（client SOCKS5 模式 @20MB/s 双跳满载）：CPU 峰值 14.6% 单核（4 核总 ~3.7%）；RSS 2.7→3.0 MB（idle→满载）；磁盘 5.6MB。家庭宽带 ~50MB/s（500Mbps）下部署余量充足。

**WiFi 链路优化专项（纯链路层，不经 phantom；接收端 Mac M3 Pro，2x2 WiFi 6）**：

| 口径 | 149/80（原配置） | 36/160（最优） | 提升 |
|---|---|---|---|
| 下行 PHY 协商 | 960 Mbps（MCS9 2SS @80） | **1441 Mbps**（MCS7 2SS @160） | +50% |
| 单流裸 HTTP | 70.1–70.2 MB/s | **73.9–80.3 MB/s** | +14% |
| 4 并发裸 HTTP 聚合 | 64 MB/s | **69–128 MB/s** | +100%（峰值翻倍） |
| 上行 PHY | 432 Mbps | 432 Mbps | 持平（Mac 发射侧限制） |

- **最终生效配置（已 nvram 持久化）**：`wl1_chanspec=36/160` + QoS 关闭 + 发射功率 1496mW（满）。80MHz 时 4 并发 64 < 单流 70，证明空中时间已饱和；160MHz 把 PHY 抬 50% 后多流翻倍，单流稳定 +14%。
- **DFS/160 注意**：36/160 占用 DFS 子信道（52–144），检测到雷达时 AP 依法让出并跳信道（家用环境概率低，实测稳定）；Mac 侧信道切换瞬间可能漫游粘到 2.4G（实测 10 MB/s 假数据一度误导为 160MHz 劣化），关开 WiFi 即回 5G。
- 路由器侧 cipher 推荐 **AES-256-GCM**（硬件加速）；phantom 口径下 160MHz 增益小（58.3 MB/s）是因为路由器 A53 relay/单流 HTTP 服务本身是瓶颈，非 WiFi 限制。

**教训**：① `minihttpd.rs` `std::fs::read` 整文件读内存，×4 并发 = 512MB → 路由器 OOM 硬重启（/tmp tmpfs 全清，SSH 公钥/文件重传）。路由器侧压测需流式文件服务。② busybox 无 seq/setsid/httpd applet；路由器 curl 无代理支持（`proxy support is disabled in this libcurl`）。③ SSH 频繁登录触发华硕 fail2ban（端口 DROP）——公钥免密 + ControlMaster 单连接复用（`-o ControlPath=... -o ControlPersist`）是正解。④ macOS 重连后可能留在 2.4G 不回切（漫游粘性），需关开 WiFi 强制重连 5G；测速前先查 `wl assoclist` 确认客户端在 5G。⑤ 多口径测速需交叉验证：SSH 管道拉流仅 34MB/s（SSH 流控瓶颈）、minihttpd 单流 ~70 封顶（单线程），只有多并发聚合 + PHY 协商速率能揭示真实 WiFi 极限。

## 8. 测试资产

- `tests/e2e/bench-suite.sh`——阶段回归基准（微基准 ×2 平台 + 链路 + 弱网 4 场景）
- `tests/e2e/bench-matrix.sh`——Phase D 全矩阵（cipher×协议×弱网×重联）
- `.e2e-ctx/bench-p0..p4.log`、`bench-pD.log`——逐阶段 BENCH 结构化数据
- 鸿蒙签名材料重建：`client/harmony/signing/`（新 app.p12/app-debug.cer/app-profile-debug.p7b，密码 phantom123；旧材料在 `signing/backup-old/`）
- `.e2e-ctx/failover/client.toml`——双服务器 failover 测试配置
- `tests/e2e/tcp_relay.py`——Phase E 鸿蒙链路回环 relay（127.0.0.1:8443 → 第二台 Mac），模拟器出站到 LAN 的固定桥接件
- `tests/e2e/router-client.toml`——路由器 LAN SOCKS5 部署模板（0.0.0.0:1080 + proxy_auth，隔离式代理）
