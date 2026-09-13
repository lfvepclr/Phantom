#!/bin/sh
# Phantom koolshare 软件中心插件 —— 安装脚本
#
# 软件中心把离线包解压到 /tmp/phantom/ 后运行本脚本（module=phantom）。
# 也可以手工执行：
#   scp phantom.tar.gz admin@<路由器IP>:/tmp/
#   ssh admin@<路由器IP> 'tar xzf /tmp/phantom.tar.gz -C /tmp && sh /tmp/phantom/install.sh'
#
# 两种安装形态：
#   1) 软件中心模式（koolshare 官改 / ks梅林）：装到 /koolshare，dbus 存配置
#   2) 降级模式（无软件中心的梅林/官方固件）：装到 /jffs/phantom，配置文件存配置
#
# POSIX sh only：路由器的 shell 是 BusyBox ash，不是 bash。

DIR=$(cd "$(dirname "$0")"; pwd)
module=phantom
TITLE="Phantom 透明网关"
DESCR="Phantom 加密隧道透明网关"

# 不要用裸 `sh`：Asuswrt 上 /usr/sbin/sh 是指向 /bin/memaccess 的软链
# （Broadcom 的 dw/dh/db/sw/sh/sb 调试工具家族），而 sshd/cron 的非交互
# PATH 把 /usr/sbin 排在 /bin 前面，`sh xxx.sh` 会跑到 memaccess 上。
if [ -x /bin/sh ]; then
    SH=/bin/sh
else
    SH=sh
fi

# 运行期文件（页面要读的状态与日志）统一放 tmpfs，且必须在 /tmp/upload/：
#   * 本固件 httpd **不服务 docroot 下的 .txt**（/phantom_status.txt 与固件自带的
#     /Lang_Hdr.txt 一样 404），软链进 /koolshare/webs 是死路；
#   * 页面能读的文本通道是 httpdb 的 /_temp/<name>，映射到 /tmp/upload/<name>
#     （对照软件中心自己的 /tmp/upload/soft_log.txt ↔ /_temp/soft_log.txt）；
#   * tmpfs 不磨损 flash，状态每 2 秒写一次也安全。
PHANTOM_LOG=/tmp/upload/phantom_log.txt
PHANTOM_STATUS=/tmp/upload/phantom_status.txt
PHANTOM_PIDFILE=/tmp/phantom.pid
PHANTOM_STATUS_PIDFILE=/tmp/phantom_status.pid
export PHANTOM_LOG PHANTOM_STATUS PHANTOM_PIDFILE PHANTOM_STATUS_PIDFILE

# 按绝对路径查找外部命令。
#
# **本固件的 busybox 没有 `command -v`（也没有 `type`）** —— 用了只会得到
# "command: not found"，任何基于它的判断都会静默走错分支。曾经因此让插件的
# 「是否能用 dbus」判定永远失败，退化成读空的配置文件，表现为「提交后永不启动」。
# 所以关键命令一律在安装时探测出绝对路径，写进 phantom.env 供运行时使用。
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

# 探测结果，找不到就退回裸命令名（交给 PATH）
_d=$(cmd_path dbus);   [ -n "$_d" ] && PHANTOM_DBUS="$_d"   || PHANTOM_DBUS=""
_d=$(cmd_path curl);   [ -n "$_d" ] && PHANTOM_CURL="$_d"   || PHANTOM_CURL=""
_d=$(cmd_path wget);   [ -n "$_d" ] && PHANTOM_WGET="$_d"   || PHANTOM_WGET=""
_d=$(cmd_path cru);    [ -n "$_d" ] && PHANTOM_CRU="$_d"    || PHANTOM_CRU=""
_d=$(cmd_path setsid); [ -n "$_d" ] && PHANTOM_SETSID="$_d" || PHANTOM_SETSID=""
# 本机 busybox 没有 setsid applet，但带了 start-stop-daemon（-b 会 setsid），
# 后台派发优先用它 —— 这是让 start 彻底脱离 httpd 进程组的关键。
_d=$(cmd_path start-stop-daemon); [ -n "$_d" ] && PHANTOM_SSD="$_d" || PHANTOM_SSD=""

# SOCKS 测速专用 curl。
# 固件自带的 curl 可能是用 --disable-proxy 编的：实测 `/usr/sbin/curl` 一调
# `--socks5-hostname` 就报 "proxy support is disabled in this libcurl"（返回 000），
# 而仅测「本机 metrics」那种不走代理的请求照常可用 —— 所以不能只探测「有没有 curl」，
# 必须**按 SOCKS 真调一次**（目标 127.0.0.1:1，秒失败，不会外联）。
# koolshare 随包带的 /koolshare/bin/curl-fancyss 有 SOCKS，优先用它。
probe_socks_curl() {
    for _c in $PHANTOM_CURL /koolshare/bin/curl-fancyss /koolshare/bin/curl \
              /usr/bin/curl /usr/sbin/curl /bin/curl; do
        [ -n "$_c" ] || continue
        [ -x "$_c" ] || continue
        # 两个判据都要看（都踩过坑）：
        #   * 不给 -s：--disable-proxy 编的 curl 会打印
        #     "proxy support is disabled in this libcurl"；加了 -s 连这句都被静音，
        #     探测会永远「成功」；
        #   * 退出码 4（CURLE_NOT_BUILT_IN）：有些构建只回退出码不回文案。
        _err=$("$_c" -o /dev/null --max-time 2 --socks5-hostname 127.0.0.1:1 \
                    http://127.0.0.1:1 2>&1)
        _rc=$?
        case "$_err" in
            *"proxy support is disabled"*) continue ;;
        esac
        [ "$_rc" = "4" ] && continue
        printf '%s' "$_c"
        return 0
    done
    return 1
}
_d=$(probe_socks_curl); [ -n "$_d" ] && PHANTOM_CURL_SOCKS="$_d" || PHANTOM_CURL_SOCKS=""

DBUS="${PHANTOM_DBUS:-dbus}"
export PHANTOM_DBUS PHANTOM_CURL PHANTOM_CURL_SOCKS PHANTOM_WGET PHANTOM_CRU PHANTOM_SETSID PHANTOM_SSD

# 未安装完成前 set -e 会中断平台检测的友好提示，所以只在安装阶段开启。

echo_date() {
    echo "【$(date '+%Y-%m-%d %H:%M:%S')】 $*"
}

# BusyBox sed 支持 `sed -i`，BSD sed（macOS）要求 -i 必须带后缀，
# 两者语法不兼容；统一走「临时文件 + mv」，在任何 sed 上行为一致。
sed_inplace() {
    sed "$1" "$2" >"$2.sedtmp" 2>/dev/null && mv "$2.sedtmp" "$2" || rm -f "$2.sedtmp"
}

exit_install() {
    local state=$1
    case "$state" in
        1)
            echo_date "本插件适用于【koolshare 梅林改/官改 hnd/axhnd/axhnd.675x】固件平台！"
            echo_date "你的固件平台不能安装！！！"
            echo_date "本插件支持机型/平台：https://github.com/koolshare/rogsoft#rogsoft"
            echo_date "退出安装！"
            # 精确删解压目录，不要用 /tmp/phantom* ——那会把用户放在 /tmp 的
            # 离线包（phantom-0.1.0.tar.gz）一起删掉，想重装就得重新上传
            rm -rf "/tmp/${module}" >/dev/null 2>&1
            exit 1
            ;;
        0|*)
            rm -rf "/tmp/${module}" >/dev/null 2>&1
            exit 0
            ;;
    esac
}

# 离线包完整性自检。
#
# 为什么必须有：拷贝用的都是通配符（`cp scripts/*.sh`），包缺目录时 cp 只会
# 报错到 stderr，脚本继续往下跑，最后照样打印「安装完毕」——用户看到的是
# 「插件装了但页面打不开、脚本找不到」。与其事后排查，不如在动任何东西之前退出。
verify_package() {
    local missing=""
    for item in install.sh uninstall.sh version scripts/phantom_config.sh \
                scripts/phantom_status.sh webs/Module_phantom.asp res/phantom.css \
                res/icon-phantom.png bin/phantom-aarch64 bin/phantom-armv7; do
        [ -e "${DIR}/${item}" ] || missing="${missing} ${item}"
    done
    if [ -n "${missing}" ]; then
        echo_date "错误：安装包不完整，缺少以下文件："
        echo_date "  ${missing}"
        echo_date "请重新下载完整的离线包（dist/phantom-<version>.tar.gz）后重装。"
        echo_date "注意：必须在解压出的 phantom/ 目录里执行 install.sh，不要单独拷贝个别文件。"
        exit 1
    fi
}

# ---------------------------------------------------------------- 机型/固件

get_model() {
    local ODMPID=$(nvram get odmpid 2>/dev/null)
    local PRODUCTID=$(nvram get productid 2>/dev/null)
    if [ -n "${ODMPID}" ]; then
        MODEL="${ODMPID}"
    else
        MODEL="${PRODUCTID}"
    fi
}

get_fw_type() {
    # 官改固件的 extendno 形如 "24199_koolcenter"（不含 koolshare 字样），
    # ks梅林改版才是 "xxx_koolshare"，所以两个关键词都要认。
    local KS_TAG=$(nvram get extendno 2>/dev/null | grep -E "koolshare|koolcenter")
    if [ -d "/koolshare" ]; then
        if [ -n "${KS_TAG}" ]; then
            FW_TYPE_CODE="2"
            FW_TYPE_NAME="koolshare官改固件"
        else
            FW_TYPE_CODE="4"
            FW_TYPE_NAME="koolshare梅林改版固件"
        fi
    else
        if uname -o 2>/dev/null | grep -q Merlin; then
            FW_TYPE_CODE="3"
            FW_TYPE_NAME="梅林原版固件"
        else
            FW_TYPE_CODE="1"
            FW_TYPE_NAME="华硕官方固件"
        fi
    fi
}

# 软件中心模式要求：/koolshare 存在 + skipd 存在 + 内核 >= 4.1
platform_test() {
    local LINUX_VER=$(uname -r | awk -F. '{print $1$2}')
    if [ -d "/koolshare" ] && [ -f "/usr/bin/skipd" ] && [ "${LINUX_VER}" -ge "41" ]; then
        KS_MODE=1
        echo_date "机型：${MODEL} ${FW_TYPE_NAME} 符合安装要求，开始安装插件！"
        return 0
    fi
    # 无软件中心：只要有可写的 /jffs 就走降级安装
    if [ -d "/jffs" ] && [ -w "/jffs" ]; then
        KS_MODE=0
        echo_date "未检测到 koolshare 软件中心（${FW_TYPE_NAME}），改用 /jffs 降级安装！"
        return 0
    fi
    return 1
}

get_ui_type() {
    # 默认 ASUSWRT 皮肤；ROG/TUF 机型按 MODEL 判定（与 rogsoft 官方插件一致）
    local ROG=0
    local TUF=0
    [ "${MODEL}" = "GT-AC5300" ] && ROG=1
    [ "${MODEL}" = "GT-AX6000" ] && ROG=1
    [ "${MODEL}" = "GT-AX11000" ] && ROG=1
    [ "${MODEL}" = "GT-AX11000_BO4" ] && ROG=1
    [ "${MODEL}" = "GT-AXE11000" ] && ROG=1
    [ "${MODEL}" = "RT-AC86U" ] && ROG=1
    [ "${MODEL}" = "TUF-AX3000" ] && TUF=1
    [ "${MODEL}" = "TUF-AX5400" ] && TUF=1

    if [ "${TUF}" = "1" ]; then
        UI_TYPE="TUF"
    elif [ "${ROG}" = "1" ]; then
        UI_TYPE="ROG"
    else
        UI_TYPE="ASUSWRT"
    fi
}

install_ui() {
    get_ui_type
    # 皮肤标记同时存在于 ASP 内联样式和 res/phantom.css 里，两个文件都要 sed
    local ASP="${WEBS_DIR}/Module_${module}.asp"
    local CSS="${RES_DIR}/phantom.css"
    local targets=""
    [ -f "$ASP" ] && targets="$ASP"
    [ -f "$CSS" ] && targets="$targets $CSS"
    [ -n "$targets" ] || return 0
    local f
    case "${UI_TYPE}" in
        ROG)
            echo_date "安装 ROG 皮肤！"
            for f in $targets; do sed_inplace '/asuscss/d' "$f"; done
            ;;
        TUF)
            echo_date "安装 TUF 皮肤！"
            for f in $targets; do
                sed_inplace '/asuscss/d' "$f"
                sed_inplace 's/3e030d/3e2902/g;s/91071f/92650F/g;s/680516/D0982C/g;s/cf0a2c/c58813/g;s/700618/74500b/g;s/530412/92650F/g' "$f"
            done
            ;;
        *)
            echo_date "安装 ASUSWRT 皮肤！"
            for f in $targets; do sed_inplace '/rogcss/d' "$f"; done
            ;;
    esac
}

# ---------------------------------------------------------------- 安装路径

setup_paths() {
    if [ "${KS_MODE}" = "1" ]; then
        SCRIPTS_DIR="/koolshare/scripts"
        RES_DIR="/koolshare/res"
        WEBS_DIR="/koolshare/webs"
        BIN_DIR="/koolshare/bin"
        RUNTIME_DIR="/koolshare/etc/phantom"
        UNINSTALL="/koolshare/scripts/uninstall_${module}.sh"
    else
        INSTALL_ROOT="/jffs/phantom"
        SCRIPTS_DIR="${INSTALL_ROOT}"
        RES_DIR="${INSTALL_ROOT}/res"
        WEBS_DIR="${INSTALL_ROOT}/webs"
        BIN_DIR="${INSTALL_ROOT}/bin"
        # 降级模式的运行目录就是安装根本身，这样 ${RUNTIME_DIR}/etc/phantom.conf
        # 落在 /jffs/phantom/etc/phantom.conf，不会多出一层 etc
        RUNTIME_DIR="${INSTALL_ROOT}"
        UNINSTALL="${INSTALL_ROOT}/uninstall.sh"
    fi
}

# 选二进制：rogsoft 建议 hnd/axhnd 用 32 位，但 aarch64 机型内核未必开
# CONFIG_COMPAT，所以双架构都打包，按 uname -m 选。
pick_binary() {
    local arch=$(uname -m)
    case "$arch" in
        aarch64|arm64|armv8*) SRC_BIN="${DIR}/bin/phantom-aarch64" ;;
        armv7*|armv6*|armv5*|arm) SRC_BIN="${DIR}/bin/phantom-armv7" ;;
        *) SRC_BIN="${DIR}/bin/phantom-aarch64" ;;
    esac
    if [ ! -f "${SRC_BIN}" ]; then
        # 回退到包里存在的任意一个，总比没有强
        if [ -f "${DIR}/bin/phantom-armv7" ]; then
            SRC_BIN="${DIR}/bin/phantom-armv7"
        elif [ -f "${DIR}/bin/phantom-aarch64" ]; then
            SRC_BIN="${DIR}/bin/phantom-aarch64"
        else
            echo_date "错误：安装包内没有找到二进制（${DIR}/bin/phantom-*）"
            exit 1
        fi
    fi
    echo_date "选择二进制：$(basename "${SRC_BIN}")（uname -m = ${arch}）"
}

# 页面要读的状态与日志：物理文件在 /tmp/upload/（tmpfs），HTTP 通道是
# httpdb 的 /_temp/<name>。
#
# 踩过的坑：最初把文件放 /tmp 再软链进 docroot，以为页面 GET /phantom_status.txt
# 就能读到 —— 真机上恒 404。原因是 httpd 只服务白名单扩展名，.txt 一律不服务
# （用固件自带的 /Lang_Hdr.txt 验证过），与权限、软链都无关。
# 改用 koolshare 通用通道后无需任何软链：写 /tmp/upload/x.txt，页面读 /_temp/x.txt。
setup_runtime_files() {
    mkdir -p /tmp/upload 2>/dev/null
    if [ ! -d /tmp/upload ]; then
        echo_date "警告：/tmp/upload 不存在且建不出来，页面将读不到状态与日志"
    fi
    # 初始状态文件：插件装了就先有个合法的「未运行」文档，
    # 页面第一次轮询就是 200 而不是 404（避免一整屏 console 噪音）
    if [ ! -f "$PHANTOM_STATUS" ]; then
        printf '%s\n' '{"ts":0,"running":0,"up_rate":0,"down_rate":0,"total_up":0,"total_down":0,"udp_up":0,"udp_down":0,"conns":0,"direct":0,"proxy":0,"cpu":0}' >"$PHANTOM_STATUS"
    fi
    [ -f "$PHANTOM_LOG" ] || : >"$PHANTOM_LOG"
    chmod 644 "$PHANTOM_STATUS" "$PHANTOM_LOG" 2>/dev/null

    # 清理历史版本遗留的死软链（升级安装时）
    rm -f "${WEBS_DIR}/phantom_status.txt" "${WEBS_DIR}/phantom_log.txt" >/dev/null 2>&1
    rm -f /www/_temp/phantom_status.txt /www/_temp/phantom_log.txt >/dev/null 2>&1
    rm -f /tmp/phantom_log.txt /tmp/phantom_status.txt >/dev/null 2>&1

    echo_date "状态/日志写入 /tmp/upload/（页面路径 /_temp/phantom_status.txt）"
}

write_env() {
    mkdir -p "${RUNTIME_DIR}/etc" 2>/dev/null
    cat >"${RUNTIME_DIR}/phantom.env" <<EOF
# Phantom 插件安装布局（由 install.sh 生成，勿手工编辑）
PHANTOM_KS=${KS_MODE}
PHANTOM_SCRIPTS_DIR=${SCRIPTS_DIR}
PHANTOM_BIN_DIR=${BIN_DIR}
PHANTOM_RUNTIME_DIR=${RUNTIME_DIR}
# 运行期文件一律放 tmpfs 的 /tmp/upload：页面经 httpdb 的 /_temp/ 读，
# 且不磨损 flash（httpd 不服务 docroot 下的 .txt，所以不能放 docroot）。
# pidfile 也在 tmpfs —— 路由器重启后自动清空，避免陈旧 pid 被复用导致
# "服务已在运行"的误判。
PHANTOM_LOG=${PHANTOM_LOG}
PHANTOM_STATUS=${PHANTOM_STATUS}
PHANTOM_PIDFILE=${PHANTOM_PIDFILE}
PHANTOM_STATUS_PIDFILE=${PHANTOM_STATUS_PIDFILE}
PHANTOM_CONF=${RUNTIME_DIR}/etc/phantom.toml
PHANTOM_DOMAINS=${RUNTIME_DIR}/etc/proxy_domains.txt
PHANTOM_UI=${UI_TYPE}
# 外部命令的绝对路径（安装时探测）。
# 本固件的 busybox 没有 command -v，运行时不猜、直接用这里的结果。
PHANTOM_DBUS=${PHANTOM_DBUS}
PHANTOM_CURL=${PHANTOM_CURL}
# 测速专用（带 SOCKS 支持的 curl；固件自带 curl 可能是 --disable-proxy 编的）
PHANTOM_CURL_SOCKS=${PHANTOM_CURL_SOCKS}
PHANTOM_WGET=${PHANTOM_WGET}
PHANTOM_CRU=${PHANTOM_CRU}
PHANTOM_SETSID=${PHANTOM_SETSID}
PHANTOM_SSD=${PHANTOM_SSD}
EOF
    chmod 644 "${RUNTIME_DIR}/phantom.env"
}

# ---------------------------------------------------------------- 配置项默认值

set_defaults() {
    local PLVER=$(cat "${DIR}/version" 2>/dev/null)
    [ -n "${PLVER}" ] || PLVER="0.1.0"

    "$DBUS" set ${module}_version="${PLVER}"
    "$DBUS" set softcenter_module_${module}_version="${PLVER}"
    "$DBUS" set softcenter_module_${module}_install="1"
    "$DBUS" set softcenter_module_${module}_name="${module}"
    "$DBUS" set softcenter_module_${module}_title="${TITLE}"
    "$DBUS" set softcenter_module_${module}_description="${DESCR}"

    [ -n "$("$DBUS" get ${module}_enable)" ]      || "$DBUS" set ${module}_enable="0"
    [ -n "$("$DBUS" get ${module}_uri)" ]         || "$DBUS" set ${module}_uri=""
    [ -n "$("$DBUS" get ${module}_mode)" ]        || "$DBUS" set ${module}_mode="smart"
    [ -n "$("$DBUS" get ${module}_protocol)" ]    || "$DBUS" set ${module}_protocol="tcp"
    [ -n "$("$DBUS" get ${module}_lan_if)" ]      || "$DBUS" set ${module}_lan_if="br0"
    [ -n "$("$DBUS" get ${module}_tun_addr)" ]    || "$DBUS" set ${module}_tun_addr="10.7.0.1/24"
    [ -n "$("$DBUS" get ${module}_tun_name)" ]    || "$DBUS" set ${module}_tun_name="phantom0"
    [ -n "$("$DBUS" get ${module}_dns_hijack)" ]  || "$DBUS" set ${module}_dns_hijack="1"
    [ -n "$("$DBUS" get ${module}_builtin_wl)" ]  || "$DBUS" set ${module}_builtin_wl="1"
    [ -n "$("$DBUS" get ${module}_whitelist)" ]   || "$DBUS" set ${module}_whitelist=""
    [ -n "$("$DBUS" get ${module}_cron_enable)" ] || "$DBUS" set ${module}_cron_enable="0"
    [ -n "$("$DBUS" get ${module}_cron_time)" ]   || "$DBUS" set ${module}_cron_time="4:30"
    [ -n "$("$DBUS" get ${module}_watchdog)" ]    || "$DBUS" set ${module}_watchdog="1"
    # 服务端带宽（Mbps），仅用于把测速结果翻译成「是否接近天花板」
    [ -n "$("$DBUS" get ${module}_server_up_mbps)" ]   || "$DBUS" set ${module}_server_up_mbps="3"
    [ -n "$("$DBUS" get ${module}_server_down_mbps)" ] || "$DBUS" set ${module}_server_down_mbps="5"
    [ -n "$("$DBUS" get ${module}_log_level)" ]   || "$DBUS" set ${module}_log_level="info"
    [ -n "$("$DBUS" get ${module}_table)" ]       || "$DBUS" set ${module}_table="200"
    "$DBUS" set ${module}_last_act="安装完成 $(date '+%m-%d %H:%M:%S')"
}

# ---------------------------------------------------------------- 安装

install_now() {
    local PLVER=$(cat "${DIR}/version" 2>/dev/null)

    # 更新前先停服，避免把二进制从运行中的进程底下换掉
    if [ -f "${SCRIPTS_DIR}/${module}_config.sh" ]; then
        echo_date "安装前先停止已有服务..."
        "$SH" "${SCRIPTS_DIR}/${module}_config.sh" stop >/dev/null 2>&1
    fi

    echo_date "安装插件相关文件..."
    mkdir -p "${SCRIPTS_DIR}" "${RES_DIR}" "${WEBS_DIR}" "${BIN_DIR}" "${RUNTIME_DIR}"

    cp -rf "${DIR}/scripts/"*.sh "${SCRIPTS_DIR}/"
    cp -rf "${DIR}/webs/"*.asp "${WEBS_DIR}/"
    [ -f "${DIR}/res/phantom.css" ] && cp -rf "${DIR}/res/phantom.css" "${RES_DIR}/"
    [ -f "${DIR}/res/icon-phantom.png" ] && cp -rf "${DIR}/res/icon-phantom.png" "${RES_DIR}/"
    cp -rf "${DIR}/uninstall.sh" "${UNINSTALL}"

    chmod 755 "${SCRIPTS_DIR}/${module}_"*.sh >/dev/null 2>&1
    chmod 755 "${UNINSTALL}" >/dev/null 2>&1

    # 再验一次：cp 失败时不让它变成「安装成功但什么都没有」
    local after=""
    for f in "${SCRIPTS_DIR}/${module}_config.sh" "${WEBS_DIR}/Module_${module}.asp" \
             "${RES_DIR}/phantom.css" "${UNINSTALL}"; do
        [ -f "$f" ] || after="${after} ${f}"
    done
    if [ -n "${after}" ]; then
        echo_date "错误：文件拷贝失败，以下目标不存在："
        echo_date "  ${after}"
        echo_date "请检查磁盘空间（df -h /koolshare）与目录权限后重试。"
        exit 1
    fi

    pick_binary
    cp -f "${SRC_BIN}" "${BIN_DIR}/phantom"
    chmod 755 "${BIN_DIR}/phantom"

    setup_paths
    setup_runtime_files
    # install_ui 先跑：write_env 要把最终的 UI 类型写进 phantom.env
    install_ui
    write_env

    if [ "${KS_MODE}" = "1" ]; then
        # 开机自启与 nat-start 兜底都靠 init.d 软链：
        #   S98 → ks-wan-start.sh 在 wan-start 时以 `start` 调用
        #   N98 → ks-nat-start.sh 在 nat-start 时以 `start_nat` 调用
        # （Asuswrt 重建 iptables 会把 gateway 的转发/DNAT 规则冲掉，
        #   N 钩子负责把规则补回来，否则「插件显示运行中、LAN 却断」。）
        mkdir -p /koolshare/init.d
        rm -f "/koolshare/init.d/S98${module}.sh" "/koolshare/init.d/N98${module}.sh" >/dev/null 2>&1
        ln -sf "${SCRIPTS_DIR}/${module}_config.sh" "/koolshare/init.d/S98${module}.sh"
        ln -sf "${SCRIPTS_DIR}/${module}_config.sh" "/koolshare/init.d/N98${module}.sh"
        echo_date "设置插件默认参数..."
        set_defaults
    else
        install_ui
        echo_date "注册 /jffs/scripts 启动钩子..."
        register_jffs_hooks
        # 降级模式没有 dbus，用配置文件保存默认值
        [ -f "${RUNTIME_DIR}/etc/phantom.conf" ] || write_default_conf
    fi

    # 二进制自检（失败只提示，不阻断安装）
    if "${BIN_DIR}/phantom" --version >/dev/null 2>&1; then
        echo_date "二进制自检通过："$("${BIN_DIR}/phantom" --version 2>/dev/null | head -n 1)
    else
        echo_date "警告：二进制自检未通过（架构不匹配或缺少 --version），请用 SSH 手工执行 ${BIN_DIR}/phantom --version 确认"
    fi

    # 之前是启用状态则重新拉起
    local ENABLE=$("$DBUS" get ${module}_enable 2>/dev/null)
    if [ "${ENABLE}" = "1" ] && [ -f "${SCRIPTS_DIR}/${module}_config.sh" ]; then
        echo_date "安装完毕，重新启用${TITLE}！"
        "$SH" "${SCRIPTS_DIR}/${module}_config.sh" start >/dev/null 2>&1
    fi

    echo_date "${TITLE} 插件安装完毕（版本 ${PLVER}）！"
    exit_install 0
}

# 降级模式：梅林/官方固件的开机与 nat 钩子
register_jffs_hooks() {
    mkdir -p /jffs/scripts 2>/dev/null || {
        echo_date "警告：/jffs/scripts 不存在，请在 Web UI 启用「JFFS 自定义脚本」"
        return 1
    }
    local HOOK="/jffs/scripts/services-start"
    [ -f "$HOOK" ] || printf '#!/bin/sh\n' >"$HOOK"
    if ! grep -q "${SCRIPTS_DIR}/${module}_config.sh" "$HOOK" 2>/dev/null; then
        # 钩子由固件在启动时执行，PATH 不可控，所以显式写 /bin/sh
        printf '%s\n' "${SH} ${SCRIPTS_DIR}/${module}_config.sh start &   # phantom" >>"$HOOK"
    fi
    # Asuswrt 在 nat-start 会重建 iptables，需要重启补规则
    local NAT_HOOK="/jffs/scripts/nat-start"
    [ -f "$NAT_HOOK" ] || printf '#!/bin/sh\n' >"$NAT_HOOK"
    if ! grep -q "${SCRIPTS_DIR}/${module}_config.sh" "$NAT_HOOK" 2>/dev/null; then
        # start_nat 只在规则真被冲掉时才重启，避免每次 NAT 重载都抖一次全屋网络
        printf '%s\n' "${SH} ${SCRIPTS_DIR}/${module}_config.sh start_nat &   # phantom: nat-start flushes iptables" >>"$NAT_HOOK"
    fi
    chmod 755 "$HOOK" "$NAT_HOOK" >/dev/null 2>&1
}

write_default_conf() {
    mkdir -p "${RUNTIME_DIR}/etc"
    cat >"${RUNTIME_DIR}/etc/phantom.conf" <<'CONF'
# Phantom 降级模式配置（无软件中心时由 phantom_config.sh 读取）
# 由 dbus_shim 读写，格式与 dbus 键一一对应（去掉 phantom_ 前缀）
enable='0'
uri=''
mode='smart'
protocol='tcp'
lan_if='br0'
tun_name='phantom0'
tun_addr='10.7.0.1/24'
table='200'
dns_hijack='1'
builtin_wl='1'
whitelist=''
cron_enable='0'
cron_time='4:30'
watchdog='1'
log_level='info'
server_up_mbps='3'
server_down_mbps='5'
CONF
    chmod 600 "${RUNTIME_DIR}/etc/phantom.conf"
}

install() {
    verify_package
    get_model
    get_fw_type
    if ! platform_test; then
        exit_install 1
    fi
    setup_paths
    install_now
}

install
