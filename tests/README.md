# Phantom E2E 测试（`phantom-e2e`）

进程内启动真实服务端 + 真实客户端跑完整链路，**不打桩、不 mock 协议**。
本 crate 不是库，只提供测试固件与测试用例。

## 目录

| 路径 | 内容 |
|------|------|
| `src/` | 测试固件：`echo.rs`（TCP/UDP echo）、`mock_web.rs`（Mock 百度）、`fixture.rs`（端到端装配）、`network.rs`（弱网模拟）、`socks5.rs`、`throughput.rs`、`udp_echo.rs` |
| `tests/` | 集成用例，一文件一主题（见下表） |
| `e2e/` | 手工/半自动脚本与数据面验证工具（`minihttpd.rs`、`lossy_proxy.rs`、各 `stage-*.sh`、`bench-*.sh`） |
| `bench/` | 性能基准，见 [bench/README.md](bench/README.md) |
| `*.md` | 测试与性能报告：`E2E_TEST_PLAN.md`、`E2E_CIPHER_REPORT.md`、`PERF_OPTIMIZATION_REPORT.md`、`PERF_TUN_PATH_REPORT.md` |

## 用例索引（`tests/`）

| 文件 | 覆盖 |
|------|------|
| `cipher_matrix.rs` | 五种密码套件 × TCP/QUIC 协商矩阵 |
| `config_effect.rs` | 白名单、配置生效 |
| `correctness.rs` | 数据正确性 / 大流量不串流 |
| `full_link_tcp.rs` / `full_link_udp.rs` | TCP / UDP 全链路 |
| `dns_hijack.rs` | TUN 模式 DNS 拦截与防泄露 |
| `rule_engine.rs` / `rule_engine_equivalence.rs` | 分流规则与新旧引擎等价性 |
| `quic_mux.rs` | 8 隧道共享 1 连接、错误 PSK 零握手 |
| `http_proxy.rs` / `http_tunnel.rs` | HTTP CONNECT、绝对 URI 重写、同端口嗅探 |
| `socks5_udp.rs` / `tcp_session_pool.rs` | UDP ASSOCIATE、TCP 会话池 |
| `stats_metrics.rs` | Prometheus 指标导出 |
| `weak_network.rs` | 弱网（丢包 / 抖动）表现 |
| `performance.rs` / `throughput.rs` | 吞吐与延迟（`--ignored`，需手动跑） |
| `real_world.rs` | Mock 百度（默认）/ 真百度（`--ignored`） |
| `cli_system.rs` | L2 系统级：自举、端口递增、TUN/网关参数校验 |

## 常用命令

```bash
cargo test -p phantom-e2e --release                              # 全量
cargo test -p phantom-e2e --test full_link_tcp --release         # 单个
cargo test -p phantom-e2e --test real_world --release -- --ignored   # 真实外网
cargo bench -p phantom-bench                                     # 基准
```

> 需要 TUN / 网关的用例在 macOS 上会被跳过；网关单测为 Linux 专属。
