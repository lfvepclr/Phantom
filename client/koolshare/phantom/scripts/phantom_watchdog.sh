#!/bin/sh
# Phantom 看门狗：由 cru 每 5 分钟调用一次
#
# 进程不在且插件处于启用状态时自动拉起，并把连续失败次数写进
# phantom_watchdog_fails，页面上可以看到「反复重启」这类异常。

module=phantom

for candidate in /koolshare/etc/phantom/phantom.env /jffs/phantom/phantom.env; do
    if [ -f "$candidate" ]; then
        . "$candidate"
        break
    fi
done

SCRIPTS_DIR="${PHANTOM_SCRIPTS_DIR:-/koolshare/scripts}"
RUNTIME_DIR="${PHANTOM_RUNTIME_DIR:-/koolshare/etc/phantom}"

# 见 phantom_config.sh：Asuswrt 上 /usr/sbin/sh 是 memaccess 的软链，不是 shell
if [ -x /bin/sh ]; then
    SH=/bin/sh
else
    SH=sh
fi

PIDFILE="${PHANTOM_PIDFILE:-/tmp/phantom.pid}"
LOG="${PHANTOM_LOG:-/tmp/upload/phantom_log.txt}"
CONFIG="${SCRIPTS_DIR}/${module}_config.sh"
USER_CONF="${RUNTIME_DIR}/etc/phantom.conf"

# cru 拉起时 /tmp/upload 一定存在（kscore 建过），这里只是兜底
[ -d "$(dirname "$LOG")" ] || mkdir -p "$(dirname "$LOG")" 2>/dev/null

conf_get() {
    [ -f "$USER_CONF" ] || return 0
    sed -n "s/^$1='\(.*\)'\$/\1/p" "$USER_CONF" 2>/dev/null | head -n 1
}

# BusyBox sed 与 BSD sed 的 -i 语法不兼容，统一走临时文件
sed_inplace() {
    sed "$1" "$2" >"$2.sedtmp" 2>/dev/null && mv "$2.sedtmp" "$2" || rm -f "$2.sedtmp"
}

conf_set() {
    key="$1"
    val=$(printf '%s' "$2" | tr -d "'\"\`")
    [ -f "$USER_CONF" ] || { touch "$USER_CONF"; chmod 600 "$USER_CONF"; }
    if grep -q "^${key}=" "$USER_CONF" 2>/dev/null; then
        sed_inplace "s|^${key}=.*|${key}='${val}'|" "$USER_CONF"
    else
        echo "${key}='${val}'" >>"$USER_CONF"
    fi
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

set_cfg() {
    if [ "$DBUS_OK" = "1" ]; then
        "$DBUS" set "${module}_$1=$2" >/dev/null 2>&1
    else
        conf_set "$1" "$2"
    fi
}

is_running() {
    [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null
}

# 插件处于「关闭」状态时也要看一眼残留：
# 进程僵死/崩溃后它装的 ip rule/iptables 不会自己消失，而规则指着 tun
# 就等于把全屋流量丢进黑洞（表现为断网，且开关是关的所以没人管）。
table=$(get_cfg table); [ -n "$table" ] || table=200

if [ "$(get_cfg enable)" != "1" ]; then
    if is_running || ip rule show 2>/dev/null | grep "lookup ${table}" >/dev/null 2>&1; then
        "$SH" "$CONFIG" stop >/dev/null 2>&1
        echo "【$(date '+%Y-%m-%d %H:%M:%S')】 看门狗：插件已关闭，清理残留进程/规则" >>"$LOG" 2>/dev/null
    fi
    exit 0
fi

if is_running; then
    set_cfg watchdog_fails 0

    # 进程在 ≠ 在工作：隧道卡死时进程仍留在 ps 里（kill -0 照样成功），
    # 但已经不再转发一个包，而规则还指着 tun。此时 start 会当成
    # 「已在运行」跳过，只有看门狗能救。
    health=$("$SH" "$CONFIG" _health 2>/dev/null | tail -n 1)
    if [ "$health" = "healthy" ]; then
        set_cfg watchdog_stale 0
        exit 0
    fi

    # 连续两次探测不到 metrics 才动手（避开刚启动、metrics 尚未起来的窗口）
    stale=$(get_cfg watchdog_stale)
    [ -n "$stale" ] || stale=0
    case "$stale" in ''|*[!0-9]*) stale=0 ;; esac
    stale=$((stale + 1))
    set_cfg watchdog_stale "$stale"
    if [ "$stale" -lt 2 ]; then
        exit 0
    fi

    echo "【$(date '+%Y-%m-%d %H:%M:%S')】 看门狗：进程僵死（metrics 无响应），强制重启" >>"$LOG" 2>/dev/null
    # stop 会无条件清理 ip rule/iptables（不能留着黑洞再接一个隧道）
    "$SH" "$CONFIG" stop >/dev/null 2>&1
    "$SH" "$CONFIG" start >/dev/null 2>&1

    if is_running; then
        set_cfg watchdog_stale 0
        set_cfg watchdog_fails 0
        echo "【$(date '+%Y-%m-%d %H:%M:%S')】 看门狗：僵死后重启成功" >>"$LOG" 2>/dev/null
    else
        echo "【$(date '+%Y-%m-%d %H:%M:%S')】 看门狗：僵死重启失败，请查看日志页" >>"$LOG" 2>/dev/null
    fi
    exit 0
fi

fails=$(get_cfg watchdog_fails)
[ -n "$fails" ] || fails=0
case "$fails" in ''|*[!0-9]*) fails=0 ;; esac
fails=$((fails + 1))
set_cfg watchdog_fails "$fails"

echo "【$(date '+%Y-%m-%d %H:%M:%S')】 看门狗：检测到服务未运行，尝试拉起（第 ${fails} 次）" >>"$LOG" 2>/dev/null
"$SH" "$CONFIG" start >/dev/null 2>&1

if is_running; then
    set_cfg watchdog_fails 0
    echo "【$(date '+%Y-%m-%d %H:%M:%S')】 看门狗：拉起成功" >>"$LOG" 2>/dev/null
else
    echo "【$(date '+%Y-%m-%d %H:%M:%S')】 看门狗：拉起失败，请查看上方日志" >>"$LOG" 2>/dev/null
fi

exit 0
