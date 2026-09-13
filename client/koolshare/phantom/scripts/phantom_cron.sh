#!/bin/sh
# Phantom 定时任务注册（定时重启 + 看门狗）
#
# 用法：/bin/sh phantom_cron.sh {sync|clear}
#   sync  按 dbus 配置幂等注册（每天重复调用无副作用）
#   clear 删除全部 phantom 定时任务
#
# cru 是 Asuswrt/Merlin 的 crontab 封装，优先使用；没有则回退 crontab。

module=phantom

for candidate in /koolshare/etc/phantom/phantom.env /jffs/phantom/phantom.env; do
    if [ -f "$candidate" ]; then
        . "$candidate"
        break
    fi
done

SCRIPTS_DIR="${PHANTOM_SCRIPTS_DIR:-/koolshare/scripts}"
RUNTIME_DIR="${PHANTOM_RUNTIME_DIR:-/koolshare/etc/phantom}"
USER_CONF="${RUNTIME_DIR}/etc/phantom.conf"
CONFIG="${SCRIPTS_DIR}/${module}_config.sh"
WATCHDOG="${SCRIPTS_DIR}/${module}_watchdog.sh"

# 见 phantom_config.sh：Asuswrt 上 /usr/sbin/sh 是 memaccess 的软链，不是 shell。
# cron 的 PATH 同样把 /usr/sbin 排在前面，所以写进 crontab 的命令也要用绝对路径。
if [ -x /bin/sh ]; then
    SH=/bin/sh
else
    SH=sh
fi

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

have_cru() {
    [ -n "$CRU" ]
}

cron_add() {
    id="$1"
    sched="$2"
    cmd="$3"
    if have_cru; then
        "$CRU" d "$id" >/dev/null 2>&1
        "$CRU" a "$id" "${sched} ${cmd}" >/dev/null 2>&1
        return 0
    fi
    # 回退：直接改 crontab（保留其它条目）
    tmp="/tmp/phantom_cron.$$"
    crontab -l 2>/dev/null | grep -v "phantom_" >"$tmp"
    printf '%s %s\n' "$sched" "$cmd" >>"$tmp"
    crontab "$tmp" 2>/dev/null
    rm -f "$tmp"
    return 0
}

cron_del() {
    id="$1"
    if have_cru; then
        "$CRU" d "$id" >/dev/null 2>&1
        return 0
    fi
    tmp="/tmp/phantom_cron.$$"
    crontab -l 2>/dev/null | grep -v "phantom_" >"$tmp"
    crontab "$tmp" 2>/dev/null
    rm -f "$tmp"
    return 0
}

clear_all() {
    cron_del phantom_restart
    cron_del phantom_watchdog
    echo "已清理 phantom 定时任务"
}

sync_all() {
    # 看门狗：每 5 分钟检查一次，进程不在且已启用则拉起
    watchdog=$(get_cfg watchdog)
    [ -n "$watchdog" ] || watchdog="1"
    if [ "$watchdog" = "1" ] && [ -f "$WATCHDOG" ]; then
        cron_add phantom_watchdog "*/5 * * * *" "${SH} ${WATCHDOG}"
    else
        cron_del phantom_watchdog
    fi

    # 定时重启：cron_time 形如 4:30 或 4:30:1（第三个字段是周几，0=周日）
    cron_enable=$(get_cfg cron_enable)
    if [ "$cron_enable" = "1" ]; then
        cron_time=$(get_cfg cron_time)
        [ -n "$cron_time" ] || cron_time="4:30"
        hour=$(printf '%s' "$cron_time" | cut -d: -f1)
        minute=$(printf '%s' "$cron_time" | cut -d: -f2)
        dow=$(printf '%s' "$cron_time" | cut -d: -f3)
        [ -n "$hour" ] || hour="4"
        [ -n "$minute" ] || minute="30"
        [ -n "$dow" ] && [ "$dow" != "$cron_time" ] || dow="*"
        cron_add phantom_restart "${minute} ${hour} * * ${dow}" "${SH} ${CONFIG} restart"
        echo "已注册定时重启：每天 ${hour}:${minute}（周字段 ${dow}）"
    else
        cron_del phantom_restart
    fi
    return 0
}

case "$1" in
    sync)  sync_all ;;
    clear) clear_all ;;
    list)
        if have_cru; then
            "$CRU" l 2>/dev/null | grep phantom
        else
            crontab -l 2>/dev/null | grep phantom
        fi
        ;;
    *)
        echo "Usage: $0 {sync|clear|list}"
        exit 1
        ;;
esac
exit 0
