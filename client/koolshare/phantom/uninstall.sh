#!/bin/sh
# Phantom koolshare 软件中心插件 —— 卸载脚本
#
# 软件中心把它复制为 /koolshare/scripts/uninstall_phantom.sh 后调用。
# 手工卸载：/bin/sh /koolshare/scripts/uninstall_phantom.sh
# 降级模式：/bin/sh /jffs/phantom/uninstall.sh

module=phantom

echo_date() {
    echo "【$(date '+%Y-%m-%d %H:%M:%S')】 $*"
}

# 见 install.sh：BSD sed 与 BusyBox sed 的 -i 语法不兼容，统一走临时文件
sed_inplace() {
    sed "$1" "$2" >"$2.sedtmp" 2>/dev/null && mv "$2.sedtmp" "$2" || rm -f "$2.sedtmp"
}

# 见 phantom_config.sh：Asuswrt 上 /usr/sbin/sh 是 memaccess 的软链，不是 shell
if [ -x /bin/sh ]; then
    SH=/bin/sh
else
    SH=sh
fi

# 按绝对路径找 dbus：本固件的 busybox 没有 `command -v`/`type`
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

# 定位安装布局：优先软件中心，其次降级目录
for candidate in /koolshare/etc/phantom/phantom.env /jffs/phantom/phantom.env; do
    if [ -f "$candidate" ]; then
        . "$candidate"
        PHANTOM_ENV="$candidate"
        break
    fi
done

SCRIPTS_DIR="${PHANTOM_SCRIPTS_DIR:-/koolshare/scripts}"
RUNTIME_DIR="${PHANTOM_RUNTIME_DIR:-/koolshare/etc/phantom}"
BIN_DIR="${PHANTOM_BIN_DIR:-/koolshare/bin}"
KS_MODE="${PHANTOM_KS:-1}"

echo_date "停止 Phantom 服务..."
if [ -f "${SCRIPTS_DIR}/${module}_config.sh" ]; then
    "$SH" "${SCRIPTS_DIR}/${module}_config.sh" stop >/dev/null 2>&1
fi

echo_date "清理定时任务..."
if [ -f "${SCRIPTS_DIR}/${module}_cron.sh" ]; then
    "$SH" "${SCRIPTS_DIR}/${module}_cron.sh" clear >/dev/null 2>&1
fi

echo_date "删除插件文件..."
rm -f "${SCRIPTS_DIR}/${module}_config.sh" \
      "${SCRIPTS_DIR}/${module}_status.sh" \
      "${SCRIPTS_DIR}/${module}_speedtest.sh" \
      "${SCRIPTS_DIR}/${module}_cron.sh" \
      "${SCRIPTS_DIR}/${module}_diag.sh" \
      "${SCRIPTS_DIR}/${module}_watchdog.sh" >/dev/null 2>&1
rm -f "${BIN_DIR}/phantom" >/dev/null 2>&1
rm -f /koolshare/webs/Module_${module}.asp >/dev/null 2>&1
rm -f /koolshare/res/phantom.css /koolshare/res/icon-phantom.png >/dev/null 2>&1
rm -f /koolshare/init.d/S98${module}.sh /koolshare/init.d/N98${module}.sh >/dev/null 2>&1
# 运行期文件在 tmpfs（/tmp/upload 是页面的文本通道）；顺带清历史版本留下的
# /tmp 直放文件与 docroot / /www/_temp 死软链
rm -f /tmp/upload/phantom_log.txt /tmp/upload/phantom_status.txt >/dev/null 2>&1
rm -f /tmp/phantom_log.txt /tmp/phantom_status.txt /tmp/phantom.pid /tmp/phantom_status.pid >/dev/null 2>&1
rm -f "/koolshare/webs/phantom_status.txt" "/koolshare/webs/phantom_log.txt" >/dev/null 2>&1
rm -f /www/_temp/phantom_log.txt /www/_temp/phantom_status.txt >/dev/null 2>&1

if [ "${KS_MODE}" = "0" ]; then
    # 降级模式：清掉 /jffs/scripts 里的钩子行
    for hook in /jffs/scripts/services-start /jffs/scripts/nat-start; do
        [ -f "$hook" ] && sed_inplace '/phantom_config\.sh/d' "$hook"
    done
    rm -rf /jffs/phantom >/dev/null 2>&1
    rm -rf "${RUNTIME_DIR}" >/dev/null 2>&1
    echo_date "/jffs 降级安装已清理"
else
    rm -rf "${RUNTIME_DIR}" >/dev/null 2>&1
    echo_date "清理 dbus 配置..."
    for key in enable uri mode protocol lan_if tun_name tun_addr table dns_hijack \
               builtin_wl whitelist cron_enable cron_time watchdog log_level \
               last_act status rate_up rate_down total_up total_down conns \
               route_direct route_proxy speed_last diag_file \
               server_up_mbps server_down_mbps watchdog_fails; do
        "$DBUS" remove ${module}_${key} >/dev/null 2>&1
    done
    "$DBUS" remove softcenter_module_${module}_version >/dev/null 2>&1
    "$DBUS" remove softcenter_module_${module}_install >/dev/null 2>&1
    "$DBUS" remove softcenter_module_${module}_name >/dev/null 2>&1
    "$DBUS" remove softcenter_module_${module}_title >/dev/null 2>&1
    "$DBUS" remove softcenter_module_${module}_description >/dev/null 2>&1
fi

rm -rf /tmp/${module}* >/dev/null 2>&1
echo_date "Phantom 插件已卸载！"
exit 0
