#!/bin/sh
# Phantom 夜间性能采样（每 5 分钟一行，落到 /tmp/upload/phantom_perf.log）
#
# 为什么需要它：故障只在"晚上人多"时出现，白天复现不了，而插件原本没有留下
# 任何可回看的数据（页面的 CPU 还恒为 0）。这里每分钟级别地记下判断瓶颈所需
# 的最小集合，第二天 `tail` 一下就能回答"到底是谁慢"：
#
#   cpu_pct  phantom 占整机 CPU 的百分比（多核合计）
#   conns    活跃 TCP 连接数
#   fd        phantom 进程的 fd 使用数/上限（EMFILE 会表现为"新连接打不开"）
#   ct       conntrack 条目数（每 LAN 连接会占两条）
#   fc_hw/sw flow cache 里走硬件加速的流数 / 全部流数（hw=0 说明完全没用上）
#   d_up/dn  直连（明文）字节累计     t_up/dn 隧道（加密）字节累计
#   ipset    内核分流白名单条目数
#   dns_ms   上游 DNS 解析耗时（路由器自身，直连）
#   rtt/loss 到公网的 RTT 与丢包（判断是不是 ISP 晚高峰）
#   wifi     关联的无线客户端数（判断是不是空口争用）
#   link     各以太口协商速率（100Mb 的口会拖慢挂在上面的设备）
#
# 表头只在首次运行时写入。日志本身由 phantom_config.sh 的 _trim 顺手裁剪。

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
if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
    ticks=$(proc_cpu_ticks "$pid")
    [ -n "$ticks" ] || ticks=0
    if [ -f "$STATE" ]; then
        prev=$(cat "$STATE" 2>/dev/null)
        prev_ticks=${prev%% *}
        prev_ts=${prev##* }
        case "$prev_ticks" in ''|*[!0-9]*) prev_ticks=0 ;; esac
        case "$prev_ts" in ''|*[!0-9]*) prev_ts=0 ;; esac
        dt=$((now - prev_ts))
        [ "$dt" -le 0 ] && dt=1
        dticks=$((ticks - prev_ticks))
        [ "$dticks" -lt 0 ] && dticks=0
        cpu_pct=$((dticks * 100 / (HZ * dt)))
    fi
    echo "$ticks $now" >"$STATE" 2>/dev/null
fi

# ---- metrics（连接数、字节分段、ipset 条目、生效模式）---------------------
conns=0; d_up=0; d_dn=0; t_up=0; t_dn=0; ipset=0; split=-1
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
    fi
fi

# ---- fd / conntrack ------------------------------------------------------
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
fc_total=0; fc_hw=0
if [ -f /proc/fcache/nflist ]; then
    set -- $(awk '
        /HW_TotHits/ { next }
        /^ *[0-9]+ / { total++; if ($0 !~ /4294967295/) hw++ }
        END { print total+0, hw+0 }
    ' /proc/fcache/nflist 2>/dev/null)
    fc_total=${1:-0}
    fc_hw=${2:-0}
fi

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

# ---- 无线客户端与有线口速率 ----------------------------------------------
wifi=0
if [ -x /usr/sbin/wl ] || [ -x /bin/wl ] || [ -x /usr/bin/wl ]; then
    WL=$(cmd_path wl)
    for i in eth6 eth7; do
        n=$("$WL" -i "$i" assoclist 2>/dev/null | wc -l | tr -d ' ')
        [ -n "$n" ] && wifi=$((wifi + n))
    done
fi
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

if [ ! -f "$PERF_LOG" ]; then
    echo "ts,cpu_pct,conns,fd,fd_limit,ct,fc_hw,fc_sw,d_up,d_dn,t_up,t_dn,ipset,gw,dns_ms,rtt_ms,loss_pct,wifi,link" >>"$PERF_LOG"
fi
echo "$(date '+%Y-%m-%d %H:%M'),${cpu_pct},${conns},${fd_used},${fd_limit},${ct},${fc_hw},$((fc_total - fc_hw)),${d_up},${d_dn},${t_up},${t_dn},${ipset},${split},${dns_ms},${rtt},${loss},${wifi},${link}" >>"$PERF_LOG" 2>/dev/null

# 只保留最近 2000 行（约一周），别把 tmpfs 写满
lines=$(wc -l <"$PERF_LOG" 2>/dev/null | tr -d ' ')
if [ -n "$lines" ] && [ "$lines" -gt 2000 ] 2>/dev/null; then
    tail -n 2000 "$PERF_LOG" >"${PERF_LOG}.tmp" 2>/dev/null && cat "${PERF_LOG}.tmp" >"$PERF_LOG"
    rm -f "${PERF_LOG}.tmp" 2>/dev/null
fi
exit 0
