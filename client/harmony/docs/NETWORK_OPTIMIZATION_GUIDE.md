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
