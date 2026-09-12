# Phantom VPN — HarmonyOS 网络传输优化指南

> **目标平台**: HarmonyOS NEXT (API 12+ / 鸿蒙 6 以上)
> **应用类型**: VPN 隧道客户端 (VpnExtensionAbility + Rust NAPI 核心)
> **生成日期**: 2026-09-12

---

## 目录

1. [当前架构概览](#1-当前架构概览)
2. [网络并发流优化](#2-网络并发流优化)
3. [零拷贝优化](#3-零拷贝优化)
4. [SIMD / 单指令多数据处理](#4-simd--单指令多数据处理)
5. [芯片专用优化 API](#5-芯片专用优化-api)
6. [Socket / TCP 传输层调优](#6-socket--tcp-传输层调优)
7. [VPN 隧道层优化](#7-vpn-隧道层优化)
8. [内存与缓冲区优化](#8-内存与缓冲区优化)
9. [跨进程通信优化](#9-跨进程通信优化)
10. [线程模型与并发调度](#10-线程模型与并发调度)
11. [优化布局总览](#11-优化布局总览)
12. [优先级矩阵](#12-优先级矩阵)

---

## 1. 当前架构概览

```
┌─────────────────────────────────────────────────────────┐
│                    UI Process (ArkUI)                     │
│  pages/Index.ets ←→ VpnBridge.ets (文件 IPC)             │
│  TunnelProbe.ets (SOCKS5 探测)                           │
└──────────────────────┬──────────────────────────────────┘
                       │ vpnExtension.startVpnExtensionAbility
┌──────────────────────▼──────────────────────────────────┐
│              VPN Extension Process                        │
│  PhantomVpnExtensionAbility.ets                           │
│  ├── vpnExtension.createVpnConnection → TUN fd            │
│  ├── phantomHarmonyStart(fd, uri, mode) → Rust 核心       │
│  ├── connection API 网络变更监听                          │
│  └── 轮询 phantomHarmonyGetStats() → JSON string          │
└──────────────────────┬──────────────────────────────────┘
                       │ NAPI (fd as i32)
┌──────────────────────▼──────────────────────────────────┐
│              Rust Native Core (libphantom_harmony.so)     │
│  phantom_client::platform::android                        │
│  ├── TUN 读写循环 (epoll/mio)                             │
│  ├── TCP/UDP 流复用 + QUIC 连接池                          │
│  ├── 加密层 (AES-GCM / ChaCha20-Poly1305)                 │
│  └── DNS 隧道 + 路由白名单                                 │
└─────────────────────────────────────────────────────────┘
```

**当前瓶颈识别**:

| 瓶颈点 | 位置 | 影响 |
|--------|------|------|
| Stats 轮询 JSON 序列化 | `phantomHarmonyGetStats()` → string → JSON.parse | 每次轮询产生堆分配 + 字符串编解码 |
| 跨进程文件 IPC | `VpnBridge.ets` 沙箱文件读写 | 磁盘 I/O 延迟，无法实时推送 |
| TUN fd 跨语言传递 | NAPI `fd: number` → Rust `RawFd` | fd 本身零拷贝，但后续 read/write 仍走 syscall |
| 网络变更去抖 | `setTimeout` 300ms | 切换延迟可感知 |
| 加密在 Rust 用户态 | 无硬件加速调度 | 未利用芯片加密指令 |

---

## 2. 网络并发流优化

### 2.1 TaskPool 并行网络任务

> **API**: `@kit.ArkTS` → `taskpool` (API 11+)
> **场景**: 多目标延迟探测、并发握手、批量 DNS 解析

```typescript
import { taskpool } from '@kit.ArkTS';

// 将 TunnelProbe 的并发探测改为 TaskPool 并行
@Concurrent
function probeTarget(socksAddr: string, target: string): Promise<number> {
  // 在子线程中执行 SOCKS5 握手 + HTTP 测量
  return measureLatency(socksAddr, target);
}

// 并行探测多个目标，主线程零阻塞
async function parallelProbe(targets: string[]): Promise<Map<string, number>> {
  const tasks = targets.map(t => taskpool.execute(probeTarget, socksAddr, t));
  const results = await Promise.all(tasks);
  return new Map(targets.map((t, i) => [t, results[i]]));
}
```

**⚠️ 慎用注意**:
- TaskPool 任务上限受系统调度控制，建议并发 ≤ 8
- `@Concurrent` 函数内不可访问主线程 `@State` 变量
- 网络操作在 TaskPool 中执行是官方推荐做法（避免 UI 线程卡顿）

### 2.2 Sendable 跨线程零拷贝传递

> **API**: `@kit.ArkTS` → `collections.Sendable` (API 12+)
> **性能**: 100KB 数据传递效率约为序列化拷贝的 **20 倍**，1MB 约为 **100 倍**

```typescript
import { collections } from '@kit.ArkTS';

// 将探测结果用 Sendable 共享，避免跨线程拷贝
@Sendable
class ProbeResult {
  target: string = '';
  rttMs: number = 0;
  throughputKbps: number = 0;
}

@Concurrent
function probe(result: ProbeResult): ProbeResult {
  result.rttMs = measureRtt(result.target);
  return result; // 零拷贝返回
}
```

**⚠️ 慎用注意**:
- `@Sendable` 对象内部不能持有非 Sendable 引用
- 修改 Sendable 对象属性时需注意线程安全（无自动锁）

### 2.3 Worker 线程长连接池

> **API**: `@kit.ArkTS` → `worker.Worker` (API 7+)
> **场景**: VPN 数据面常驻后台，独立于 UI 生命周期

```typescript
import { worker } from '@kit.ArkTS';

// VPN 数据面 worker — 常驻后台处理隧道读写
const tunnelWorker = new worker.Worker('entry/ets/workers/TunnelWorker.ets');

// 主线程仅发控制指令，数据面在 worker 中运行
tunnelWorker.postMessage({ type: 'start', fd: tunFd, config: linkUri });
```

**⚠️ 慎用注意**:
- Worker 线程创建有开销（~5ms），适合长生命周期任务
- Worker 内不能使用 UI 组件相关 API
- 最多创建 7 个 Worker 实例（系统限制）

---

## 3. 零拷贝优化

### 3.1 NAPI External ArrayBuffer（引擎托管外部内存）

> **API**: Node-API `napi_create_external_arraybuffer`
> **场景**: Rust 核心处理完的 TUN 数据包直接暴露给 ArkTS，无需拷贝

**当前问题**: `phantomHarmonyGetStats()` 返回 JSON string，ArkTS 侧 `JSON.parse()` 产生二次拷贝。

**优化方案**: Rust 侧直接返回 External ArrayBuffer：

```rust
// rust/src/lib.rs — 零拷贝 stats 返回
#[napi]
pub fn phantom_harmony_get_stats_buffer(env: Env) -> Result<Uint8Array> {
    let stats = phantom_android::get_stats();
    // 直接将 stats 序列化到堆上，通过 External 暴露
    let bytes = serde_json::to_vec(&stats).unwrap();
    // napi_create_external_arraybuffer — 引擎托管，不拷贝
    Ok(Uint8Array::new(env, bytes)?)
}
```

```typescript
// ArkTS 侧 — 直接解码，无 JSON.parse 堆分配
const buf: Uint8Array = phantomLib.phantomHarmonyGetStatsBuffer();
const stats = parseStatsInPlace(buf); // 原地解析
```

**⚠️ 慎用注意**:
- External ArrayBuffer 的生命周期由引擎管理，Rust 侧不能释放底层内存
- 适合一次性消费的缓冲区，不适合长期持有

### 3.2 SharedMemory 跨进程零拷贝

> **API**: `@kit.ArkTS` → `ArkTSUtils.SharedMemory` (API 12+)
> **场景**: UI 进程 ↔ VPN Extension 进程的 stats 共享，替代当前文件 IPC

**当前问题**: `VpnBridge.ets` 通过沙箱文件 `phantom_vpn_status.txt` 做跨进程通信，每次轮询触发磁盘 I/O。

**优化方案**:

```typescript
import { ArkTSUtils } from '@kit.ArkTS';

// VPN Extension 进程 — 创建共享内存
const shm = ArkTSUtils.SharedMemory.create('phantom_stats', 4096);
const buf = shm.map(ArkTSUtils.SharedMemory.PROT_READ | ArkTSUtils.SharedMemory.PROT_WRITE);

// 写入 stats（直接内存写入，无序列化）
function writeStats(stats: VpnStats): void {
  const view = new DataView(buf);
  view.setUint32(0, stats.upBytes, true);
  view.setUint32(4, stats.downBytes, true);
  view.setUint32(8, stats.connections, true);
  // ... 原子写入
}

// UI 进程 — 读取共享内存
const shm = ArkTSUtils.SharedMemory.attach('phantom_stats');
const buf = shm.map(ArkTSUtils.SharedMemory.PROT_READ);
// 直接读取，无文件 I/O，无 JSON 解析
```

**⚠️ 慎用注意**:
- SharedMemory 需要两个进程协商名称和大小
- 需要手动处理内存屏障 / 原子操作保证一致性
- VPN Extension 进程的约束限制可能影响 SharedMemory 可用性，需验证

### 3.3 TUN fd 直接传递（当前已实现 ✓）

当前架构已通过 NAPI 将 TUN fd 直接传给 Rust 核心，fd 传递本身是零拷贝的。
后续 Rust 侧的 `read(fd)` / `write(fd)` 是必须的 syscall，无法避免。

**可优化方向**: 使用 `recvmmsg` / `sendmmsg` 批量系统调用减少 syscall 次数（见 §5.2）。

---

## 4. SIMD / 单指令多数据处理

### 4.1 Rust 自动向量化

> **场景**: 加密前的数据 XOR、checksum 计算、批量内存比较

Rust 核心已通过 `phantom_core` 处理加密。编译器在 `--release` 下会自动向量化简单循环。

**显式 SIMD 优化** (Rust nightly + `std::simd`):

```rust
// 批量 XOR（用于流密钥混合）— 一次处理 32 字节
use std::simd::*;

fn xor_blocks_avx2(dst: &mut [u8], src: &[u8], key: &[u8]) {
    let chunks = dst.chunks_exact_mut(32)
        .zip(src.chunks_exact(32))
        .zip(key.chunks_exact(32));
    for ((d_chunk, s_chunk), k_chunk) in chunks {
        let s = u8x32::from_slice(s_chunk);
        let k = u8x32::from_slice(k_chunk);
        let result = s ^ k;
        result.copy_to_slice(d_chunk);
    }
}
```

**⚠️ 慎用注意**:
- `std::simd` 仍在 nightly，生产环境建议用 `wide` crate 或 `packed_simd2`
- 需要确认目标设备 CPU 支持 NEON（ARM64 默认支持）
- 自动向量化已覆盖大部分场景，手动 SIMD 仅用于热点确认后

### 4.2 NEON 指令（ARM64 芯片专用）

HarmonyOS 设备均为 ARM64 架构，NEON SIMD 是标配。

**Rust 中使用 NEON 内联汇编**:

```rust
// CRC32 加速 — 利用 ARM64 CRC32 硬件指令
#[cfg(target_arch = "aarch64")]
#[inline(always)]
fn crc32_arm64(crc: u32, data: u32) -> u32 {
    let mut result: u32;
    unsafe {
        std::arch::asm!(
            "crc32cw {0:w}, {1:w}, {2:w}",
            out(reg) result,
            in(reg) crc,
            in(reg) data,
        );
    }
    result
}
```

**应用场景**:
- TUN 数据包 checksum 校验（CRC32 / Adler32）
- 加密 GCM 模式的 GHASH 运算
- 批量数据比较（路由匹配）

### 4.3 AES 硬件加速（ARM Crypto Extensions）

> **场景**: VPN 加密层是最主要的 CPU 消耗

ARM64 设备普遍支持 AES 硬件指令（`AESE`, `AESD`, `AESMC`, `AESIMC`）。

**Rust 侧确保启用硬件加速**:

```toml
# rust/Cargo.toml — 确保加密库使用硬件 AES
[dependencies]
aes-gcm = { version = "0.10", features = ["aes-armv8"] }
# 或使用 ring / RustCrypto 的 asm 后端
```

```rust
// 编译时确认 ARM64 AES 特性
#[cfg(all(target_arch = "aarch64", target_feature = "aes"))]
fn has_hw_aes() -> bool { true }

#[cfg(not(all(target_arch = "aarch64", target_feature = "aes")))]
fn has_hw_aes() -> bool { false }
```

**⚠️ 慎用注意**:
- 需在 `build.rs` 或 `.cargo/config.toml` 中设置 `target-feature=+aes,+neon`
- 部分低端设备可能不支持 ARMv8 Crypto Extensions，需运行时检测
- `ring` crate 自动检测并选择最优后端，推荐使用

---

## 5. 芯片专用优化 API

### 5.1 HarmonyOS HiAI / NPU 加速（慎用）

> **API**: `@kit.MindKit` / HiAI Foundation
> **场景**: 不直接适用于 VPN 数据面，但可用于 AI 路由决策

**不推荐用于数据面**: NPU 加速面向模型推理（矩阵运算），VPN 数据包处理是流式字节操作，NPU 无优势。

**可能用途**: 基于流量特征的智能路由选择（如用轻量模型预测最优出口节点）。

**⚠️ 慎用注意**:
- NPU 调度延迟（~ms 级）远大于包处理延迟（~μs 级）
- 引入 NPU 依赖增大包体积，与 VPN 轻量目标矛盾
- **结论: 不推荐当前阶段使用**

### 5.2 批量系统调用 — `recvmmsg` / `sendmmsg`

> **场景**: TUN fd 批量读写，减少 syscall 次数

**当前问题**: Rust 核心可能逐包 `read(tun_fd)` / `write(tun_fd)`，高吞吐时 syscall 开销显著。

**优化方案** (Rust 侧):

```rust
use libc::{recvmmsg, sendmmsg, mmsghdr};

// 批量从 TUN 读取最多 64 个数据包，一次 syscall
fn batch_read_tun(fd: i32, bufs: &mut [IoSliceMut]) -> usize {
    let mut msgs = [mmsghdr::default(); 64];
    // 配置 msgs[i].msg_hdr 指向 bufs[i] ...
    let n = unsafe { recvmmsg(fd, msgs.as_mut_ptr(), 64, 0, ptr::null_mut()) };
    n as usize
}
```

**性能**: 64 包批量读取可将 syscall 开销从 64 次降为 1 次，高 PPS 场景提升 3-5x。

**⚠️ 慎用注意**:
- `recvmmsg` 在 HarmonyOS 内核可用性需验证（Linux 3.0+，HarmonyOS 内核基于 Linux）
- 批量大小需根据 MTU 和内存预算调优，建议 32-64
- 增加单次延迟（等待填满批量），对延迟敏感场景需设置超时

### 5.3 `io_uring` 异步 I/O（内核 5.1+）

> **场景**: TUN fd + 网络 socket 的统一异步 I/O

```rust
use io_uring::{IoUring, SubmissionQueue, CompletionQueue};

// 用 io_uring 替代 epoll，减少 syscall 次数
let mut ring = IoUring::new(256)?;
// 批量提交 read/write 请求
ring.submission().push(&Read::new(tun_fd, buf))?;
ring.submit()?;
// 一次 io_uring_enter 完成所有 I/O
```

**⚠️ 慎用注意**:
- HarmonyOS 内核版本需 >= 5.1 才支持 io_uring
- `io-uring` crate 在交叉编译环境可能有兼容性问题
- 对比 `mio`(epoll) 的收益在高 QPS（>10K pps）时才显著
- **需验证 HarmonyOS NEXT 内核是否启用 io_uring 支持**

### 5.4 内存大页 (Huge Pages)

> **场景**: TUN 缓冲区分配，减少 TLB miss

```rust
// 使用 mmap 分配 2MB 大页
fn alloc_hugepage(size: usize) -> *mut u8 {
    unsafe {
        libc::mmap(
            ptr::null_mut(),
            size,
            libc::PROT_READ | libc::PROT_WRITE,
            libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_HUGETLB,
            -1, 0,
        ) as *mut u8
    }
}
```

**⚠️ 慎用注意**:
- 需内核配置 `CONFIG_HUGETLBFS=y`
- 大页资源有限，系统可能 OOM
- 对小缓冲区（< 2MB）无意义，仅用于大型环形缓冲区

---

## 6. Socket / TCP 传输层调优

### 6.1 Socket ExtraOptions 精细控制

> **API**: `@ohos.net.socket` -> `TCPSocket.setExtraOptions()` (API 7+)

```typescript
import { socket } from '@kit.NetworkKit';

const tcp = socket.constructTCPSocketInstance();

// 连接后立即调优 socket 参数
await tcp.setExtraOptions({
  keepAlive: true,           // 保持连接，避免 NAT 超时断连
  OOBInline: false,          // 不内联带外数据
  TCPNoDelay: true,          // 禁用 Nagle 算法，降低延迟（VPN 场景关键）
  socketLinger: { on: true, linger: 0 }, // 关闭时立即发送 RST，避免 TIME_WAIT
  receiveBufferSize: 262144, // 256KB 接收缓冲区
  sendBufferSize: 262144,    // 256KB 发送缓冲区
  reuseAddress: true,        // 允许端口复用
  socketTimeout: 0,          // 无超时（数据面自行管理）
});
```

**⚠️ 慎用注意**:
- `TCPNoDelay: true` 会增加小包数量，高吞吐场景需配合批量写入
- `receiveBufferSize` / `sendBufferSize` 系统可能 cap 到 `net.core.rmem_max`
- VPN 隧道场景下 `TCPNoDelay` 几乎必开，否则 Nagle 引入 40ms+ 延迟

### 6.2 UDP Socket 优化（DNS / QUIC）

```typescript
const udp = socket.constructUDPSocketInstance();

await udp.setExtraOptions({
  receiveBufferSize: 131072,  // 128KB
  sendBufferSize: 131072,
  reuseAddress: true,
});
```

### 6.3 TLSSocket 会话复用

> **API**: `@ohos.net.socket` -> `TLSSocket` (API 8+)

```typescript
import { socket } from '@kit.NetworkKit';

const tls = socket.constructTLSSocketInstance();

// TLS 配置 — 启用会话票证复用，减少握手开销
await tls.bind({ address: '0.0.0.0', port: 0, family: 1 });
await tls.connect({
  address: { address: serverHost, port: serverPort, family: 1 },
  options: {
    ALPNProtocols: ['h2', 'http/1.1'],  // ALPN 协商
    // TLS 1.3 默认支持会话票证，无需额外配置
  },
});
```

**⚠️ 慎用注意**:
- HarmonyOS TLSSocket 的会话缓存策略由系统管理，应用层无法直接控制
- TLS 1.3 一次 RTT 握手已很快，优化收益相对有限
- 当前 Phantom 使用 Rust 核心 TLS（rustls），此条仅适用于 ArkTS 侧的探测连接

---

## 7. VPN 隧道层优化

### 7.1 VpnExtensionAbility 配置优化

> **API**: `@ohos.net.vpnExtension` (API 11+)

```typescript
import { vpnExtension } from '@kit.NetworkKit';

// 创建 VPN 连接 — 优化 MTU 和路由
const vpnConnection = vpnExtension.createVpnConnection({
  mtu: 1400,  // MTU 1400 适配 PPPoE/隧道嵌套开销
  addresses: [{
    address: { address: '10.8.0.2', family: 1 },
    prefixLength: 24,
  }],
  routes: [{
    interface: '',
    destination: { address: '0.0.0.0', family: 1 },
    gateway: { address: '10.8.0.1', family: 1 },
    prefixLength: 0,
    hasGateway: true,
  }],
  dnsServers: ['10.8.0.1'],  // 隧道 DNS 防泄漏
  searchDomains: [],
});

await vpnConnection.protectAllFd(true); // 保护 VPN 自身连接不被路由回 TUN
```

**⚠️ 慎用注意**:
- `mtu` 过大导致分片，过小降低有效载荷率，1400 是安全默认值
- `protectAllFd` 确保隧道 socket 不被 TUN 捕获，避免路由环路
- VPN Extension 有模块引用限制（不支持部分模块），复杂逻辑应放 Rust 核心

### 7.2 MTU 动态探测 (Path MTU Discovery)

```rust
// 在 Rust 核心中实现 PMTUD，动态调整 TUN MTU
// 当收到 ICMP "fragmentation needed" 时降低 MTU

#[napi]
pub fn phantom_harmony_set_mtu(mtu: u16) -> Result<()> {
    phantom_android::set_tun_mtu(mtu);
    Ok(())
}
```

### 7.3 多路径并发 (Multipath TCP / QUIC)

> **场景**: Wi-Fi + 蜂窝同时传输，提升带宽和可靠性

Phantom Rust 核心已支持 QUIC（多路复用）。HarmonyOS 侧的优化：

```typescript
import { connection } from '@kit.NetworkKit';

// 监听多网络接口，通知 Rust 核心使用多路径
connection.createNetConnection().register((netHandle, capabilityInfo) => {
  const networks = getActiveNetworks();
  if (networks.length > 1) {
    phantomLib.phantomHarmonyOnMultiPath(networks.map(n => ({
      ifname: n.ifname,
      preferred: n.type === 'Wi-Fi',
    })));
  }
});
```

**⚠️ 慎用注意**:
- MPTCP 需内核和运营商支持，HarmonyOS 侧无法直接控制
- QUIC 多路径在应用层实现更可控，依赖 Rust 核心（如 quiche/quinn）
- 蜂窝 + Wi-Fi 并发会增加功耗和流量消耗

---

## 8. 内存与缓冲区优化

### 8.1 对象池复用 (ArkTS 侧)

> **场景**: 频繁创建的探测请求、stats 对象

```typescript
class ProbeRequestPool {
  private pool: ProbeRequest[] = [];
  private max: number = 32;

  acquire(): ProbeRequest {
    return this.pool.pop() ?? new ProbeRequest();
  }

  release(req: ProbeRequest): void {
    if (this.pool.length < this.max) {
      req.reset();
      this.pool.push(req);
    }
  }
}
```

### 8.2 Rust 侧缓冲区池化

```rust
// 使用 bumpalo 线性分配器，循环结束后一次性释放
use bumpalo::Bump;

fn process_tun_packets(arena: &Bump, fd: i32) {
    let buf = arena.alloc_slice_fill_default(1500);
    // 处理数据包...
    // arena 在循环外 reset，无逐包 free 开销
}
```

### 8.3 ArrayBuffer 预分配

```typescript
// 预分配固定大小 ArrayBuffer，避免动态扩容
const STATS_BUFFER = new ArrayBuffer(256);
const statsView = new DataView(STATS_BUFFER);

function updateStats(up: number, down: number): void {
  statsView.setBigUint64(0, BigInt(up), true);
  statsView.setBigUint64(8, BigInt(down), true);
}
```

---

## 9. 跨进程通信优化

### 9.1 当前方案：文件 IPC（可优化）

当前 `VpnBridge.ets` 使用沙箱文件做跨进程通信：
- `phantom_vpn_start.txt` — UI 写入，Extension 读取
- `phantom_vpn_status.txt` — Extension 写入，UI 轮询读取
- `phantom_vpn_stats.txt` — Extension 写入，UI 轮询读取

**问题**: 磁盘 I/O 延迟 + 字符串序列化开销

### 9.2 优化方案 A：SharedMemory（推荐）

见 §3.2，使用 `ArkTSUtils.SharedMemory` 实现零拷贝跨进程通信。

### 9.3 优化方案 B：Emitter 事件总线

> **API**: `@kit.BasicServicesKit` -> `emitter` (API 11+)

```typescript
import { emitter } from '@kit.BasicServicesKit';

// VPN Extension 进程 — 发送事件
emitter.emit({ eventId: 1001 }, {
  data: { status: 'connected', up: 12345, down: 67890 }
});

// UI 进程 — 监听事件（替代轮询）
emitter.on({ eventId: 1001 }, (eventData) => {
  this.vpnStatus = eventData.data.status;
  this.upBytes = eventData.data.up;
  this.downBytes = eventData.data.down;
});
```

**⚠️ 慎用注意**:
- Emitter 跨进程传递数据仍有序列化开销（非零拷贝）
- 但相比文件轮询，消除了磁盘 I/O 和轮询间隔延迟
- 适合状态变更通知（低频），不适合高频 stats 更新

### 9.4 优化方案 C：混合方案（最优）

```
┌──────────────┐                     ┌──────────────┐
│  UI Process  │                     │ VPN Extension │
│              │  Emitter (事件)     │              │
│  状态显示    │◄───── 状态变更 ─────│  状态推送    │
│              │                     │              │
│  流量统计    │◄─── SharedMemory ──│  统计写入    │
│  (定时读取)  │   (零拷贝共享)      │  (直接写入)  │
└──────────────┘                     └──────────────┘
```

- **低频状态变更** -> Emitter 事件推送（连接/断开/错误）
- **高频流量统计** -> SharedMemory 定时读取（零拷贝，无 I/O）

---

## 10. 线程模型与并发调度

### 10.1 当前线程模型

```
UI Thread (主线程)
  ├── ArkUI 渲染
  ├── VpnBridge 轮询 (setInterval)
  └── TunnelProbe 探测

VPN Extension Thread
  ├── VpnExtensionAbility 生命周期
  ├── connection API 监听
  └── NAPI 调用 → Rust 核心

Rust Core (独立线程池)
  ├── TUN 读写线程
  ├── TCP/UDP 流处理线程
  ├── 加密工作线程
  └── DNS 解析线程
```

### 10.2 优化建议

| 优化项 | 当前 | 建议 | 收益 |
|--------|------|------|------|
| Stats 轮询 | UI 线程 setInterval | SharedMemory + Emitter | 消除主线程定时器 |
| 网络探测 | UI 线程同步 | TaskPool 并行 | 主线程零阻塞 |
| 网络变更监听 | setTimeout 去抖 300ms | 立即响应 + Rust 侧去抖 | 减少 300ms 延迟 |
| Rust 核心 | 自管理线程池 | 保持不变 | 已最优 |

### 10.3 ArkTS 侧线程安全

```typescript
@Entry
@Component
struct Index {
  @StorageLink('vpnStatus') @Watch('onStatusChange') vpnStatus: string = 'disconnected';
  @StorageLink('upBytes') upBytes: number = 0;
  @StorageLink('downBytes') downBytes: number = 0;

  onStatusChange(): void {
    if (this.vpnStatus === 'connected') {
      this.startStatsPolling();
    }
  }
}
```

---

## 11. 优化布局总览

```
┌─────────────────────────────────────────────────────────────────┐
│                        优化分层架构                               │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌─ Layer 4: 芯片层 (ARM64) ──────────────────────────────┐    │
│  │  - AES 硬件指令 (AESE/AESD)                            │    │
│  │  - NEON SIMD (CRC32/GHASH/XOR)                         │    │
│  │  - 大页内存 (Huge Pages)                                │    │
│  └────────────────────────────────────────────────────────┘    │
│                          ↑                                      │
│  ┌─ Layer 3: 内核层 (HarmonyOS Kernel) ───────────────────┐    │
│  │  - recvmmsg/sendmmsg 批量 syscall                       │    │
│  │  - io_uring 异步 I/O (需验证可用性)                     │    │
│  │  - TUN fd 零拷贝传递 (已实现 ✓)                         │    │
│  └────────────────────────────────────────────────────────┘    │
│                          ↑                                      │
│  ┌─ Layer 2: NAPI / Rust 核心层 ──────────────────────────┐    │
│  │  - External ArrayBuffer 零拷贝                          │    │
│  │  - Arena 分配器 (bumpalo)                               │    │
│  │  - rustls + ring 硬件加密后端                           │    │
│  │  - mio/epoll → io_uring (可选)                          │    │
│  └────────────────────────────────────────────────────────┘    │
│                          ↑                                      │
│  ┌─ Layer 1: ArkTS 并发层 ────────────────────────────────┐    │
│  │  - TaskPool 并行探测                                    │    │
│  │  - Sendable 零拷贝跨线程                                │    │
│  │  - Worker 常驻数据面                                    │    │
│  │  - Socket ExtraOptions 调优                             │    │
│  └────────────────────────────────────────────────────────┘    │
│                          ↑                                      │
│  ┌─ Layer 0: 跨进程通信层 ────────────────────────────────┐    │
│  │  - SharedMemory (高频 stats)                            │    │
│  │  - Emitter 事件 (低频状态)                              │    │
│  │  - 替代文件 IPC                                         │    │
│  └────────────────────────────────────────────────────────┘    │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

---

## 12. 优先级矩阵

### 按收益/成本排序

| 优先级 | 优化项 | 层级 | 预期收益 | 实现成本 | 风险 |
|:------:|--------|------|----------|----------|------|
| **P0** | TCPNoDelay=true | L1 Socket | 延迟降低 40ms+ | 极低 | 无 |
| **P0** | AES 硬件加速确认 | L4 芯片 | 加密吞吐 5-10x | 低 | 需验证编译标志 |
| **P0** | Socket 缓冲区调优 | L1 Socket | 减少丢包 | 极低 | 无 |
| **P1** | Emitter 替代文件轮询 | L0 IPC | 消除磁盘 I/O | 低 | 无 |
| **P1** | TaskPool 并行探测 | L1 并发 | 主线程零阻塞 | 低 | 无 |
| **P1** | 对象池复用 | L2 内存 | 减少 GC | 低 | 无 |
| **P2** | SharedMemory 跨进程 | L0 IPC | 零拷贝 stats | 中 | 需验证 Extension 约束 |
| **P2** | External ArrayBuffer | L2 NAPI | 消除 JSON 序列化 | 中 | 生命周期管理 |
| **P2** | NEON CRC32/GHASH | L4 SIMD | 包处理加速 | 中 | 需汇编验证 |
| **P2** | 网络变更立即响应 | L1 并发 | 减少 300ms 延迟 | 低 | 需测试抖动 |
| **P3** | recvmmsg 批量读取 | L3 内核 | syscall 减少 3-5x | 高 | 需验证内核支持 |
| **P3** | io_uring 异步 I/O | L3 内核 | 高 QPS 提升 | 高 | 需验证内核支持 |
| **P3** | Sendable 跨线程 | L1 并发 | 20-100x 传递加速 | 中 | 线程安全 |
| **P4** | NPU/HiAI 路由 | L4 芯片 | 智能路由 | 高 | 不推荐 |
| **P4** | 大页内存 | L3 内核 | 减少 TLB miss | 高 | 需内核配置 |
| **P4** | MTU 动态探测 | L2 隧道 | 避免分片 | 中 | 实现复杂 |

### 建议实施路线

```
Phase 1 (立即): P0 项 — Socket 调优 + AES 硬件确认
Phase 2 (短期): P1 项 — Emitter + TaskPool + 对象池
Phase 3 (中期): P2 项 — SharedMemory + NEON + ArrayBuffer
Phase 4 (长期): P3 项 — 批量 syscall + io_uring (需内核验证)
```

---

## 附录 A: HarmonyOS API 版本速查

| API | 模块 | 最低版本 | 用途 |
|-----|------|----------|------|
| `taskpool` | @kit.ArkTS | API 11 | 并行任务调度 |
| `@Concurrent` | @kit.ArkTS | API 11 | 标记并发函数 |
| `@Sendable` | @kit.ArkTS | API 12 | 跨线程零拷贝 |
| `collections.Sendable` | @kit.ArkTS | API 12 | Sendable 集合 |
| `ArkTSUtils.SharedMemory` | @kit.ArkTS | API 12 | 跨进程共享内存 |
| `worker.Worker` | @kit.ArkTS | API 7 | 长驻后台线程 |
| `emitter` | @kit.BasicServicesKit | API 11 | 跨进程事件 |
| `vpnExtension` | @kit.NetworkKit | API 11 | VPN 增强管理 |
| `socket.setExtraOptions` | @kit.NetworkKit | API 7 | Socket 调优 |
| `TLSSocket` | @kit.NetworkKit | API 8 | TLS 连接 |
| `connection` | @kit.NetworkKit | API 7 | 网络连接管理 |

## 附录 B: 慎用清单汇总

| 优化项 | 慎用原因 | 建议 |
|--------|----------|------|
| NPU/HiAI | 调度延迟远大于包处理延迟 | 不用于数据面 |
| io_uring | 需内核 5.1+，HarmonyOS 可用性未知 | 先验证再使用 |
| 大页内存 | 需内核配置，资源有限 | 仅大型缓冲区 |
| MAP_HUGETLB | 同上 | 谨慎使用 |
| Sendable 修改 | 无自动锁，需手动线程安全 | 只读传递优先 |
| External ArrayBuffer | 生命周期由引擎管理 | 一次性消费场景 |
| recvmmsg 批量 | 增加单次延迟 | 设超时 + 延迟敏感场景慎用 |
| MPTCP | 需内核+运营商支持 | 用 QUIC 多路径替代 |
| Worker 数量 | 系统限制 7 个 | 长生命周期复用 |

---

> **文档结束** — 以上优化项基于 Phantom VPN 当前架构分析生成。
> 实施前建议逐项验证 HarmonyOS 目标版本的 API 可用性。

---

# 13. 复核与实施结果（2026-09-12）

本节是对上面 12 节建议的**逐条复核**：能落地的已经落地，做不到的给出依据（含 SDK/依赖源码位置），
避免下一次再做无收益的改动。

复核环境：真机 HOP-AL00（HarmonyOS 7.0.0.105 / API 26）、香港 VPS（1 vCPU / 1 GB，上行 3 Mbps，RTT ≈ 40 ms）、
对比机 Apple Silicon（用于展示硬件 AES 与软件实现的量级差）。

## 13.1 已实施

| 项 | 落地位置 | 改动 | 证据 |
|---|---|---|---|
| §4.3 / §12 P0 **硬件 AES**（真问题，已修） | `.cargo/config.toml` 的 `[target.aarch64-unknown-linux-ohos]`、`core/src/crypto/cipher.rs` | 在 ohos 目标上加 `-C target-feature=+aes`；新增 `hardware_aes_available()`，`auto_detect()` 与启动日志都改用它 | 见 §13.3 |
| §12 P0 **Socket 调优（Nagle）** | 新增 `client/src/net_tune.rs`，在 `tunnel.rs`（本地 SOCKS5 入口）、`tun.rs`（隧道中继 + 直连回退）、`socks5.rs` / `http_proxy.rs`（直连上游）共 5 处调用 | 关闭数据面所有 hop 的 Nagle；`net_tune` 单测断言连接两端 `nodelay() == true`，并断言未调优的 loopback 连接默认为 false | `cargo test -p phantom-client --lib`（57 passed，含 2 个新用例） |
| §6.1 / §6.2 **探测 socket 调优** | `client/harmony/entry/src/main/ets/common/TunnelProbe.ets` | 探测连接 `setExtraOptions({TCPNoDelay: true, keepAlive: true, receive/sendBufferSize: 256 KiB})`；探测结果不再混入本地 Nagle/小缓冲的误差 | SDK `@ohos.net.socket.d.ts` 的 `TCPExtraOptions extends ExtraOptionsBase` 字段核对通过；ArkTS 编译 0 error |
| §9.1 / §12 P1 **跨进程桥 I/O**（用等效方案替代 Emitter，见 §13.2） | `client/harmony/.../common/VpnBridge.ets`、`pages/Index.ets` | 日志由「每秒整文件读+重写」改为**追加写 + 超 512 KiB 才压缩**；UI 读取改为 **stat 尺寸不变即短路、变了只读尾部 20 KiB**；统计快照内容不变则不写；UI 对「快照停止刷新」改为速率归零 | 每 tick 的磁盘 I/O 从「~2000 行读 + 2000 行写」降到「新行字节数」；空闲隧道不再每秒写文件 |
| **§13.4 每流握手开销**（本指南之外的最大收益） | 新增 `client/src/tcp_pool.rs`；`socks5.rs` 的 `open_tcp_tunnel`、`http_proxy.rs`、`tunnel.rs`、`platform/{android,macos}.rs` | TCP 隧道改为**预握手会话池**：新流复用已认证会话，省掉 connect + Noise 握手（≈2 RTT）；池有 TTL、网络 epoch、单飞补池与失败回退，杜绝半开连接 | `cargo test -p phantom-e2e --test tcp_session_pool`（2 passed，含「空闲期间会话已死」回退）；真机日志 `pooled session, idle 132 ms` |

## 13.2 已否决（按当前 SDK / 内核 / 架构不成立）

| 建议 | 否决依据 |
|---|---|
| §3.2 SharedMemory 跨进程共享内存 | **ArkTS 里没有这个 API**：本机 SDK 全量 `*.d.ts` 中 `SharedMemory` 无任何命中（`ArkTSUtils.SharedMemory` 不存在）。NDK 侧有 `shared_memory.h`，但 UI 进程与 VpnExtension 进程都是 ArkTS，接入成本远大于收益 |
| §9.3 Emitter 事件总线替代文件轮询 | **emitter 是进程内事件总线**：SDK 文档原文为 "sending and processing events between threads **in a process**"。UI 与 VpnExtension 是两个进程，跨进程推送不成立；已改为降低文件桥本身的成本（§13.1 第 4 行） |
| §3.1 NAPI External ArrayBuffer 替代 stats JSON | stats 仅 1 Hz、约 120 字节，`JSON.parse`/字符串拼接的开销远小于一次文件写入；External ArrayBuffer 还要处理引擎托管内存的生命周期。已改为「内容不变不写」 |
| §5.2 `recvmmsg` / `sendmmsg` 批量读 TUN | **TUN 是字符设备不是 socket**，`recvmmsg` 对它返回 `ENOTSOCK`；Linux TUN 每次 `read` 只能取一个包。真正的批量读需要多队列 TUN，`VpnExtensionAbility` 未暴露 |
| §5.3 `io_uring` | HarmonyOS 内核是否启用未验证，且收益只在 >10k pps 显现；当前链路 3 Mbps ≈ 250 pps，tokio 的 epoll 已足够 |
| §5.4 大页内存 | 需要内核 `CONFIG_HUGETLBFS` 与保留大页；我们的缓冲区是 1.5 KiB / 16 KiB 量级，`MAP_HUGETLB` 无意义 |
| §5.1 NPU/HiAI、§7.3 MPTCP | 前者调度延迟远大于包处理延迟；后者需要内核与运营商支持，HarmonyOS 侧无法施加 |
| §2.1 TaskPool / §2.2 Sendable / §2.3 Worker | 探测与状态轮询都是**异步 I/O**，没有占用 UI 线程的 CPU 热点；真正让 UI 卡的是每秒整文件读日志，已按 §13.1 修掉。继续拆 Worker 只增加跨线程拷贝 |
| §7.1 MTU 改 1400 | **不需要**：`MAX_FRAME_PAYLOAD = 65504`（`core/src/constants.rs`），1500 字节的数据报/段不会被隧道分片；客户端在本地终结 TCP，降 MTU 只会把同样的字节拆成更多包。保持 1500 |
| §6.3 TLSSocket 会话复用 | 隧道是 Rust 侧 rustls/Noise，ArkTS 的 `TLSSocket` 只影响探测连接；探测每次新建连接，会话复用无从谈 |

## 13.3 硬件 AES：一个真实的功能性缺陷

`.cargo/config.toml` 早已为 ohos 打开 `--cfg=aes_armv8`（“把 ARMv8 指令后端编进来”），`.so` 里也确有
AES 指令（700+ 处 `aese/aesmc`）。但它**在真机上一行都没执行**：

* `aes` / `polyval` 的后端选择走 `cpufeatures::new!(..., "aes")`；
* `cpufeatures` 只实现了 `linux` / `android` / Apple 的运行时探测，
  **其他目标（含 `target_os = "ohos"`）的 `__detect_target_features!` 直接展开成 `false`**
  （源码：`cpufeatures-0.2.17/src/aarch64.rs` 末段）；
* 于是 `CipherSuite::auto_detect()` 在手机上返回 ChaCha20-Poly1305，AES 走的是 ~30 倍慢的 fixslice 软件实现;
* `cpufeatures` 的文档化行为是：**只要编译期打开了对应 `target-feature`，`get()` 恒为 `true` 且跳过运行时探测**
  （源码：`cpufeatures-0.2.17/src/lib.rs` 的 `new!` 宏注释与 `__unless_target_features!`）。

修法是加 `-C target-feature=+aes`，让「后端已编入」与「CPU 可用」两件事都成立。风险与依据：HarmonyOS NEXT
设备均为 ARMv8-A，AES/PMULL 是这一代 SoC 的标配（Kirin 全系具备），与既有注释里的假设一致——只是原来的
写法没能真正生效。

参考量级（Apple Silicon，`cargo run -p phantom-core --example cipher_bench --release -- 65536`）：

| cipher | Enc MiB/s | Dec MiB/s |
|---|---|---|
| AES-256-GCM（硬件后端） | 1576.8 | 1529.2 |
| AES-128-GCM（硬件后端） | 1730.3 | 1692.7 |
| ChaCha20-Poly1305（NEON） | 591.0 | 572.2 |
| ASCON-128（软件） | 436.3 | 454.0 |

同一台机器上把 `--cfg=aes_armv8` 去掉，AES-256-GCM 会掉到 ~177 MiB/s 量级（见 `.cargo/config.toml` 注释）。

真机确认方式（每次连接都会写进 App 日志）：

```
[INFO] cipher auto = AES-256-GCM (hardware AES: yes)
```

若显示 `ChaCha20-Poly1305 (hardware AES: no)`，说明该构建没带上 `+aes` 或 SoC 确实没有 AES 单元——两种
情况的处置完全不同，这行日志就是为了把它们区分开。

## 13.4 已实施：TCP 预握手会话池（真正的瓶颈：每流一次握手）

复核发现的最大的可获得收益不在本指南任何一条里：

`client/src/socks5.rs` 的 TCP 分支过去对**每一个**请求都执行
`TcpTransport::new() → connect(server) → NoiseInitiator::handshake()`，即一条新连接要付
**TCP 握手 1 RTT + Noise 握手 1 RTT ≈ 80 ms**（HK 节点 40 ms RTT，实测 ping 37–40 ms），之后才开始连目标站。
浏览器打开一个页面往往新建 5–15 条连接，这会直接体现为 TTFB 变长。（QUIC 分支本来就用连接池复用同一条
已认证连接，所以 `proto=quic` 没有这笔开销。）

已按「客户端 TCP 会话池」实现，见 `client/src/tcp_pool.rs`：

| 设计点 | 做法 |
|---|---|
| 复用 | 后台预握手若干条会话（`TARGET_IDLE = 2`，上限 4），新流直接取用，只付隧道内的 SYN/ACK |
| 预热 | 连接建立后立刻预热一条，第一个页面就享受到；每次取用后再补一条（150 ms 后） |
| 半开连接 | `take` 即**移除**（不可能重复使用）；超过 `MAX_IDLE_AGE = 8 s` 直接丢弃；会话带**网络 epoch**，换网后旧会话全部作废 |
| 会话死掉 | 池上 SYN 失败 → 落回现建连接重试一次，用户连接**不会**因此失败（只有多付一次本该省下的握手） |
| 换网/切服务器 | `clear()` 清空并静默 1.5 s，避免在新网络上猛打握手；池按「服务器地址 + cipher」分键，切服务器天然不复用 |
| 观测 | `PoolStats { created, reused }`；命中时日志为 `Tunnel established → … (pooled session, idle N ms)` |

验证：

- `cargo test -p phantom-e2e --test tcp_session_pool`（2 passed）：
  - `second_flow_reuses_a_pooled_session`：第二条流由池中会话服务（`reused == 1`）且数据完整；
  - `dead_pooled_session_falls_back_to_a_fresh_connection`：用可切断连接的 TCP shim 模拟「空闲期间会话已死」，
    流仍必须成功（覆盖半开场景）。
- 真机（HOP-AL00，20:49）：`INFO Tunnel established → 142.251.156.119:443 (cipher=Auto, pooled session, idle 132 ms)`
  ——该 Google 连接未再付 connect + Noise 握手。
- 回归：`cargo test -p phantom-e2e` 全绿（含 HTTP CONNECT、UDP associate、QUIC 复用、规则引擎等价性）。

顺带修掉一个**既有测试失真**：`tests/tests/quic_mux.rs` 依赖「目标 IP 一定走隧道」，而白名单化之后
智能模式默认直连，测试的 echo 目标（127.0.0.1）于是直接连，8 条流全直连、QUIC 握手计数为 0 ——
断言虽然偶发通过 payload 检查，却早已不再验证它声称的性质。现给该测试配置 `final_action = proxy`，
恢复「N 条 SOCKS5 隧道复用一条 QUIC 连接」的真实断言。

仍未做的备选（需要你决定，因为涉及服务端）：**A/B 到 `proto=quic`** —— 客户端零改动，服务端以
QUIC（同一端口 UDP/443）监听并更新连接串，顺带消除 TCP-over-TCP 的队头阻塞；前提是云厂商/链路放行 UDP 443。

对比之下，本指南里的零拷贝 / SIMD / 批量系统调用在 3 Mbps 链路上都不会改变体感：按 375 KB/s 计，
即使最慢的软件 AES（177 MiB/s）也还有约 480 倍余量。

## 13.5 怎么量性能（本仓库可用手段）

1. **静态**：`.so` 里是否真有硬件指令 ——
   `llvm-objdump -d --no-show-raw-insn libphantom_harmony.so | grep -c 'aese\|aesmc'`（当前构建 703）。
2. **启动日志**：`cipher auto = … (hardware AES: …)`，见 §13.3。
3. **桌面微基准**：`cargo run -p phantom-core --example cipher_bench --release -- 1024 16384 65536`；
   交叉编译到真机：`cargo build -p phantom-core --example cipher_bench --release --target aarch64-unknown-linux-ohos`
   再 `hdc file send`（本机实测该设备 shell **拒绝执行未签名二进制**：`/data/local/tmp/...: Permission denied`，
   真机侧请改用 App 内指标）。
4. **App 内**：连接详情里的延迟探测（含本地入口 + 隧道握手 + 服务端 connect 的全程耗时）与速率测试
   （固定窗口采样，走真实隧道）。
5. **端到端**：`scripts/speedtest.sh` + 服务端 `/var/log/phantom.log` 对账；当前基线上限是 VPS 上行 3 Mbps ≈ 375 KB/s。

## 13.6 改动落在哪一层

本仓库有三个客户端（macOS / HarmonyOS / 华硕路由器插件）共用 `phantom-core` 与 `phantom-client`，所以改动的
归属必须严格区分「通用」与「平台专用」：

| 层 | 目录 | 本次改动 |
|---|---|---|
| 通用能力 | `core/`（协议、加密、传输） | `hardware_aes_available()` 与 `auto_detect()` 的后端判定 |
| 通用客户端数据面 | `client/src/`（被 mac / 路由器 / 鸿蒙 / CLI 共用） | `net_tune.rs` 与 5 处 Nagle 调用；`tcp_pool.rs` 会话池（三个客户端共用同一份实现与同一套半开保护） |
| HarmonyOS 专用 | `client/harmony/` | NAPI 启动日志（cipher 判定）、ArkTS 日志/统计桥与探测 socket 调优、UI 侧速率归零 |
| 构建配置 | 仓库根 `.cargo/config.toml` | 仅 `[target.aarch64-unknown-linux-ohos]` 一节（该 target 只有鸿蒙在用；路由器是 `…-linux-musl`、mac 是 `…-apple-darwin`）。cargo 只从工作区根读取配置，放这里才能同时覆盖 DevEco/脚本与根目录发起的构建 |

反向约束同样成立：`net_tune` 这类「所有客户端都受益」的修正不要写进 `client/harmony`；而 cipher 判定日志这种
只有鸿蒙才需要的诊断，也不要写进 `client/src/platform/`（否则 Android 客户端会继承一份与它无关的理由）。
