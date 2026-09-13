#!/bin/sh
# Phantom 一键诊断：把排障需要的信息一次性打出来
#
# 用法（注意用 /bin/sh 绝对路径：Asuswrt 的 /usr/sbin/sh 是 memaccess，不是 shell）：
#   /bin/sh /koolshare/scripts/phantom_diag.sh                 # 直接看
#   /bin/sh /koolshare/scripts/phantom_diag.sh > /tmp/diag.txt # 回传到开发机
#   ssh -p <SSH端口> admin@<路由器IP> '/bin/sh /koolshare/scripts/phantom_config.sh diag' > /tmp/phantom_diag.txt
#
# 输出默认脱敏：连接串的公钥与 PSK 会被替换成 ***，白名单只报条数。

module=phantom

for candidate in /koolshare/etc/phantom/phantom.env /jffs/phantom/phantom.env; do
    if [ -f "$candidate" ]; then
        . "$candidate"
        break
    fi
done

SCRIPTS_DIR="${PHANTOM_SCRIPTS_DIR:-/koolshare/scripts}"
BIN_DIR="${PHANTOM_BIN_DIR:-/koolshare/bin}"
RUNTIME_DIR="${PHANTOM_RUNTIME_DIR:-/koolshare/etc/phantom}"
PIDFILE="${PHANTOM_PIDFILE:-/tmp/phantom.pid}"
LOG="${PHANTOM_LOG:-/tmp/upload/phantom_log.txt}"
USER_CONF="${RUNTIME_DIR}/etc/phantom.conf"
STATUS_JSON="${PHANTOM_STATUS:-/tmp/upload/phantom_status.txt}"

section() {
    echo ""
    echo "===== $1 ====="
}

mask_uri() {
    printf '%s' "$1" | sed 's|^\(phantom://\)[^@]*@|\1***@|; s|\(psk=\)[^&]*|\1***|'
}

conf_get() {
    [ -f "$USER_CONF" ] || return 0
    sed -n "s/^$1='\(.*\)'\$/\1/p" "$USER_CONF" 2>/dev/null | head -n 1
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

DBUS="${PHANTOM_DBUS:-}"
[ -n "$DBUS" ] && [ -x "$DBUS" ] || DBUS=$(cmd_path dbus) || DBUS=dbus
CRU="${PHANTOM_CRU:-}"
[ -n "$CRU" ] && [ -x "$CRU" ] || CRU=$(cmd_path cru)
CURL="${PHANTOM_CURL:-}"
[ -n "$CURL" ] && [ -x "$CURL" ] || CURL=$(cmd_path curl) || CURL=curl

DBUS_OK=0
if [ "${PHANTOM_KS:-1}" = "1" ] && "$DBUS" get ${module}_version >/dev/null 2>&1; then
    DBUS_OK=1
fi

get_cfg() {
    if [ "$DBUS_OK" = "1" ]; then
        "$DBUS" get "${module}_$1" 2>/dev/null
    else
        conf_get "$1"
    fi
}

echo "Phantom 诊断报告 $(date '+%Y-%m-%d %H:%M:%S')"

section "固件与内核"
echo "productid : $(nvram get productid 2>/dev/null)"
echo "odmpid    : $(nvram get odmpid 2>/dev/null)"
echo "extendno  : $(nvram get extendno 2>/dev/null)"
echo "buildno   : $(nvram get buildno 2>/dev/null)"
echo "uname     : $(uname -a 2>/dev/null)"
echo "软件中心   : $([ -d /koolshare ] && echo present || echo absent)  skipd: $([ -f /usr/bin/skipd ] && echo yes || echo no)"
echo "安装模式   : $([ "${PHANTOM_KS:-1}" = "1" ] && echo koolshare || echo jffs)  UI: ${PHANTOM_UI:-unknown}"

section "二进制"
if [ -x "${BIN_DIR}/phantom" ]; then
    echo "路径     : ${BIN_DIR}/phantom"
    echo "大小     : $(ls -l "${BIN_DIR}/phantom" 2>/dev/null | awk '{print $5}') bytes"
    echo "版本     : $("${BIN_DIR}/phantom" --version 2>&1 | head -n 1)"
else
    echo "错误：${BIN_DIR}/phantom 不存在或不可执行"
fi

section "配置（已脱敏）"
for key in enable mode protocol lan_if tun_name tun_addr table dns_hijack builtin_wl \
           cron_enable cron_time watchdog log_level watchdog_fails last_act speed_last; do
    val=$(get_cfg "$key")
    echo "${key} = ${val}"
done
echo "uri      = $(mask_uri "$(get_cfg uri)")"
wl_count=0
if [ -f "${RUNTIME_DIR}/etc/proxy_domains.txt" ]; then
    wl_count=$(grep -c . "${RUNTIME_DIR}/etc/proxy_domains.txt" 2>/dev/null | tr -d ' ')
fi
echo "whitelist= ${wl_count} 条（内容不输出，避免泄露）"

section "进程"
if [ -f "$PIDFILE" ]; then
    pid=$(cat "$PIDFILE" 2>/dev/null)
    echo "pidfile  : $pid"
    if [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null; then
        echo "状态     : running"
        ps -o pid,pcpu,pmem,etime,comm -p "$pid" 2>/dev/null
    else
        echo "状态     : 进程不存在"
    fi
else
    echo "状态     : 无 pidfile（未运行）"
fi
ps 2>/dev/null | grep -i "[p]hantom" || echo "(无其它 phantom 进程)"

section "网络：TUN 与策略路由"
ip link show "$(get_cfg tun_name | sed 's/^$/phantom0/')" 2>/dev/null | head -n 5
echo "--- ip rule ---"
ip rule show 2>/dev/null | grep -E "lookup (main|$(get_cfg table | sed 's/^$/200/'))"
echo "--- 路由表 ---"
ip route show table "$(get_cfg table | sed 's/^$/200/')" 2>/dev/null
echo "--- 接口收发（TUN）---"
grep -E "phantom|tun" /proc/net/dev 2>/dev/null

section "iptables（摘要）"
echo "--- nat PREROUTING ---"
iptables -t nat -S PREROUTING 2>/dev/null | grep -i phantom
echo "--- filter FORWARD ---"
iptables -S FORWARD 2>/dev/null | grep -i phantom

section "metrics 采样（间隔 3 秒）"
if [ -f "$STATUS_JSON" ]; then
    echo "状态文件 : $(cat "$STATUS_JSON" 2>/dev/null)"
else
    echo "(状态文件不存在：采样循环未运行)"
fi
s1=$("$CURL" -s --max-time 2 http://127.0.0.1:9150/metrics 2>/dev/null | grep '^phantom')
if [ -n "$s1" ]; then
    sleep 3
    echo "--- 第一次 ---"
    printf '%s\n' "$s1"
    echo "--- 第二次 ---"
    "$CURL" -s --max-time 2 http://127.0.0.1:9150/metrics 2>/dev/null | grep '^phantom'
else
    echo "(metrics 不可用：隧道未运行或 metrics_listen 被占用)"
fi

section "定时任务"
if [ -n "$CRU" ]; then
    "$CRU" l 2>/dev/null | grep phantom || echo "(无 phantom 定时任务)"
else
    crontab -l 2>/dev/null | grep phantom || echo "(无 phantom 定时任务)"
fi

section "磁盘"
df -h /koolshare /jffs /tmp 2>/dev/null

section "最近日志（200 行）"
tail -n 200 "$LOG" 2>/dev/null || echo "(日志不存在)"

echo ""
echo "===== 诊断结束 ====="
exit 0
