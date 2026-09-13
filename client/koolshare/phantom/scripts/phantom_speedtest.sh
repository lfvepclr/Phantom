#!/bin/sh
# Phantom 吞吐探测（测速）
#
# 关键：gateway 模式只代理 iif br0 的转发流量，路由器自身发起的连接是直连的，
# 所以必须显式走 SOCKS5 127.0.0.1:1080，否则测的是裸宽带。
# 测得的是「phantom 用户态转发吞吐上限」，不含 LAN→TUN 的 NAT 转发路径。

module=phantom

for candidate in /koolshare/etc/phantom/phantom.env /jffs/phantom/phantom.env; do
    if [ -f "$candidate" ]; then
        . "$candidate"
        break
    fi
done

RUNTIME_DIR="${PHANTOM_RUNTIME_DIR:-/koolshare/etc/phantom}"
SCRIPTS_DIR="${PHANTOM_SCRIPTS_DIR:-/koolshare/scripts}"
USER_CONF="${RUNTIME_DIR}/etc/phantom.conf"

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

# 测速必须走 SOCKS5，而**固件自带的 curl 可能编了 --disable-proxy**：
# /usr/sbin/curl 一调 --socks5-hostname 就报 "proxy support is disabled in this libcurl"
# （返回 000），只有 koolshare 随包带的 /koolshare/bin/curl-fancyss 能用。
# install.sh 已经探测好写进 PHANTOM_CURL_SOCKS；这里再兜底探测一次（老环境/手改配置）。
socks_curl_ok() {
    [ -n "$1" ] && [ -x "$1" ] || return 1
    # 不给 -s（-s 会把 "proxy support is disabled" 一起静音，探测永远成功），
    # 另外把退出码 4（CURLE_NOT_BUILT_IN）也当「不支持 SOCKS」。
    _err=$("$1" -o /dev/null --max-time 2 --socks5-hostname 127.0.0.1:1 \
                http://127.0.0.1:1 2>&1)
    _rc=$?
    case "$_err" in
        *"proxy support is disabled"*) return 1 ;;
    esac
    [ "$_rc" = "4" ] && return 1
    return 0
}

CURL="${PHANTOM_CURL_SOCKS:-}"
if ! socks_curl_ok "$CURL"; then
    CURL=""
    for _c in /koolshare/bin/curl-fancyss /koolshare/bin/curl /usr/bin/curl \
              /usr/sbin/curl /bin/curl $(cmd_path curl); do
        if socks_curl_ok "$_c"; then
            CURL="$_c"
            break
        fi
    done
fi

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
    fi
}

[ -n "$CURL" ] || {
    echo "【$(date '+%Y-%m-%d %H:%M:%S')】 测速失败：本机没有带 SOCKS 支持的 curl（固件自带的多为 --disable-proxy 编译）。"
    echo "【$(date '+%Y-%m-%d %H:%M:%S')】 解决：确认 /koolshare/bin/curl-fancyss 存在（koolshare 自带），或用 LAN 客户端实测网速。"
    set_cfg speed_last "测速失败：无 SOCKS curl $(date '+%m-%d %H:%M:%S')"
    exit 1
}

SOCKS="127.0.0.1:1080"
URL=$(get_cfg speedtest_url)
# 样本默认 5MB：服务端上行 3Mbps 时约 13 秒，既能跑满慢启动又不会让页面等太久
[ -n "$URL" ] || URL="http://cachefly.cachefly.net/5mb.test"
FALLBACK_URL="http://speedtest.tele2.net/5MB.zip"
MAX_TIME=60

# 服务端带宽（Mbps），用于把测速结果翻译成「链路是否健康」。
# 方向是反的：客户端下载占用的是服务端上行，客户端上传占用的是服务端下行。
SERVER_UP_MBPS=$(get_cfg server_up_mbps)
[ -n "$SERVER_UP_MBPS" ] || SERVER_UP_MBPS=3
SERVER_DOWN_MBPS=$(get_cfg server_down_mbps)
[ -n "$SERVER_DOWN_MBPS" ] || SERVER_DOWN_MBPS=5
# 下载方向的天花板 = 服务端上行
DL_LIMIT_KBPS=$((SERVER_UP_MBPS * 1000 / 8))

human_bps() {
    # 入参 bytes/s，输出人类可读（整数运算，ash 没有浮点）
    bps=$1
    if [ "$bps" -ge 1048576 ]; then
        printf '%s.%s MB/s' $((bps / 1048576)) $(( (bps % 1048576) * 10 / 1048576 ))
    elif [ "$bps" -ge 1024 ]; then
        printf '%s KB/s' $((bps / 1024))
    else
        printf '%s B/s' "$bps"
    fi
}

run_once() {
    url="$1"
    out=$("$CURL" -s --socks5-hostname "$SOCKS" \
               -o /dev/null \
               -w '%{size_download} %{speed_download} %{time_total} %{time_starttransfer} %{http_code}' \
               --max-time "$MAX_TIME" "$url" 2>/dev/null)
    printf '%s' "$out"
}

echo "【$(date '+%Y-%m-%d %H:%M:%S')】 测速目标：${URL}（经 SOCKS5 ${SOCKS}）"

res=$(run_once "$URL")
code=$(printf '%s' "$res" | awk '{print $NF}')
size=$(printf '%s' "$res" | awk '{print $1}')

if [ "$code" != "200" ] || [ -z "$size" ] || [ "$size" -lt 1024 ] 2>/dev/null; then
    echo "【$(date '+%Y-%m-%d %H:%M:%S')】 主测速源不可用（http=${code} size=${size}），改用备用源：${FALLBACK_URL}"
    res=$(run_once "$FALLBACK_URL")
    code=$(printf '%s' "$res" | awk '{print $NF}')
    size=$(printf '%s' "$res" | awk '{print $1}')
    URL="$FALLBACK_URL"
fi

if [ -z "$size" ] || [ "$size" -lt 1024 ] 2>/dev/null; then
    echo "【$(date '+%Y-%m-%d %H:%M:%S')】 测速失败：未下载到数据（http=${code}）。请确认隧道已启动且服务端可访问外网。"
    set_cfg speed_last "测速失败 $(date '+%m-%d %H:%M:%S')"
    exit 1
fi

speed=$(printf '%s' "$res" | awk '{print $2}' | cut -d. -f1)
total=$(printf '%s' "$res" | awk '{print $3}')
ttfb=$(printf '%s' "$res" | awk '{print $4}')
[ -n "$speed" ] || speed=0

size_mb=$((size / 1048576))
if [ "$size_mb" -lt 1 ]; then
    size_text="$((size / 1024)) KB"
else
    size_text="$size_mb MB"
fi

echo "【$(date '+%Y-%m-%d %H:%M:%S')】 测速结果：$(human_bps "$speed")（样本 ${size_text}，耗时 ${total}s，首字节 ${ttfb}s）"
echo "【$(date '+%Y-%m-%d %H:%M:%S')】 口径说明：这是 phantom 用户态转发的吞吐上限，不含 LAN→TUN 的 NAT 转发路径，因此不等于客户端实测网速。"

# 与服务端带宽上限对比：接近上限说明链路健康，明显偏低才是需要排查的问题
if [ "$DL_LIMIT_KBPS" -gt 0 ] 2>/dev/null; then
    # 单位必须对齐：speed 是 bytes/s，折算成 kbps 后要和「服务端上行 Mbps → kbps」
    # 比，不能拿 kbps 去除 KB/s（那是 8 倍误差，真机上会打印出 786% 这种荒唐值）。
    speed_kbps=$((speed * 8 / 1000))
    limit_kbps=$((SERVER_UP_MBPS * 1000))
    pct=$((speed_kbps * 100 / limit_kbps))
    echo "【$(date '+%Y-%m-%d %H:%M:%S')】 下载方向天花板 = 服务端上行 ${SERVER_UP_MBPS} Mbps（约 $((DL_LIMIT_KBPS)) KB/s），本次达到 ${pct}%"
    if [ "$pct" -ge 80 ]; then
        echo "【$(date '+%Y-%m-%d %H:%M:%S')】 结论：已接近服务端带宽上限，链路健康（想更快只能换更快的服务器）。"
    elif [ "$pct" -ge 50 ]; then
        echo "【$(date '+%Y-%m-%d %H:%M:%S')】 结论：中等水平。可尝试调大 TUN MTU、检查 RTT，或换用 QUIC。"
    else
        echo "【$(date '+%Y-%m-%d %H:%M:%S')】 结论：明显低于服务端上限，建议排查：路由器 CPU、TUN 写队列峰值（tun_txq_peak_bytes）、重传（tcp_dup_bytes）、以及是否走了错误的链路。"
    fi
fi

set_cfg speed_last "$(human_bps "$speed") @ $(date '+%m-%d %H:%M:%S')"
exit 0
