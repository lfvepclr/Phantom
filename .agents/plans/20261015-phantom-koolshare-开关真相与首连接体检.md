# koolshare 插件体检：开关真相、首连接顿卡、国内 DNS 备选池、性能采样与 Rust 升级

日期：2026-10-15
范围：client/koolshare/**、client/src/{ipset,tun,gateway,tcp_pool,dns,stats}.rs、core/src/config.rs、xtask、文档。
关联：[20260913-phantom-koolshare-路由器插件.md](20260913-phantom-koolshare-路由器插件.md)、
[20261004-phantom-koolshare-提交失败-失效会话打崩httpd.md](20261004-phantom-koolshare-提交失败-失效会话打崩httpd.md)、
[20261004-phantom-路由器内核分流-直连回内核快路径与夜间性能取证.md](20261004-phantom-路由器内核分流-直连回内核快路径与夜间性能取证.md)。

> 文中 `<路由器IP>` / `<SSH端口>` 为占位符；本仓库不记录真实地址、端口与密钥。

## 一、体检结论（真机只读 + 代理侧实测）

目标机型 RT-AX86U Pro，koolshare 官改 3.0.0.4.388_24199_koolcenter，内核 4.19.183，
设备已连续运行 18h 以上（uptime 18:48，load 1.28）。

### 1.1 控制面是健康的

| 项 | 观测 |
|---|---|
| httpd 崩溃 | 本次开机以来 grep -c 'Comm: httpd' /tmp/syslog.log = 0（上一轮「失效会话打崩 httpd」修复后未再增长） |
| phantom 进程 | 常驻 16h，RSS 8.9 MB，6 线程，CPU 0–1%（/proc/<pid>/stat 增量） |
| fd | 194 / 16384（ulimit -n 16384 生效） |
| conntrack | 1759 条，上限充裕；MemFree 314 MB / 1018 MB |
| 其他 | 无 OOM、无 call trace |

### 1.2 「开关不好用」= 三条无视开关自行拉起的路径（已复现）

1. 开机钩子不看开关：syslog 里躺着 `admin: [软件中心]-[ks-wan-start.sh]: /koolshare/init.d/S98phantom.sh start`，
   而 phantom_config.sh 的 start 全程不读 phantom_enable → 关掉插件后重启路由器照样拉起。
2. 定时重启不看开关：cru l 有 `30 4 * * * …/phantom_config.sh restart #phantom_restart#`，
   它只受 cron_enable 控制；restart 同样不读 enable，stop() 也不删这条 cron。
   插件日志 04:30:02「服务已停止」→ 04:30:06「启动成功」正是它干的。
3. 页面看不出真相：总开关只是本地变量 + 显隐，必须点【提交】才落库；而 get_run_status() 在开关为「关」时
   直接 render_stopped() 返回，根本不查询真实状态；状态文件也没有时间戳新鲜度判断，采样进程死掉后
   页面会一直显示旧值。

### 1.3 「首次打开 Google 顿一下」= 两条成因

1. ipset 延迟发布竞态（主因）：DNS 应答已经写回客户端，但 IpsetPublisher::add_ips() 只把 IP 放进
   pending，真正执行 ipset restore 的是 1 秒一次的 flush_loop（FLUSH_INTERVAL = 1s，见 client/src/ipset.rs）。
   tun.rs::deliver_dns_response 的注释写着 "Publishing it here — before the response reaches the app"，
   与实现不符：客户端拿到 A 记录后立刻发 SYN，此时内核 ipset 里还没有该 IP → 首个连接走直连 → 被墙
   RST/超时 → 应用重试（1 秒后 IP 已落库）才进隧道。
   现场证据：phantom_tun_unknown_flow_rst_total（非 SYN 包进了 TUN 只能回 RST）25 分钟从 697 涨到 781
   （约 3 次/分钟），说明「中途被拉进 TUN 的连接」一直在发生。
2. 冷连接代价：经 SOCKS5 实测（/koolshare/bin/curl-fancyss；固件自带 curl 是 --disable-proxy 编译的，不能用）：

| 探测 | DNS | connect | TLS | TTFB |
|---|---|---|---|---|
| 第 1 次（冷） | 0.08 ms | 0.38 ms | 547 ms | 635 ms |
| 第 2 次 | 0.08 ms | 0.48 ms | 124 ms | 223 ms |
| 第 3 次 | 0.09 ms | 0.42 ms | 127 ms | 228 ms |

   3 次探测 = phantom_tunnel_connects_total +3，但**这个计数是「每次转发尝试」而不是
   「物理握手次数」**（`socks5.rs` 在 take/冷建分支之前就 +1，指标 HELP 文案写成
   "Outer tunnel connections opened" 属于误导，本轮一并修正文案）——所以 +3 只能说明
   三次都走了转发路径，**不能**证明每次都是冷连接。冷/暖 412ms 的差额究竟有多少来自
   隧道握手，要靠新加的 `phantom_pool_*` 指标在真机上判定；在此之前不给结论。
   （TcpSessionPool 的参数：TARGET_IDLE=2、MAX_IDLE_AGE=8s、REFILL_DELAY=150ms；
   其 PoolStats{created,reused} 过去没有暴露成 Prometheus 指标，真机上看不见命中率。）

### 1.4 日志异常

| 现象 | 定性 |
|---|---|
| phantom_perf.sh 的 fc_hw 恒为 0 | 真缺陷。分类正则 `^ *[0-9]+ ` 只匹配到软件流那半边，`$0 !~ /4294967295/` 因此恒为 0。实测同一时刻 /proc/fcache/nflist 有 905 条流、其中 453 条 Fhw_idx 非哨兵值（约 50% 走硬件加速）—— 上一轮「flow cache 完全没用上」的结论是被采样器误导的 |
| perf 日志 conns 列 | 语义错误：取的是 phantom_tcp_connections（累计计数器，02:05=0 → 20:45=2245），却当作「活跃连接数」。真实活跃度看 conntrack |
| 每次启动的 WARN phantom_client::failover: Config reload: server list is empty | 噪音；需查明条件或降级 |
| 开机阶段日志时间戳 2018-05-05 13:05:33（早于 02:02，NTP 未同步） | 真机时钟未同步导致，易被误读为「日志乱序」 |

### 1.5 性能与重传

| 指标 | 观测 | 结论 |
|---|---|---|
| 路由器自身 TCP 重传 | /proc/net/snmp 30s 增量：OutSegs +9855、RetransSegs +23 → 0.23% | 当前正常（累计 4% 是历史值） |
| WAN RTT / 丢包 | ping 223.5.5.5 全天 4 ms、0% 丢包 | ISP 侧白天无异常 |
| phantom CPU | 全天 0–1% | 数据面不是瓶颈 |
| 无线 | 2.4G(eth6) txretrans 407748/1377809=29.6%、rxcrc 869779；5G(eth7) 378990/2256495=16.8%、rxerror 8 | 唯一可量化的「卡顿」来源：2.4G 空口干扰严重（2.4G/5G 各 8 台客户端） |
| 有线 | eth0 RX drop 71609（约 1/s 持续增长）、errors 12；eth2 只协商到 100Mb/s | 需记录与观察 |
| 隧道 | tun_write_wait_max_ms 225、flow_stall_drops 12、dup_acks 2488、tcp_dup_bytes 328 KB（16h 累计） | 量级很轻，不是当前主因 |

### 1.6 泄漏体检基线

- 路由器 IPv6 全关（nvram get ipv6_service = disabled，ip -6 addr 无输出）→ 不存在 IPv6 泄漏路径。
- DNS 分流已是双通道：client.dns（默认 8.8.8.8:53）只经隧道（隧道内共享 UDP 流）；
  client.dns_direct（默认 223.5.5.5:53）只走物理网卡；LAN 的 53 被 mangle 标记进 TUN + nat DNAT 兜底；
  隧道域名默认不下发 AAAA（dns_ipv6_via_tunnel=false）。
- tls:// 前缀目前只是形式（parse_dns_addr 只做 strip，仍是明文 UDP）→ DoT/DoH 未实现。
- 国内 DNS 单点：dns_direct 是单个 String → 单个 SocketAddr → 一个 local_socket 只对一个上游发；
  上游抽风/被劫持即国内域名全线解析失败。

### 1.7 依赖与工具链

- 本机 rustc/cargo 1.96.0，rustup check 提示可升 1.99.0。
- cargo update --dry-run：89 个包可升（tokio 1.52.3→1.53.3、serde_json 1.0.150→1.0.154、
  thiserror 2.0.18→2.0.21、clap 4.6.1→4.6.7 等），无跨大版本破坏性跳变。
- 仓库无 rust-toolchain.toml、无 CI 固定版本；Cargo.lock 被 .gitignore 忽略（保持现状）。

## 二、本轮改动

### 2.1 开关：关就是关

- phantom_config.sh：start 默认读 phantom_enable（≠1 只记一行「已关闭，忽略 start」并退出 0）；
  新增 start_force 作为唯一例外；restart 同样受门控（≠1 只 stop + 清理）。
- phantom_cron.sh sync：定时重启仅在 enable=1 且 cron_enable=1 时注册，否则删除；
  stop() 末尾调用 sync，使「关掉即无拉起路径」。
- 看门狗保留「关闭时只清残留、不启动」的语义。
- 每条 start/stop 日志与 last_act 带触发源标签 [boot|cron|nat|watchdog|user|install]，
  新增 dbus 键 phantom_run_trigger 记录当前运行实例由谁拉起。

### 2.2 页面：期望 vs 实际（rev 9）

- 状态 JSON 增 rules（ip rule + iptables 是否在位，15s 粒度缓存）、proc、trigger。
- 无论开关开或关都轮询状态（关 5s / 开 2.5s，仍走 .asp 会话探针门控）：
  显示「期望：开/关」「实际：运行中/已停止/残留/状态未知」。
- 期望与实际不一致时：关闭状态却有进程/规则 → 红条 + 【一键修复】（action cleanup）；
  开启状态但未运行 → 黄条提示看日志；运行中但规则不在位 → 黄条提示 LAN 可能直连。
- 状态文件 ts 停止推进超 20s → 标记「状态采样已停止，数据可能过期」。
- 开关改动未提交时提示「已改动，点【提交】生效」；提交成功后刷新本地基线。

### 2.3 首连接：消除竞态 + 冷启动

- IpsetPublisher::publish_now()：对 DnsRoute::Tunnel 的应答，在写回 TUN 之前同步完成 ipset restore。
  与 flush_loop 共用一把 flush_lock（覆盖 take_batch + push），因此并发冲刷时「要么本函数落库、
  要么正持有锁的那次落库」，返回即代表已生效。已在集合内（TTL 未过期）的 IP 直接跳过，不产生多余 fork。
  新增指标 phantom_ipset_publish_wait_max_ms 记录等待最大值。
- 会话池：prewarm_tcp_pool 之外，每次取用后仍主动 refill；把 phantom_pool_created_total /
  phantom_pool_reused_total / phantom_pool_idle 暴露成指标，让「冷启动到底付了多少钱」可被观测。

### 2.4 国内 DNS 备选池

- client.dns_direct 值语法扩展为逗号/空白分隔的地址列表（单值仍合法）；新增 parse_dns_addrs()，
  parse_dns_addr() 保留为取首个地址/旧调用点。默认池：223.5.5.5:53,119.29.29.29:53,180.76.76.76:53
  （阿里/腾讯/百度）。
- DnsProxy::new(tunnel_upstream, Vec<SocketAddr>)；set_direct_upstreams() 取代 set_direct_upstream()
  （热更新整个池并重置健康状态）。
- 切换策略：900 ms 无应答即换下一个上游（同 id 重发，最多 3 次尝试；迟到应答因 pending 已消费而丢弃）；
  连续 3 次失败的上游冷却 60 s 后排到最后；200 ms 清扫任务驱动重试。
- 指标：phantom_dns_direct_active、phantom_dns_direct_failovers_total、phantom_dns_direct_timeouts_total。
- client.dns（隧道侧）保持单地址：它经隧道由服务端网络解析，扩成池会牵动隧道内共享 UDP 流。
- 插件设置页新增「国内解析器池」输入框（默认上述三台，逗号分隔，做 IP:端口校验）并写入生成的 TOML；
  cleanup_rules 里 DNAT 取值仍从 dns = "…" 行提取（未受影响）。

### 2.5 数据面：只标记新连接（connmark）

- 复核：本机 /proc/net/ip_tables_matches 有 conntrack，ip_tables_targets 有 CONNMARK。
- plan_kernel_split 改为：
  - PREROUTING 首条 `-m conntrack --ctstate ESTABLISHED,RELATED -j CONNMARK --restore-mark`
  - ipset 规则 `-i <lan> -m conntrack --ctstate NEW -m set --match-set phantom_proxy dst -j MARK --set-mark 1 -j CONNMARK --save-mark`
  - DNS 53 保持逐包标记（学习通道不能断）
  - -I 是前插，因此按逆序生成命令；teardown_plan 对称删除两条新规则。
- 在途连接因此不再被中途拉进 TUN（unknown_flow_rst 停止增长），新连接仍按白名单进隧道。
- 能力探测失败（无 conntrack/CONNMARK）回退旧规则并暴露 phantom_gateway_connmark。

### 2.6 采样修正 + 一键网络体检

- phantom_perf.sh：flow-cache 改用「总行数−表头」与 grep -vc 4294967295 判定；conns 改为
  「本 5 分钟新增连接数（累计差值）」并保留 conntrack；新增 dup_acks、tcp_dup_bytes、tun_wait_max_ms、
  txq_peak、tunnel_conn_fail、unknown_rst、stall_drops、pool_reused、eth0/eth2 drop+err 增量、
  wifi24/wifi5 的 txretrans/rxcrc 增量与两频段客户端数；表头升 v2，旧行仍可解析（分析按表头列名取值）。
- 新增 phantom_health.sh + action health，页面运维页加「一键网络体检」按钮，结果写
  /tmp/upload/phantom_health.txt，逐条 ✅/⚠️/❌ + 建议：分流实况、国内 DNS 池、DNS 走向与 AAAA 抑制、
  IPv6 状态、WebRTC/真实 IP 说明、冷/暖首连接耗时、性能快照。
- 启动时写 boot marker（date/uptime/trigger），解释 NTP 未同步期间的时间戳。
- live-check.sh 增：开关真相（enable vs 进程 vs ip rule vs cron）、S98/N98 钩子、双频无线计数器、
  以太口 drop/err 增量、perf 日志新鲜度、国内 DNS 池探测。

### 2.7 Rust 与依赖

- rustup update 到 1.99.x + cargo update 落 patch/minor（不跨大版本）；修新工具链引入的 lint/编译问题；
  跑全量单测 + 端到端 + 基准冒烟；重出 koolshare 双架构包与 server 包。
- 若某依赖在 1.99 下破坏 aarch64-musl / armv7 / OHOS 目标，逐个回退该依赖并在此记录。

## 三、接口与默认值

| 项 | 变更 |
|---|---|
| action | 新增 start_force、cleanup、health |
| 状态 JSON | 新增 rules / proc / trigger |
| dbus | 新增 phantom_run_trigger（当前实例由谁拉起）、phantom_dns_direct（国内解析器池，逗号分隔） |
| 配置 | client.dns_direct 支持逗号分隔列表，默认三台国内公共 DNS；ARCHITECTURE.md §5 同步 |
| 指标 | phantom_ipset_publish_wait_max_ms、phantom_pool_created_total、phantom_pool_reused_total、phantom_pool_idle、phantom_gateway_connmark、phantom_dns_direct_active、phantom_dns_direct_failovers_total、phantom_dns_direct_timeouts_total |

不做：DoT/DoH（需要新增 TLS 客户端栈，tls:// 前缀目前只是形式）、WebRTC 防泄漏改造、IPv6 隧道、
射频参数调整、client.dns 多地址。

## 四、测试与验收

- L0：`sh -n` 全部脚本；`cargo check --workspace`；`cargo fmt --check`。
- L1：`PHANTOM_SMOKE_ROOT=$(mktemp -d) sh client/koolshare/phantom/tests/smoke.sh`，
  以及 `PHANTOM_SMOKE_MODE=jffs PHANTOM_SMOKE_ROOT=$(mktemp -d) sh …`（两种形态各 158 项），
  再加 `cargo test -p phantom-client`。断言见 §五。
  注意：`PHANTOM_SMOKE_ROOT` 不要带 `/jffs` 后缀 —— 脚本里的路径重写会把插入的前缀再替换一次，
  造出「$ROOT/$ROOT/jffs」这种假路径，结果 jffs 形态会误报「初始状态文件缺失」（与插件无关）。
- L2 真机（部署新包后，需要人工配合）：
  1. 关掉开关 → 重启路由器 → 仍为关、cru l 无 phantom_restart、页面显示「期望关/实际关」；
     打开 → 3–6 s 内变「运行中」；再关闭 → 进程 / ip rule / iptables 零残留。
  2. 首连接：清空浏览器与 DNS 缓存后首次打开 Google，冷 TTFB 明显下降、unknown_flow_rst 不再增长、
     `iptables -t mangle -S PREROUTING` 可见 restore/save 规则、pool_reused 开始增长。
  3. DNS 池：体检页确认三台上游可达与 RTT；把首选改成不可达地址（如 203.0.113.1）验证自动切换、
     国内域名解析不中断、failovers_total 增长且切回后恢复。
  4. 泄漏三测（browserleaks dns/webrtc/ip）按文档口径复现并记录结论。
  5. 过夜一版新 perf 日志，对照 2.4G/5G + eth0/eth2 增量；Rust 升级后重出包跑一天无异常。

## 五、测试断言清单（L1）

- 脚本：enable=0 时 start 不启动进程、restart 只停不启、cron sync 不注册 restart；start_force 仍能启动；
  stop 会清掉 phantom_restart；start_nat 仅在规则缺失时重启。
- 状态：JSON 含 rules/proc/trigger；ASP 含双徽章、残留横幅、未提交提示、一键修复、状态新鲜度判断。
- perf：表头 v2 且列名齐全；flow-cache 统计对固定样例文本得出非 0 的 HW 命中。
- health：脚本产出结构化结论（含国内 DNS 池逐台探测）。
- Rust：parse_dns_addrs 混合分隔/非法项/单值兼容；上游超时后切换且只向客户端回一次、迟到应答被丢弃、
  失败冷却与恢复、正常应答不产生多余重试；DNS 应答前 ipset 已发布（顺序断言）且后台批量路径保留；
  connmark 规则与 teardown 快照；pool 指标随取用增长。

## 六、假设

- start_force 是「关就是关」的唯一例外。
- 未匹配域名继续用国内明文 DNS 解析（browserleaks 显示大陆国旗属分流设计，不是泄漏）；
  WebRTC 暴露家宽公网 IP 属「默认直连」策略的必然结果，本轮只在文档说明。
- client.dns 保持单地址；不引入 DoT/DoH 与 IPv6 隧道。
- Cargo.lock 继续不入库（保持现有 .gitignore 约定）。
- 验收需要人工部署新包、浏览器 Ctrl+F5 拿新 rev，并重启一次路由器。
