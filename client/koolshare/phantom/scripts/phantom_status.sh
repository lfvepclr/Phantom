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
INTERVAL=2

prev_up=0
prev_down=0
prev_udp_up=0
prev_udp_down=0
prev_ts=0
first=1

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

# curl 优先，没有就退到 busybox wget（部分精简固件只带 wget）
fetch() {
    if [ -n "$CURL" ]; then
        "$CURL" -s --max-time 2 "$1" 2>/dev/null
    elif [ -n "$WGET" ]; then
        "$WGET" -q -O - -T 2 "$1" 2>/dev/null
    fi
}

write_empty() {
    cat >"${STATUS_JSON}.tmp" 2>/dev/null <<EOF
{"ts":$(date +%s),"running":0,"up_rate":0,"down_rate":0,"total_up":0,"total_down":0,"udp_up":0,"udp_down":0,"conns":0,"direct":0,"proxy":0,"cpu":0}
EOF
    mv "${STATUS_JSON}.tmp" "${STATUS_JSON}" 2>/dev/null
    chmod 644 "${STATUS_JSON}" 2>/dev/null
}

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

        cpu=$(ps -o pcpu= -p "$pid" 2>/dev/null | tr -d ' ')
        [ -n "$cpu" ] || cpu=0
        cpu=$(printf '%s' "$cpu" | cut -d. -f1)
        [ -n "$cpu" ] || cpu=0

        if [ "$first" = "1" ]; then
            up_rate=0
            down_rate=0
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
        fi

        prev_up=$total_up
        prev_down=$total_down
        prev_udp_up=$udp_up
        prev_udp_down=$udp_down
        prev_ts=$now

        cat >"${STATUS_JSON}.tmp" 2>/dev/null <<EOF
{"ts":${now},"running":1,"up_rate":${up_rate},"down_rate":${down_rate},"total_up":${total_up},"total_down":${total_down},"udp_up":${udp_up},"udp_down":${udp_down},"conns":${conns},"direct":${direct},"proxy":${proxy},"cpu":${cpu}}
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
