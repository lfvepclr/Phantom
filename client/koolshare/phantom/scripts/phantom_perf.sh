#!/bin/sh
# Phantom 夜间性能采样（每 5 分钟一行，落到 /tmp/upload/phantom_perf.log）
#
# 为什么需要它：故障只在"晚上人多"时出现，白天复现不了，而插件原本没有留下
# 任何可回看的数据（页面的 CPU 还恒为 0）。这里每分钟级别地记下判断瓶颈所需
# 的最小集合，第二天 `tail` 一下就能回答"到底是谁慢"：
#
#   cpu_pct  phantom 占整机 CPU 的百分比（多核合计）
#   new_conns 本 5 分钟**新增**连接数（累计差值；直接读 phantom_tcp_connections 是
#             累计值，曾被当成"活跃连接数"，于是"晚上人多"这件事在数据上永远看不出来）
#   conns_total 累计连接数（同一份原始计数，留着对账）
#   fd        phantom 进程的 fd 使用数/上限（EMFILE 会表现为"新连接打不开"）
#   ct       conntrack 条目数（每 LAN 连接会占两条）
#   fc_hw/sw flow cache 里走硬件加速的流数 / 全部流数（hw=0 说明完全没用上）
#   d_up/dn  直连（明文）字节累计     t_up/dn 隧道（加密）字节累计
#   ipset    内核分流白名单条目数
#   dns_ms   上游 DNS 解析耗时（路由器自身，直连）
#   rtt/loss 到公网的 RTT 与丢包（判断是不是 ISP 晚高峰）
#   wifi     关联的无线客户端数（判断是不是空口争用）
#   wifi24/5 两个频段各自的客户端数；wifi24/5_txretrans|rxcrc 是**增量**
#            （2.4G 的 txretrans 占比 + rxcrc 是"无线卡顿"最直接的证据）
#   dup_acks/tcp_dup    重复 ACK 与重复注入字节（隧道侧重传/乱序的代理指标）
#   tun_wait_max/txq_peak  TUN 写入最长等待 / 写队列峰值（用户态 relay 卡顿）
#   tunnel_conn_fail/unknown_rst/stall_drops  隧道建连失败 / 未知流被 RST /
#            停滞放弃的流数（"看着看着断一下"就是 unknown_rst 在涨）
#   pool_*   TCP 会话池：created / reused / idle（reused 才是"池子替人省下握手"的判词）
#   eth*_rx_drop_d / eth*_rx_err_d  以太口 drop/err 的**增量**
#   link     各以太口协商速率（100Mb 的口会拖慢挂在上面的设备）
#
# 表头只在首次运行时写入（v2：第 2 列 ver=2；旧行仍可解析 —— 分析脚本按表头名取值）。
# 日志本身由 phantom_config.sh 的 _trim 顺手裁剪。

module=phantom

for candidate in /koolshare/etc/phantom/phantom.env /jffs/phantom/phantom.env; do
    if [ -f "$candidate" ]; then
        . "$candidate"
        break
    fi
done

PERF_LOG="${PHANTOM_PERF_LOG:-/tmp/upload/phantom_perf.log}"
PIDFILE="${PHANTOM_PIDFILE:-/tmp/phantom.pid}"
STATE="${PHANTOM_PERF_STATE:-/tmp/phantom_perf.state}"

PHANTOM_BIN_DIRS="${PHANTOM_BIN_DIRS:-/bin /sbin /usr/bin /usr/sbin /koolshare/bin /opt/bin /usr/local/bin}"
cmd_path() {
    _c="$1"
    _dirs=$(printf "%s" "$PHANTOM_BIN_DIRS" | tr ":" " ")
    case "$_c" in
        /*) [ -x "$_c" ] && { printf '%s' "$_c"; return 0; } ;;
    esac
    for _d in $_dirs; do
        if [ -x "$_d/$_c" ]; then
            printf '%s' "$_d/$_c"
            return 0
        fi
    done
    return 1
}

CURL=$(cmd_path curl)
PING=$(cmd_path ping)

[ -d "$(dirname "$PERF_LOG")" ] || mkdir -p "$(dirname "$PERF_LOG")" 2>/dev/null

HZ=$(getconf CLK_TCK 2>/dev/null)
case "$HZ" in ''|*[!0-9]*) HZ=100 ;; esac

proc_cpu_ticks() {
    _line=$(cat "/proc/$1/stat" 2>/dev/null) || return 0
    _rest=${_line##*') '}
    # shellcheck disable=SC2086
    set -- $_rest
    echo $(( ${12:-0} + ${13:-0} ))
}

now=$(date +%s)
pid=$(cat "$PIDFILE" 2>/dev/null)
cpu_pct=0
ticks=0
# 上一次的原始计数（用于求增量）。文件是我们自己写的 KEY=value 数字，
# source 进来最省 fork；每个值都过一遍非数字校验，坏值当 0。
prev_ticks=0; prev_ts=0; prev_conns=0
prev_eth0_drop=0; prev_eth0_err=0; prev_eth2_drop=0; prev_eth2_err=0
prev_w24_txr=0; prev_w24_rxc=0; prev_w5_txr=0; prev_w5_rxc=0
if [ -f "$STATE" ]; then
    . "$STATE" 2>/dev/null
    prev_ticks=${ticks:-0};   case "$prev_ticks" in ''|*[!0-9]*) prev_ticks=0 ;; esac
    prev_ts=${ts:-0};         case "$prev_ts" in ''|*[!0-9]*) prev_ts=0 ;; esac
    prev_conns=${conns:-0};   case "$prev_conns" in ''|*[!0-9]*) prev_conns=0 ;; esac
    prev_eth0_drop=${eth0_drop:-0}; case "$prev_eth0_drop" in ''|*[!0-9]*) prev_eth0_drop=0 ;; esac
    prev_eth0_err=${eth0_err:-0};   case "$prev_eth0_err" in ''|*[!0-9]*) prev_eth0_err=0 ;; esac
    prev_eth2_drop=${eth2_drop:-0}; case "$prev_eth2_drop" in ''|*[!0-9]*) prev_eth2_drop=0 ;; esac
    prev_eth2_err=${eth2_err:-0};   case "$prev_eth2_err" in ''|*[!0-9]*) prev_eth2_err=0 ;; esac
    prev_w24_txr=${w24_txr:-0};     case "$prev_w24_txr" in ''|*[!0-9]*) prev_w24_txr=0 ;; esac
    prev_w24_rxc=${w24_rxc:-0};     case "$prev_w24_rxc" in ''|*[!0-9]*) prev_w24_rxc=0 ;; esac
    prev_w5_txr=${w5_txr:-0};       case "$prev_w5_txr" in ''|*[!0-9]*) prev_w5_txr=0 ;; esac
    prev_w5_rxc=${w5_rxc:-0};       case "$prev_w5_rxc" in ''|*[!0-9]*) prev_w5_rxc=0 ;; esac
fi

# 增量：计数器回绕/重置时按 0 处理，别写出负数把分析看歪。
delta() {
    _d=$(( $1 - $2 ))
    [ "$_d" -lt 0 ] 2>/dev/null && _d=0
    printf '%s' "$_d"
}

if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    ticks=$(proc_cpu_ticks "$pid")
    [ -n "$ticks" ] || ticks=0
    if [ "${prev_ts:-0}" -gt 0 ] 2>/dev/null; then
        dt=$((now - prev_ts))
        [ "$dt" -le 0 ] && dt=1
        dticks=$((ticks - prev_ticks))
        [ "$dticks" -lt 0 ] && dticks=0
        cpu_pct=$((dticks * 100 / (HZ * dt)))
    fi
fi

# ---- metrics（连接数、字节分段、ipset 条目、生效模式）---------------------
conns=0; d_up=0; d_dn=0; t_up=0; t_dn=0; ipset=0; split=-1
dup_acks=0; tcp_dup=0; tun_wait_max=0; txq_peak=0
conn_fail=0; unknown_rst=0; stall_drops=0
pool_created=0; pool_reused=0; pool_idle=0
if [ -n "$CURL" ]; then
    body=$("$CURL" -s --max-time 3 http://127.0.0.1:9150/metrics 2>/dev/null)
    if [ -n "$body" ]; then
        get() { printf '%s' "$body" | sed -n "s/^$1 \([0-9]\{1,\}\)\$/\1/p" | head -n 1; }
        conns=$(get phantom_tcp_connections);   [ -n "$conns" ] || conns=0
        d_up=$(get phantom_direct_bytes_up);    [ -n "$d_up" ] || d_up=0
        d_dn=$(get phantom_direct_bytes_down);  [ -n "$d_dn" ] || d_dn=0
        t_up=$(get phantom_tunnel_bytes_up);    [ -n "$t_up" ] || t_up=0
        t_dn=$(get phantom_tunnel_bytes_down);  [ -n "$t_dn" ] || t_dn=0
        ipset=$(get phantom_whitelist_ipset_entries); [ -n "$ipset" ] || ipset=0
        split=$(get phantom_gateway_kernel_split);    [ -n "$split" ] || split=-1
        # 隧道侧健康度：这几个才是「看着看着断一下」「重传」的直接证据
        dup_acks=$(get phantom_dup_acks_total);           [ -n "$dup_acks" ] || dup_acks=0
        tcp_dup=$(get phantom_tcp_dup_bytes);             [ -n "$tcp_dup" ] || tcp_dup=0
        tun_wait_max=$(get phantom_tun_write_wait_max_ms); [ -n "$tun_wait_max" ] || tun_wait_max=0
        txq_peak=$(get phantom_tun_txq_peak_bytes);       [ -n "$txq_peak" ] || txq_peak=0
        conn_fail=$(get phantom_tunnel_connect_failures_total); [ -n "$conn_fail" ] || conn_fail=0
        unknown_rst=$(get phantom_tun_unknown_flow_rst_total);  [ -n "$unknown_rst" ] || unknown_rst=0
        stall_drops=$(get phantom_flow_stall_drops_total); [ -n "$stall_drops" ] || stall_drops=0
        pool_created=$(get phantom_pool_created_total);   [ -n "$pool_created" ] || pool_created=0
        pool_reused=$(get phantom_pool_reused_total);     [ -n "$pool_reused" ] || pool_reused=0
        pool_idle=$(get phantom_pool_idle);               [ -n "$pool_idle" ] || pool_idle=0
    fi
fi

# ---- fd / conntrack ------------------------------------------------------
#
# 顺带算「本周期新增连接数」：phantom_tcp_connections 是累计计数器，直接记它
# 曾被当成"活跃连接数"，于是"晚上人多才慢"在数据里永远看不出来。
new_conns=$(delta "${conns:-0}" "${prev_conns:-0}")

fd_used=0; fd_limit=0
if [ -n "$pid" ]; then
    fd_used=$(ls /proc/"$pid"/fd 2>/dev/null | wc -l | tr -d ' ')
    fd_limit=$(awk '/Max open files/ {print $4}' /proc/"$pid"/limits 2>/dev/null | head -n 1)
fi
[ -n "$fd_used" ] || fd_used=0
[ -n "$fd_limit" ] || fd_limit=0
ct=$(cat /proc/sys/net/netfilter/nf_conntrack_count 2>/dev/null)
[ -n "$ct" ] || ct=0

# ---- flow cache：硬件加速到底有没有生效 ---------------------------------
#
# **这里有过一个把结论带偏的 bug**：过去用
#     awk '/^ *[0-9]+ / { total++; if ($0 !~ /4294967295/) hw++ }'
# 分类，而带 HW 命中的那些行并不匹配「行首空格+数字」这个形状 ——
# 匹配上的全是软件流那半边，于是 hw 恒为 0，看起来像"flow cache 完全没用上"。
# 实测同一时刻 905 条流里有 453 条 HW 列不是哨兵值（约一半在硬件加速）。
# 现在改成按「表头之外的行数」计数、用 grep 判哨兵（busybox 下也一样可靠）。
fc_total=0; fc_hw=0
if [ -f /proc/fcache/nflist ]; then
    fc_total=$(wc -l < /proc/fcache/nflist 2>/dev/null | tr -d ' ')
    [ -n "$fc_total" ] && [ "$fc_total" -ge 2 ] 2>/dev/null && fc_total=$((fc_total - 2))
    [ -n "$fc_total" ] || fc_total=0
    fc_sw_count=$(grep -c 4294967295 /proc/fcache/nflist 2>/dev/null | tr -d ' ')
    [ -n "$fc_sw_count" ] || fc_sw_count=0
    # 哨兵 = 该流没有硬件索引；扣掉两行表头里可能出现的匹配
    fc_hw=$((fc_total - fc_sw_count))
    [ "$fc_hw" -lt 0 ] && fc_hw=0
fi
fc_sw=$((fc_total - fc_hw))

# ---- 上游 DNS 与公网 RTT -------------------------------------------------
dns_ms=0
if [ -n "$CURL" ]; then
    t=$("$CURL" -s -o /dev/null -m 3 -w '%{time_namelookup}' http://www.baidu.com 2>/dev/null)
    case "$t" in ''|*[!0-9.]*) dns_ms=0 ;; *) dns_ms=$(awk -v v="$t" 'BEGIN{printf "%d", v*1000}') ;; esac
fi
rtt=0; loss=100
if [ -n "$PING" ]; then
    p=$("$PING" -c 3 -W 2 223.5.5.5 2>/dev/null | tail -3)
    rtt=$(printf '%s' "$p" | sed -n 's#.*= [0-9.]*/\([0-9.]*\)/.*#\1#p' | head -n 1)
    loss=$(printf '%s' "$p" | sed -n 's/.*, \([0-9]*\)% packet loss.*/\1/p' | head -n 1)
    case "$rtt" in ''|*[!0-9.]*) rtt=0 ;; esac
    case "$loss" in ''|*[!0-9]*) loss=-1 ;; esac
    rtt=$(printf '%s' "$rtt" | cut -d. -f1)
fi

# ---- 无线：客户端数 + 两个频段各自的空口质量 ------------------------------
#
# 2.4G 的 txretrans 占比与 rxcrc 是"无线卡顿"最直接的证据：真机上实测
# eth6(2.4G) txretrans 407748/1377809 ≈ 29.6%、rxcrc 869779，而 5G 是 16.8%。
# 只记增量，避免被开机以来的累计值掩盖当下的变化。
wifi=0; wifi24=0; wifi5=0
w24_txr_total=0; w24_rxc_total=0; w5_txr_total=0; w5_rxc_total=0
if [ -x /usr/sbin/wl ] || [ -x /bin/wl ] || [ -x /usr/bin/wl ]; then
    WL=$(cmd_path wl)
    for i in eth6 eth7; do
        n=$("$WL" -i "$i" assoclist 2>/dev/null | wc -l | tr -d ' ')
        [ -n "$n" ] || n=0
        wifi=$((wifi + n))
        if [ "$i" = "eth6" ]; then wifi24=$n; else wifi5=$n; fi
        _cnt=$("$WL" -i "$i" counters 2>/dev/null)
        _txr=$(printf '%s' "$_cnt" | sed -n 's/.*txretrans \([0-9]*\).*/\1/p' | head -n 1)
        _rxc=$(printf '%s' "$_cnt" | sed -n 's/.*rxcrc \([0-9]*\).*/\1/p' | head -n 1)
        case "$_txr" in ''|*[!0-9]*) _txr=0 ;; esac
        case "$_rxc" in ''|*[!0-9]*) _rxc=0 ;; esac
        if [ "$i" = "eth6" ]; then
            w24_txr_total=$_txr; w24_rxc_total=$_rxc
        else
            w5_txr_total=$_txr; w5_rxc_total=$_rxc
        fi
    done
fi
w24_txr=$(delta "${w24_txr_total:-0}" "${prev_w24_txr:-0}")
w24_rxc=$(delta "${w24_rxc_total:-0}" "${prev_w24_rxc:-0}")
w5_txr=$(delta "${w5_txr_total:-0}" "${prev_w5_txr:-0}")
w5_rxc=$(delta "${w5_rxc_total:-0}" "${prev_w5_rxc:-0}")

# ---- 以太口 drop/err 增量（eth0=WAN，eth2 是真机上那个只协商到 100Mb 的口）----
eth0_drop=0; eth0_err=0; eth2_drop=0; eth2_err=0
if [ -r /proc/net/dev ]; then
    _dev() {
        awk -v d="$1" '$1 == d":" { print $4, $5 }' /proc/net/dev 2>/dev/null | head -n 1
    }
    # /proc/net/dev 的 RX 列顺序是 bytes packets **errs drop** —— 别取反了
    # （真机上第一版就写反了，日志里 drop 列记的是 errs）。
    _e0=$(_dev eth0); eth0_err=$(printf '%s' "$_e0" | cut -d' ' -f1); eth0_drop=$(printf '%s' "$_e0" | cut -d' ' -f2)
    _e2=$(_dev eth2); eth2_err=$(printf '%s' "$_e2" | cut -d' ' -f1); eth2_drop=$(printf '%s' "$_e2" | cut -d' ' -f2)
    case "$eth0_drop" in ''|*[!0-9]*) eth0_drop=0 ;; esac
    case "$eth0_err" in ''|*[!0-9]*) eth0_err=0 ;; esac
    case "$eth2_drop" in ''|*[!0-9]*) eth2_drop=0 ;; esac
    case "$eth2_err" in ''|*[!0-9]*) eth2_err=0 ;; esac
fi
e0_drop_d=$(delta "$eth0_drop" "${prev_eth0_drop:-0}")
e0_err_d=$(delta "$eth0_err" "${prev_eth0_err:-0}")
e2_drop_d=$(delta "$eth2_drop" "${prev_eth2_drop:-0}")
e2_err_d=$(delta "$eth2_err" "${prev_eth2_err:-0}")
link=""
if [ -x /usr/sbin/ethtool ] || [ -x /usr/bin/ethtool ]; then
    ETH=$(cmd_path ethtool)
    for p in eth0 eth1 eth2 eth3; do
        s=$("$ETH" "$p" 2>/dev/null | awk -F': ' '/Speed/ {print $2; exit}')
        case "$s" in ''|Unknown*) continue ;; esac
        link="${link}${p}=${s} "
    done
fi
link=$(printf '%s' "$link" | tr -d ' ')

# 表头只在首次运行时写。v2 相对 v1 的语义变化：
#   conns → new_conns（本周期新增，差值）+ conns_total（累计）
#   fc_hw 修好了（以前恒为 0）
#   新增隧道/会话池/无线/以太口增量列
# 分析脚本请按表头名取值，别按列号 —— 旧行（无 ver 列）仍然可读。
if [ ! -f "$PERF_LOG" ]; then
    echo "ts,ver,cpu_pct,new_conns,conns_total,fd,fd_limit,ct,fc_hw,fc_sw,d_up,d_dn,t_up,t_dn,ipset,gw,dns_ms,rtt_ms,loss_pct,wifi,wifi24,wifi5,wifi24_txretrans,wifi24_rxcrc,wifi5_txretrans,wifi5_rxcrc,dup_acks,tcp_dup_bytes,tun_wait_max_ms,txq_peak,tunnel_conn_fail,unknown_rst,stall_drops,pool_created,pool_reused,pool_idle,eth0_rx_drop_d,eth0_rx_err_d,eth2_rx_drop_d,eth2_rx_err_d,link" >>"$PERF_LOG"
fi
echo "$(date '+%Y-%m-%d %H:%M'),2,${cpu_pct},${new_conns},${conns},${fd_used},${fd_limit},${ct},${fc_hw},${fc_sw},${d_up},${d_dn},${t_up},${t_dn},${ipset},${split},${dns_ms},${rtt},${loss},${wifi},${wifi24},${wifi5},${w24_txr},${w24_rxc},${w5_txr},${w5_rxc},${dup_acks},${tcp_dup},${tun_wait_max},${txq_peak},${conn_fail},${unknown_rst},${stall_drops},${pool_created},${pool_reused},${pool_idle},${e0_drop_d},${e0_err_d},${e2_drop_d},${e2_err_d},${link}" >>"$PERF_LOG" 2>/dev/null

# 记住这一轮的原始计数，下一轮求增量
{
    echo "ticks=$ticks"
    echo "ts=$now"
    echo "conns=${conns:-0}"
    echo "eth0_drop=$eth0_drop"
    echo "eth0_err=$eth0_err"
    echo "eth2_drop=$eth2_drop"
    echo "eth2_err=$eth2_err"
    echo "w24_txr=$w24_txr_total"
    echo "w24_rxc=$w24_rxc_total"
    echo "w5_txr=$w5_txr_total"
    echo "w5_rxc=$w5_rxc_total"
} >"$STATE" 2>/dev/null

# 只保留最近 2000 行（约一周），别把 tmpfs 写满
lines=$(wc -l <"$PERF_LOG" 2>/dev/null | tr -d ' ')
if [ -n "$lines" ] && [ "$lines" -gt 2000 ] 2>/dev/null; then
    tail -n 2000 "$PERF_LOG" >"${PERF_LOG}.tmp" 2>/dev/null && cat "${PERF_LOG}.tmp" >"$PERF_LOG"
    rm -f "${PERF_LOG}.tmp" 2>/dev/null
fi
exit 0
