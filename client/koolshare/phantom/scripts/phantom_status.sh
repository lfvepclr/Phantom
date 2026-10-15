#!/bin/sh
# Phantom 状态采样循环（每 2 秒抓一次 /metrics，与上次快照求差得速率）
#
# 由 phantom_config.sh 在隧道启动后拉起，隧道停止时被 kill。
# 输出写到 $PHANTOM_STATUS（默认 /tmp/upload/phantom_status.txt，内容为 JSON）；
# 页面经 httpdb 的 /_temp/phantom_status.txt 读取（/_temp/ 映射到 /tmp/upload）。
# 注意不能放 docroot：本固件 httpd 不服务 .txt，软链进 /koolshare/webs 是 404。
#
# 为什么不用「页面轮询触发后台脚本」：每次 POST /_api/ 都会 spawn 一次
# shell，2 秒一次对路由器太重。常驻循环的开销只有一次本地 curl。

module=phantom

for candidate in /koolshare/etc/phantom/phantom.env /jffs/phantom/phantom.env; do
    if [ -f "$candidate" ]; then
        . "$candidate"
        break
    fi
done

RUNTIME_DIR="${PHANTOM_RUNTIME_DIR:-/koolshare/etc/phantom}"
STATUS_JSON="${PHANTOM_STATUS:-/tmp/upload/phantom_status.txt}"
PIDFILE="${PHANTOM_PIDFILE:-/tmp/phantom.pid}"
STATUS_PIDFILE="${PHANTOM_STATUS_PIDFILE:-/tmp/phantom_status.pid}"
METRICS="http://127.0.0.1:9150/metrics"
INTERVAL=3

prev_up=0
prev_down=0
prev_udp_up=0
prev_udp_down=0
prev_ts=0
prev_ticks=0
first=1

# 本机 busybox 的 ps 没有 `-o`（`ps: invalid option -- 'o'`），所以页面上的
# CPU 一直是 0 —— 夜里真出问题时反而看不到证据。直接读 /proc/<pid>/stat 的
# utime+stime 自己算差值。
HZ=$(getconf CLK_TCK 2>/dev/null)
case "$HZ" in ''|*[!0-9]*) HZ=100 ;; esac

proc_cpu_ticks() {
    _line=$(cat "/proc/$1/stat" 2>/dev/null) || return 0
    _rest=${_line##*') '}
    # 去掉 "pid (comm)" 之后，第 12/13 个字段就是 utime/stime（stat 的 14/15）。
    # shellcheck disable=SC2086
    set -- $_rest
    echo $(( ${12:-0} + ${13:-0} ))
}

metric() {
    printf '%s' "$1" | sed -n "s/^$2 \([0-9]\{1,\}\)\$/\1/p" | head -n 1
}

# 按绝对路径找外部命令（本固件的 busybox 没有 `command -v`/`type`）
PHANTOM_BIN_DIRS="${PHANTOM_BIN_DIRS:-/bin /sbin /usr/bin /usr/sbin /koolshare/bin /opt/bin /usr/local/bin}"

cmd_path() {
    _c="$1"
    _dirs=$(printf "%s" "$PHANTOM_BIN_DIRS" | tr ":" " ")
    case "$_c" in
        /*) [ -x "$_c" ] && { printf '%s' "$_c"; return 0; } ;;
    esac
    # 搜索目录可用 PHANTOM_BIN_DIRS 覆盖（测试时把 mock 目录放最前）
    for _d in $_dirs; do
        if [ -x "$_d/$_c" ]; then
            printf '%s' "$_d/$_c"
            return 0
        fi
    done
    return 1
}

CURL="${PHANTOM_CURL:-}"
[ -n "$CURL" ] && [ -x "$CURL" ] || CURL=$(cmd_path curl)
WGET="${PHANTOM_WGET:-}"
[ -n "$WGET" ] && [ -x "$WGET" ] || WGET=$(cmd_path wget)

# 「上次动作」随状态文件一起带给页面。
#
# 页面原来每 5 秒单独轮询 httpdb 的 /_api/phantom_last_act —— 那是每轮多一次
# 走 httpdb 的请求，而本固件的 httpd 碰上「失效会话 + httpdb 请求」会直接
# SIGSEGV（真机 syslog 里 130 多次 Comm: httpd 崩溃）。少一个通道就少一次
# 把路由器 Web 服务打崩的机会，所以并进状态文件。/_api/phantom_last_act 与
# dbus 键都保留，SSH / 其它工具照旧可用。
DBUS="${PHANTOM_DBUS:-}"
[ -n "$DBUS" ] && [ -x "$DBUS" ] || DBUS=$(cmd_path dbus)
USE_DBUS=0
if [ "${PHANTOM_KS:-1}" = "1" ] && [ -n "$DBUS" ] \
   && "$DBUS" get ${module}_version >/dev/null 2>&1; then
    USE_DBUS=1
fi

read_last_act() {
    if [ "$USE_DBUS" = "1" ]; then
        "$DBUS" get ${module}_last_act 2>/dev/null
    else
        sed -n "s/^last_act='\(.*\)'\$/\1/p" "${RUNTIME_DIR}/etc/phantom.conf" 2>/dev/null | head -n 1
    fi
}

# 当前实例由谁拉起（boot/cron/nat/watchdog/user/install）。
# 页面拿它解释「这次是自己开的还是被谁开的」，与 last_act 里的标签同源。
read_run_trigger() {
    if [ "$USE_DBUS" = "1" ]; then
        "$DBUS" get ${module}_run_trigger 2>/dev/null
    else
        sed -n "s/^run_trigger='\(.*\)'\$/\1/p" "${RUNTIME_DIR}/etc/phantom.conf" 2>/dev/null | head -n 1
    fi
}

# 转发面规则是否在位（ip rule + iptables FORWARD 的两条 ACCEPT）。
#
# 为什么页面需要它：进程活着、metrics 能读，**不等于** LAN 流量真的在走隧道 ——
# Asuswrt 重建 iptables 之后规则会没了，而进程还在跑。只有把「实际」摊开给用户看，
# 「开关到底是开还是关」才不再靠猜。
#
# 每 15 秒才查一次（ip/iptables 各一次 fork，2 秒一次的采样循环里不能每次都做）。
RULES_STATE=0
check_rules() {
    _lan=$(printf '%s' "${STATUS_LAN_IF:-br0}" | cut -d' ' -f1)
    _tun="${STATUS_TUN_NAME:-phantom0}"
    _table="${STATUS_TABLE:-200}"
    ip rule show 2>/dev/null | grep -qE "lookup[[:space:]]+${_table}([[:space:]]|$)" || return 1
    iptables -S FORWARD 2>/dev/null | grep -q -- "-i ${_lan} -o ${_tun} " || return 1
    iptables -S FORWARD 2>/dev/null | grep -q -- "-i ${_tun} -o ${_lan} " || return 1
    return 0
}

# 采样进程只在「隧道正在跑」时存在，所以这些值读一次就够
# （改 LAN 接口/表号会重启隧道，采样进程跟着重启）。
get_cfg_lite() {
    if [ "$USE_DBUS" = "1" ]; then
        "$DBUS" get "${module}_$1" 2>/dev/null
    else
        sed -n "s/^$1='\(.*\)'\$/\1/p" "${RUNTIME_DIR}/etc/phantom.conf" 2>/dev/null | head -n 1
    fi
}
STATUS_LAN_IF=$(get_cfg_lite lan_if); [ -n "$STATUS_LAN_IF" ] || STATUS_LAN_IF="br0"
STATUS_TUN_NAME=$(get_cfg_lite tun_name); [ -n "$STATUS_TUN_NAME" ] || STATUS_TUN_NAME="phantom0"
STATUS_TABLE=$(get_cfg_lite table); [ -n "$STATUS_TABLE" ] || STATUS_TABLE="200"

# curl 优先，没有就退到 busybox wget（部分精简固件只带 wget）
fetch() {
    if [ -n "$CURL" ]; then
        "$CURL" -s --max-time 2 "$1" 2>/dev/null
    elif [ -n "$WGET" ]; then
        "$WGET" -q -O - -T 2 "$1" 2>/dev/null
    fi
}

write_empty() {
    # 即使 metrics 读不到，也要把「进程还在不在、规则还在不在」照实写出来：
    # 页面正是靠这三者区分「已停止」和「进程/规则残留」的。
    _ep=$(cat "$PIDFILE" 2>/dev/null)
    case "$_ep" in ''|*[!0-9]*) _ep=0 ;; esac
    _tr=$(read_run_trigger | tr -d '"\\')
    cat >"${STATUS_JSON}.tmp" 2>/dev/null <<EOF
{"ts":$(date +%s),"running":0,"up_rate":0,"down_rate":0,"total_up":0,"total_down":0,"udp_up":0,"udp_down":0,"conns":0,"direct":0,"proxy":0,"cpu":0,"gw":"-","ipset":0,"fd":0,"fdl":0,"rules":${RULES_STATE},"proc":${_ep},"trigger":"${_tr}","last_act":"$(read_last_act | tr -d '\\"')"}
EOF
    mv "${STATUS_JSON}.tmp" "${STATUS_JSON}" 2>/dev/null
    chmod 644 "${STATUS_JSON}" 2>/dev/null
}

tick=0
while :; do
    # 隧道进程没了就退出（config.sh 也会 kill，这里是双保险）
    if [ ! -f "$PIDFILE" ]; then
        break
    fi
    pid=$(cat "$PIDFILE" 2>/dev/null)
    if [ -z "$pid" ] || ! kill -0 "$pid" 2>/dev/null; then
        break
    fi

    now=$(date +%s)
    # 规则在位检查要 fork 两次（ip / iptables），2 秒一轮太浪费，
    # 每 5 轮（约 15s）查一次即可 —— 页面上的「实际」标签不需要秒级精度。
    tick=$((tick + 1))
    if [ $((tick % 5)) -eq 1 ] || [ "$tick" -eq 1 ]; then
        if check_rules; then RULES_STATE=1; else RULES_STATE=0; fi
    fi
    trigger_val=$(read_run_trigger | tr -d '"\\')
    body=$(fetch "$METRICS")

    if [ -n "$body" ]; then
        total_up=$(metric "$body" phantom_tcp_bytes_up)
        total_down=$(metric "$body" phantom_tcp_bytes_down)
        udp_up=$(metric "$body" phantom_udp_bytes_up)
        udp_down=$(metric "$body" phantom_udp_bytes_down)
        conns=$(metric "$body" phantom_tcp_connections)
        direct=$(metric "$body" phantom_route_direct_total)
        proxy=$(metric "$body" phantom_route_proxy_total)
        [ -n "$total_up" ] || total_up=0
        [ -n "$total_down" ] || total_down=0
        [ -n "$udp_up" ] || udp_up=0
        [ -n "$udp_down" ] || udp_down=0
        [ -n "$conns" ] || conns=0
        [ -n "$direct" ] || direct=0
        [ -n "$proxy" ] || proxy=0

        ipset_entries=$(metric "$body" phantom_whitelist_ipset_entries)
        [ -n "$ipset_entries" ] || ipset_entries=0
        kernel_split=$(metric "$body" phantom_gateway_kernel_split)
        case "$kernel_split" in
            1) gw="kernel-split" ;;
            0) gw="relay" ;;
            *) gw="-" ;;
        esac

        fd_used=$(ls /proc/"$pid"/fd 2>/dev/null | wc -l | tr -d ' ')
        [ -n "$fd_used" ] || fd_used=0
        fd_limit=$(awk '/Max open files/ {print $4}' /proc/"$pid"/limits 2>/dev/null | head -n 1)
        [ -n "$fd_limit" ] || fd_limit=0

        ticks=$(proc_cpu_ticks "$pid")
        [ -n "$ticks" ] || ticks=0

        if [ "$first" = "1" ]; then
            up_rate=0
            down_rate=0
            cpu=0
            first=0
        else
            dt=$((now - prev_ts))
            [ "$dt" -le 0 ] && dt=1
            du=$((total_up - prev_up))
            dd=$((total_down - prev_down))
            [ "$du" -lt 0 ] && du=0
            [ "$dd" -lt 0 ] && dd=0
            up_rate=$((du / dt))
            down_rate=$((dd / dt))
            dticks=$((ticks - prev_ticks))
            [ "$dticks" -lt 0 ] && dticks=0
            # 百分比按整机（多核）算：ticks/HZ 秒的 CPU 时间占 dt 秒的比例。
            cpu=$((dticks * 100 / (HZ * dt)))
        fi

        prev_up=$total_up
        prev_down=$total_down
        prev_udp_up=$udp_up
        prev_udp_down=$udp_down
        prev_ts=$now
        prev_ticks=$ticks

        cat >"${STATUS_JSON}.tmp" 2>/dev/null <<EOF
{"ts":${now},"running":1,"up_rate":${up_rate},"down_rate":${down_rate},"total_up":${total_up},"total_down":${total_down},"udp_up":${udp_up},"udp_down":${udp_down},"conns":${conns},"direct":${direct},"proxy":${proxy},"cpu":${cpu},"gw":"${gw}","ipset":${ipset_entries},"fd":${fd_used},"fdl":${fd_limit},"rules":${RULES_STATE},"proc":${pid},"trigger":"${trigger_val}","last_act":"$(read_last_act | tr -d '\\"')"}
EOF
        mv "${STATUS_JSON}.tmp" "${STATUS_JSON}" 2>/dev/null
        chmod 644 "${STATUS_JSON}" 2>/dev/null
    else
        write_empty
    fi

    sleep $INTERVAL 2>/dev/null || sleep 2
done

write_empty
rm -f "$STATUS_PIDFILE" 2>/dev/null
exit 0
