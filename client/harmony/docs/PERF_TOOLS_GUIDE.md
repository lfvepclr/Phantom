# Phantom VPN — HarmonyOS 性能检测工具指南

> **目标**: 为 AI 提供可程序化调用的鸿蒙性能检测工具清单
> **平台**: HarmonyOS NEXT (API 12+ / 鸿蒙 6 以上)
> **生成日期**: 2026-09-12
> **用途**: AI 读取本文档 → 选择工具 → 执行检测 → 解析结果 → 输出优化建议

---

## 6. SmartPerf Host — 渲染与功耗分析

> **AI 调用方式**: 命令行采集数据 + 解析 JSON 输出
> **定位**: UI 渲染帧率、功耗、温度一站式检测

### 6.1 核心能力

| 指标 | 采集方式 | AI 检测场景 |
|------|---------|------------|
| **FPS** | `hdc shell hidumper -s RenderService -a "fps"` | UI 渲染流畅度 |
| **GPU 负载** | `hdc shell hidumper -s GpuService -a "info"` | GPU 是否成为瓶颈 |
| **功耗** | `hdc shell hidumper -s PowerManagerService -a "current"` | VPN 持续运行功耗 |
| **温度** | `hdc shell hidumper -s ThermalService -a "temp"` | 过热降频检测 |
| **掉帧** | `hdc shell hidumper -s RenderService -a "jank"` | 卡顿根因分析 |

### 6.2 AI 调用示例

```bash
# 1. 采集 FPS
hdc shell hidumper -s RenderService -a "fps co.phantom.harmony"

# 2. 采集当前功耗 (mA)
hdc shell hidumper -s PowerManagerService -a "current"

# 3. 采集设备温度
hdc shell hidumper -s ThermalService -a "temp"

# 4. 一键脚本
cat << 'EOF' > perf_collect.sh
echo "=== FPS ==="
hidumper -s RenderService -a "fps co.phantom.harmony"
echo "=== POWER ==="
hidumper -s PowerManagerService -a "current"
echo "=== TEMP ==="
hidumper -s ThermalService -a "temp"
EOF
hdc shell "sh /data/local/tmp/perf_collect.sh"
```

### 6.3 AI 结果解析规则

```json
{
  "tool": "smartperf",
  "fps": 58.3,
  "jank_count": 2,
  "jank_duration_ms": 45,
  "power_mA": 320,
  "temp_celsius": 38.5,
  "verdict": {
    "fps": {"status": "pass", "threshold": 55},
    "power": {"status": "warning", "threshold": 300, "current": 320},
    "temp": {"status": "pass", "threshold": 42}
  }
}
```

> **AI 判定阈值**:
> - FPS < 30 → 🔴 严重卡顿
> - FPS 30~55 → ⚠️ 轻度掉帧
> - 功耗 > 400mA → ⚠️ 功耗过高
> - 温度 > 42°C → 🔴 过热风险

---

## 7. hdc 命令 — Trace 采集

> **AI 调用方式**: `hdc shell hitrace <options>`
> **定位**: 通用 Trace 采集层，配合 Profiler / HiPerf 使用

### 7.1 核心命令

| 命令 | 功能 | AI 场景 |
|------|------|---------|
| `hitrace --trace_begin` | 开始 Trace 采集 | 启动自动采集 |
| `hitrace --trace_duration N` | 采集 N 秒 | 定时采样 |
| `hitrace --trace_finish` | 结束并保存 | 结束采集 |
| `hitrace -b SIZE` | 设置缓冲区大小(KB) | 大流量场景调大 |
| `hitrace -o FILE` | 输出文件路径 | 指定导出路径 |

### 7.2 AI 采集示例

```bash
# 1. 采集 VPN 相关 trace 10 秒
hdc shell "hitrace --trace_begin -b 8192 \
  -o /data/local/tmp/vpn_trace.ftrace \
  --trace_duration 10 ability app disk network"

# 2. 拉取到本地
hdc file recv /data/local/tmp/vpn_trace.ftrace ./

# 3. 抓取 syscore（CPU 调度长时统计）
hdc shell "hitrace --trace_begin -b 4096 \
  -o /data/local/tmp/syscore_trace.ftrace \
  --trace_duration 60 ability"

# 4. 仅采集标签（减少数据量）
hdc shell "hitrace --trace_begin -b 4096 \
  -t vpn_read_tun vpn_write_socket \
  --trace_duration 5 -o /data/local/tmp/vpn_tags.ftrace"
```

### 7.3 AI 解析规则

```
# ftrace 行格式: task-pid [cpu] timestamp: tag: event
# AI 提取关键事件

<idle>-0     [000] 12345.678: vpn_read_tun: len=1400
<idle>-0     [000] 12345.679: vpn_encrypt: len=1400
phantom_vpn  [001] 12345.680: vpn_write_socket: len=1400

# AI 统计:
# - 各标签出现频率
# - 标签间隔时间（可反推每步耗时）
# - 跨 CPU 迁移次数（影响缓存性能）
```

---

---

## 5. DevEco Profiler — GUI 深度分析

> **AI 调用方式**: CLI 触发 trace 导出 + 解析导出文件
> **定位**: 全维度图形化分析工具，支持 AI 辅助诊断

### 5.1 核心能力

| 能力 | 检测维度 | AI 适用性 |
|------|---------|----------|
| **CPU 分析** | 函数耗时、调用链 | 解析导出的 trace 文件 |
| **内存分析** | 堆快照、对象分配、泄漏检测 | 检测 JS Heap 异常增长 |
| **网络分析** | 请求耗时、数据量统计 | VPN 隧道流量分析 |
| **启动分析** | 冷/热启动各阶段耗时 | VPN 扩展启动优化 |
| **AI 分析助手** | 自动识别性能瓶颈 | ✅ 直接输出优化建议 |

### 5.2 AI 调用方式

```bash
# 通过命令行导出 trace 供 AI 分析
hdc shell "hitrace --trace_begin --trace_duration 10 \
  -o /data/local/tmp/vpn_trace.ftrace -b 4096 \
  ability graphic app disk network"
hdc file recv /data/local/tmp/vpn_trace.ftrace ./vpn_trace.ftrace

# AI 可解析 ftrace 格式，提取关键事件
```

### 5.3 AI 解析维度

```
# AI 从 Profiler trace 提取的关键指标
CPU:
  - VPN 扩展进程 CPU 峰值: XX%
  - 主线程阻塞时长: YYms (若 > 16ms → 卡顿风险)
  - GC 暂停总时长: ZZms

Memory:
  - Native Heap 峰值: XX MB
  - ArkTS Heap 峰值: YY MB
  - 对象泄漏嫌疑: 若 Heap 持续增长且未回收

Network:
  - 数据包收发速率: XX KB/s
  - 连接数: YY (若持续增加 → 连接泄漏)
```

### 5.4 DevEco Studio 6.0+ AI 分析助手

从 DevEco Studio 6.0 起，Profiler 内置 AI 分析助手，支持：

1. 选中一段 trace 区间，右键 → **AI 分析**
2. 自动识别：长耗时函数、GC 频繁、渲染掉帧
3. 输出自然语言优化建议 + 相关代码位置

> **AI 调用限制**: GUI 工具，适合人工复核；程序化检测优先使用 HiDumper / HiDebug

---

## 4. HiPerf — Native CPU 热点分析

> **AI 调用方式**: `hdc shell hiperf <options>`
> **定位**: Rust/C++ Native 层 CPU 热点精准定位，无需重新编译

### 4.1 核心能力

| 子命令 | 功能 | AI 检测场景 |
|--------|------|------------|
| `hiperf record -p <pid>` | 采样进程 CPU 事件 | VPN Rust 层热点检测 |
| `hiperf record -a` | 全系统采样 | 系统级干扰分析 |
| `hiperf report` | 生成采样报告 | 热点函数排名 |
| `hiperf stat` | 硬件计数器统计 | Cache miss / branch miss |

### 4.2 AI 调用示例

```bash
# 1. 对 VPN 进程采样 10 秒
PID=$(hdc shell pidof co.phantom.harmony)
hdc shell "hiperf record -p $PID -o /data/local/tmp/perf.data --sleep 10"

# 2. 生成报告
hdc shell "hiperf report -i /data/local/tmp/perf.data -o /data/local/tmp/perf.report"

# 3. 拉取到本地分析
hdc file recv /data/local/tmp/perf.report ./perf.report

# 4. 硬件计数器统计
hdc shell "hiperf stat -p $PID --sleep 5 -e instructions,cycles,cache-misses"
```

### 4.3 AI 结果解析规则

```
# AI 解析 perf.report 示例
  78.23%  libphantom_core.so    encrypt::aes_gcm::encrypt    # 🔴 加密热点
  12.15%  libphantom_core.so    tun::read_packet             # ⚠️ TUN 读取
   5.40%  libphantom_core.so    protocol::pack                # ℹ️ 打包
   ...

# AI 判定规则
# 单函数占比 > 50% → 推荐 SIMD/硬件加速
# cache-misses/instructions > 5% → 内存访问局部性差
# 热点集中在 Rust unsafe 块 → 检查是否可安全优化
```

### 4.4 AI 输出格式

```json
{
  "tool": "hiperf",
  "status": "completed",
  "hotspots": [
    {"symbol": "encrypt::aes_gcm::encrypt", "percent": 78.23, "level": "critical"},
    {"symbol": "tun::read_packet", "percent": 12.15, "level": "warning"}
  ],
  "hardware_counters": {
    "instructions": 1_200_000_000,
    "cycles": 3_000_000_000,
    "cache_misses": 45_000_000,
    "cache_miss_rate": 3.75
  },
  "suggestion": "encrypt 函数占比 78%，推荐使用硬件 AES 指令加速"
}
```

---

## 目录

1. [工具全景图](#1-工具全景图)
2. [HiTraceMeter — 代码埋点追踪](#2-hitracemeter--代码埋点追踪)
3. [HiDumper — 命令行系统信息](#3-hidumper--命令行系统信息)
4. [HiPerf — Native CPU 热点分析](#4-hiperf--native-cpu-热点分析)
5. [DevEco Profiler — GUI 深度分析](#5-deveco-profiler--gui-深度分析)
6. [SmartPerf Host — 渲染与功耗分析](#6-smartperf-host--渲染与功耗分析)
7. [hdc 命令 — Trace 采集](#7-hdc-命令--trace-采集)
8. [HiDebug — 程序化性能数据采集](#8-hidebug--程序化性能数据采集)
9. [JSLeakWatcher — ArkTS 内存泄漏检测](#9-jsleakwatcher--arkts-内存泄漏检测)
10. [GWP-ASan — 堆内存越界检测](#10-gwp-asan--堆内存越界检测)
11. [AppFreeze 增强日志 — 冻屏自动分析](#11-appfreeze-增强日志--冻屏自动分析)
12. [ArkUI Inspector — UI 组件树检测](#12-arkui-inspector--ui-组件树检测)
13. [CodeLinter — 静态性能扫描](#13-codelinter--静态性能扫描)
14. [AppAnalyzer — 场景化自动体检](#14-appanalyzer--场景化自动体检)
15. [HiAppEvent — 线上故障订阅](#15-hiappevent--线上故障订阅)
16. [Profiler AI Assistant — AI 智能分析](#16-profiler-ai-assistant--ai-智能分析)
17. [SmartPerf Device (CLI) — 命令行性能采集](#17-smartperf-device-cli--命令行性能采集)
18. [VPN 专用检测方案](#18-vpn-专用检测方案)
19. [AI 自动化检测流程](#19-ai-自动化检测流程)
20. [工具速查表](#20-工具速查表)

---

## 1. 工具全景图

```
┌─────────────────────────────────────────────────────────────────┐
│                    鸿蒙性能分析工具体系                            │
├─────────────────────────────────────────────────────────────────┤
│                                                                 │
│  ┌─ 线上监控层 ────────────────────────────────────────────┐    │
│  │  AppGallery Connect 性能管理 / 自定义 SDK                  │    │
│  └──────────────────────────────────────────────────────────┘    │
│                          ↑                                      │
│  ┌─ 专业分析层 ────────────────────────────────────────────┐    │
│  │  HiPerf (CPU 热点)  │  SmartPerf (渲染/功耗)              │    │
│  │  HiDumper (系统信息) │  HAP 分析工具                       │    │
│  └──────────────────────────────────────────────────────────┘    │
│                          ↑                                      │
│  ┌─ 开发调试层 ────────────────────────────────────────────┐    │
│  │  DevEco Studio Profiler (GUI 全能)                         │    │
│  │  HiTraceMeter (代码埋点 API)                               │    │
│  │  hdc shell (命令行 trace 采集)                             │    │
│  └──────────────────────────────────────────────────────────┘    │
│                                                                 │
└─────────────────────────────────────────────────────────────────┘
```

### 按场景选择工具

| 场景 | 推荐工具 | 阶段 | AI 可自动化 |
|------|---------|------|-------------|
| 代码耗时埋点 | HiTraceMeter | 开发 | ✅ 代码注入 + hdc 采集 |
| CPU 热点 (Native/Rust) | HiPerf | 测试 | ✅ hdc 命令 |
| CPU/内存/网络 实时监控 | HiDumper | 测试 | ✅ hdc 命令 |
| ArkTS 调用栈 / 火焰图 | DevEco Profiler | 开发 | ❌ 需 GUI |
| UI 渲染掉帧 | SmartPerf Host | 测试 | ❌ 需 GUI |
| Trace 文件解析 | hdc + text2json | 任意 | ✅ 脚本解析 |
| 线上性能监控 | AGC 性能管理 | 线上 | ✅ API 调用 |

---

## 2. HiTraceMeter — 代码埋点追踪

> **AI 自动化友好度**: ⭐⭐⭐⭐⭐
> **原理**: 在关键代码路径插入 trace 点 → hdc 采集 → 解析 .trace 文件

### 2.1 ArkTS API

```typescript
import { hiTraceMeter } from '@kit.PerformanceAnalysisKit';

// === 同步 trace（最常用）===
hiTraceMeter.startTrace('vpn_packet_encrypt', 1);
// ... 加密逻辑 ...
hiTraceMeter.finishTrace('vpn_packet_encrypt', 1);

// === 异步 trace（跨线程/跨回调）===
const taskId = 1001;
hiTraceMeter.startAsyncTrace('vpn_tunnel_handshake', taskId);
// ... 异步握手 ...
hiTraceMeter.finishAsyncTrace('vpn_tunnel_handshake', taskId);

// === 计数器 trace（统计累计值）===
hiTraceMeter.traceValue('vpn_bytes_sent', sentBytes, 1);
hiTraceMeter.traceValue('vpn_bytes_received', recvBytes, 1);
```

### 2.2 C/C++ API（Rust NAPI 可通过 FFI 调用）

```c
#include "hitrace/trace.h"

// 同步 trace
OH_HiTrace_StartTrace(HITRACE_LEVEL_COMMERCIAL, "rust_cipher_encrypt");
// ... 加密 ...
OH_HiTrace_FinishTrace(HITRACE_LEVEL_COMMERCIAL);

// 异步 trace
OH_HiTrace_StartAsyncTrace(HITRACE_LEVEL_COMMERCIAL, "rust_tcp_send", task_id);
OH_HiTrace_FinishAsyncTrace(HITRACE_LEVEL_COMMERCIAL, "rust_tcp_send", task_id);

// 带自定义参数 (API 19+)
OH_HiTrace_StartTraceEx(HITRACE_LEVEL_COMMERCIAL, "vpn_read_tun", "fd=42,size=1400");
OH_HiTrace_FinishTraceEx(HITRACE_LEVEL_COMMERCIAL);
```

### 2.3 Rust 侧通过 FFI 调用（推荐方式）

```rust
#[cfg(target_os = "ohos")]
mod hitrace {
    extern "C" {
        fn OH_HiTrace_StartTrace(level: i32, name: *const u8);
        fn OH_HiTrace_FinishTrace(level: i32);
    }

    const HITRACE_LEVEL_COMMERCIAL: i32 = 0;

    pub fn start(name: &str) {
        let c_name = std::ffi::CString::new(name).unwrap();
        unsafe { OH_HiTrace_StartTrace(HITRACE_LEVEL_COMMERCIAL, c_name.as_ptr()); }
    }

    pub fn finish() {
        unsafe { OH_HiTrace_FinishTrace(HITRACE_LEVEL_COMMERCIAL); }
    }
}

// 使用示例
pub fn process_packet(pkt: &[u8]) {
    hitrace::start("vpn_process_packet");
    let encrypted = cipher.encrypt(pkt);
    hitrace::finish();
}
```

### 2.4 VPN 关键埋点建议

| 埋点名称 | 位置 | 类型 | 含义 |
|---------|------|------|------|
| `vpn_read_tun` | TUN fd 读取后 | sync | 从虚拟网卡读包耗时 |
| `vpn_decrypt` | 解密后 | sync | 解密耗时 |
| `vpn_encrypt` | 加密后 | sync | 加密耗时 |
| `vpn_write_socket` | 写真实 socket 后 | sync | 网络发送耗时 |
| `vpn_tunnel_handshake` | 握手完成 | async | 握手总耗时 |
| `vpn_bytes_sent` | 每秒统计 | value | 发送字节数 |
| `vpn_bytes_received` | 每秒统计 | value | 接收字节数 |
| `vpn_tcp_connect` | TCP 连接建立 | async | TCP 连接耗时 |

---

## 3. HiDumper — 命令行系统信息

> **AI 调用方式**: `hdc shell hidumper <options>`
> **定位**: 获取系统级性能快照，免埋点

### 3.1 核心能力

| 子命令 | 功能 | AI 检测场景 |
|--------|------|------------|
| `hidumper -s <serviceName> -a <args>` | 获取指定系统服务状态 | 检测网络连接数、内存统计 |
| `hidumper --mem <pid>` | 获取进程内存详情 | VPN 进程内存泄漏检测 |
| `hidumper --cpu <pid>` | 获取进程 CPU 占用 | VPN CPU 峰值检测 |
| `hidumper --mem-jsheap <pid>` | 获取 JS Heap 使用量 | ArkTS 堆内存泄漏检测 |
| `hidumper --gc <pid>` | 触发并统计 GC 信息 | ArkTS 内存压力评估 |
| `hidumper --net` | 网络连接统计 | VPN 连接泄漏检测 |

### 3.2 AI 调用示例

```bash
# 1. 获取 Phantom VPN 进程 PID
PID=$(hdc shell pidof co.phantom.harmony)

# 2. 采集进程内存详情
hdc shell hidumper --mem $PID

# 3. 采集 JS Heap 使用量
hdc shell hidumper --mem-jsheap $PID

# 4. 采集 CPU 占用率
hdc shell hidumper --cpu $PID
```

### 3.3 AI 结果解析规则

```
# AI 解析 hidumper --mem 输出示例
Pss(KB)  : 123456       # 物理内存占用，重点关注
Shared   : 2048         # 共享内存
Private  : 120000       # 私有内存 → 若持续增长 => 内存泄漏
Swap(KB) : 4096         # 交换内存
```

> **AI 判定阈值**：
> - PSS > 300MB → 标记为 **内存过高** ⚠️
> - 连续 3 次采集 PSS 增长 > 5% → 标记为 **疑似泄漏** 🔴
> - JS Heap > 100MB → 标记为 **JS 堆过大** ⚠️

### 3.4 数据流图

```
AI ──hdc shell──▶ HiDumper ──▶ 系统内核/服务 ──▶ JSON/Text
                                                    │
AI ◀──解析结果◀─── 规则引擎 ◀─── 结构化输出 ────────┘
                        │
                    优化建议 ──▶ Report.md
```

---
