#!/bin/sh
# Phantom koolshare 插件 —— 总控脚本
#
# 用法：/bin/sh phantom_config.sh <action>
#      （必须用 /bin/sh 绝对路径：Asuswrt 的 /usr/sbin/sh 是 memaccess，不是 shell）
#   1            保存配置并应用（enable=1 时启动隧道）
#   2            清空日志
#   3            一键测速
#   start        启动（开机 / 手工）
#   stop         停止并回滚路由与防火墙
#   restart      重启
#   status       打印状态
#   start_nat    nat-start 兜底（规则被冲掉时重启补回）
#   ks <0|1>     兼容入口（真实固件的开机路径是 init.d/S98phantom.sh start）
#   cron         重注册定时重启与看门狗
#   diag         输出诊断信息
#
# 软件中心通过 POST /_api/ {"id":N,"method":"phantom_config.sh","params":[flag],"fields":{...}}
# 调用本脚本：fields 由软件中心先写入 dbus，本脚本再从 dbus 读；
# 注意它把**请求 id 作为第一个参数**传进来，详见文末「调用约定」一节。
#
# POSIX sh only（BusyBox ash）。

module=phantom

# ---------------------------------------------------------------- 运行环境

# 安装布局由 install.sh 生成的 phantom.env 提供
for candidate in /koolshare/etc/phantom/phantom.env /jffs/phantom/phantom.env; do
    if [ -f "$candidate" ]; then
        . "$candidate"
        break
    fi
done

KS_MODE="${PHANTOM_KS:-1}"
SCRIPTS_DIR="${PHANTOM_SCRIPTS_DIR:-/koolshare/scripts}"
BIN_DIR="${PHANTOM_BIN_DIR:-/koolshare/bin}"
RUNTIME_DIR="${PHANTOM_RUNTIME_DIR:-/koolshare/etc/phantom}"
BIN="${BIN_DIR}/phantom"
# 运行期文件在 tmpfs：页面经 httpdb 的 /_temp/ 路由读，且不磨损 flash。
# 必须落在 /tmp/upload/ —— 本固件 httpd **不服务 docroot 下的 .txt**，
# 而 httpdb 的 /_temp/<name> 映射的正是 /tmp/upload/<name>
# （对照 ks_tar_install.sh 写 /tmp/upload/soft_log.txt + Module_Softsetting.asp
#  读 /_temp/soft_log.txt）。
LOG="${PHANTOM_LOG:-/tmp/upload/phantom_log.txt}"
STATUS_JSON="${PHANTOM_STATUS:-/tmp/upload/phantom_status.txt}"
PIDFILE="${PHANTOM_PIDFILE:-/tmp/phantom.pid}"
STATUS_PIDFILE="${PHANTOM_STATUS_PIDFILE:-/tmp/phantom_status.pid}"
CONF_FILE="${PHANTOM_CONF:-${RUNTIME_DIR}/etc/phantom.toml}"
DOMAINS_FILE="${PHANTOM_DOMAINS:-${RUNTIME_DIR}/etc/proxy_domains.txt}"
USER_CONF="${RUNTIME_DIR}/etc/phantom.conf"

# gateway.rs 用 Command::new("ip") 调用外部命令，不写绝对路径，
# 所以必须保证 PATH 里有 /sbin 与 /usr/sbin。
# 追加而非前置：init.d 拉起时 PATH 可能很窄，但前面的条目（包括测试注入的
# mock 目录）优先级更高，不能被系统路径盖掉。
PATH="${PATH}:/sbin:/usr/sbin:/bin:/usr/bin:/koolshare/bin"
export PATH

# 关键：不要用裸 `sh`。
# Asuswrt 上 /usr/sbin/sh 是指向 /bin/memaccess 的软链（Broadcom 的
# dw/dh/db/sw/sh/sb 内存调试工具家族），而 sshd/cron 的非交互 PATH 把
# /usr/sbin 排在 /bin 前面，于是 `sh xxx.sh` 会跑到 memaccess 上并报
# "Address xxx is invalid"。所有内部调用一律用绝对路径。
if [ -x /bin/sh ]; then
    SH=/bin/sh
else
    SH=sh
fi
# tracing 默认输出 ANSI 颜色，重定向到文件后会影响 grep/展示
export NO_COLOR=1

mkdir -p "${RUNTIME_DIR}/etc" 2>/dev/null
# 页面要读的两个文件必须能建出来；/tmp/upload 建不出来时退回 /tmp 并告警
for _d in "$(dirname "$LOG")" "$(dirname "$STATUS_JSON")"; do
    [ -d "$_d" ] || mkdir -p "$_d" 2>/dev/null
done

# 按绝对路径找外部命令。
# **本固件的 busybox 没有 `command -v`/`type`** —— 用它们只会得到
# "command: not found"，判断会静默失败（曾经让 DBUS 判定永远为假，
# 于是插件读到空配置、提交后永不启动）。
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

# 优先用 install.sh 探测好的绝对路径，其次现场找，最后退回裸命令名
DBUS="${PHANTOM_DBUS:-}"
[ -n "$DBUS" ] && [ -x "$DBUS" ] || DBUS=$(cmd_path dbus) || DBUS=dbus
SETSID="${PHANTOM_SETSID:-}"
[ -n "$SETSID" ] && [ -x "$SETSID" ] || SETSID=$(cmd_path setsid)
SSD="${PHANTOM_SSD:-}"
[ -n "$SSD" ] && [ -x "$SSD" ] || SSD=$(cmd_path start-stop-daemon)
CURL="${PHANTOM_CURL:-}"
[ -n "$CURL" ] && [ -x "$CURL" ] || CURL=$(cmd_path curl)
WGET="${PHANTOM_WGET:-}"
[ -n "$WGET" ] && [ -x "$WGET" ] || WGET=$(cmd_path wget)

if [ "${KS_MODE}" = "1" ] && "$DBUS" get ${module}_version >/dev/null 2>&1; then
    USE_DBUS=1
else
    USE_DBUS=0
fi

# ---------------------------------------------------------------- 配置读写

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

get_cfg() {
    if [ "$USE_DBUS" = "1" ]; then
        "$DBUS" get "${module}_$1" 2>/dev/null
    else
        conf_get "$1"
    fi
}

set_cfg() {
    if [ "$USE_DBUS" = "1" ]; then
        "$DBUS" set "${module}_$1=$2" >/dev/null 2>&1
    else
        conf_set "$1" "$2"
    fi
}

# ---------------------------------------------------------------- 日志

log_line() {
    echo "【$(date '+%Y-%m-%d %H:%M:%S')】 $*" >>"$LOG" 2>/dev/null
    # PHANTOM_QUIET=1：被软件中心 POST 调用的路径不回显 stdout。
    # 那个请求的响应体被前端按 JSON 解析，混进文本会解析失败。
    [ "${PHANTOM_QUIET:-0}" = "1" ] || echo "$*"
}

# ---------------------------------------------------------------- 软件中心回包
#
# httpdb 执行脚本后会**挂起那个 HTTP 请求**，直到脚本自己 POST 回
# `http://127.0.0.1:3030/_resp/<id>`（body 就是请求 id，前端拿它和
# `response.result == id` 比对判成功）。这正是 base.sh 里
# `http_response(){ curl -X POST -d "$ARG0" http://$LANIP:3030/_resp/$ID; }`
# 干的事。
#
# 不回包的后果在真机上实测过：页面一直转圈，最后弹「后台执行失败，请看日志页」，
# 而隧道其实什么都没做（fields 已落 dbus，看起来却像「提交失败」）。
# 所以回包必须在任何耗时动作**之前**发出，页面毫秒级返回，进度由状态文件呈现。
api_ack() {
    [ -n "${API_ID:-}" ] || return 0
    _port="${PHANTOM_HTTPDB_PORT:-3030}"
    _url="http://127.0.0.1:${_port}/_resp/${API_ID}"
    if [ -n "$CURL" ] && [ -x "$CURL" ]; then
        "$CURL" -s -m 5 -X POST -d "$API_ID" "$_url" >/dev/null 2>&1
    elif [ -n "$WGET" ] && [ -x "$WGET" ]; then
        "$WGET" -q -O - --post-data "$API_ID" "$_url" >/dev/null 2>&1
    fi
    return 0
}

# 把耗时动作放到完全脱离调用会话的后台执行。
#
# 为什么必须这样：软件中心是**同步执行**后台脚本的 —— POST /_api/ 会一直等到
# 脚本退出。而启动隧道要 stop(最多 3s) + sleep 3(等 Hello)，如果留在前台，
# 那个 HTTP 请求就长时间不返回、页面一直转圈；一旦脚本里有任何东西继续持有
# httpd 的输出管道（后台子进程继承 fd 是最常见的坑），请求甚至永远不返回 ——
# 现象就是「点提交后页面永久卡死」。
#
# **光重定向 fd 还不够，必须离开调用者的会话/进程组。**
# start 会拉起一个死循环的采样进程（phantom_status.sh），它永不退出；
# 只要它和 httpd 待在同一个进程组里，httpd 等自己那个组结束时就会一直等，
# 直到超时 —— POST 于是返回失败（前端弹「后台执行失败，请看日志页」），
# 更糟的是超时后 httpd 往往会直接干掉整个进程组，把正在正常工作的
# tunnel 进程一起带走，留下 ip rule/iptables 残留在内核里 → **全屋断网**。
#
# 本机 busybox **没有编 setsid applet**（`setsid: applet not found`），
# 但带了 start-stop-daemon（`-b` 后台化时会 setsid），所以三级回退。
detach_run() {
    # 必须传绝对路径：start-stop-daemon -b 会 chdir("/")，相对路径到那时就废了。
    _self="$0"
    case "$_self" in
        /*) ;;
        *) _self="$(cd "$(dirname "$0")" 2>/dev/null && pwd)/$(basename "$0")" ;;
    esac
    if [ -n "$SSD" ]; then
        # -p 是必需的：不给 pidfile 时 ssd 会按可执行名判重，而 /bin/sh
        # 永远有进程在跑，它会直接报 "already running" 拒绝启动（实测）。
        # -b 让子进程进自己的**新进程组**、父进程立即退出（ppid=1），
        # 这才是躲开 httpd「等进程组」/「超时后杀进程组」的关键 ——
        # 本机没有 setsid，双 fork 也做不到换进程组。
        _ssdpid=/tmp/phantom_detach.pid
        rm -f "$_ssdpid"
        "$SSD" -S -b -p "$_ssdpid" -x "$SH" -- "$_self" "$1" >/dev/null 2>&1 </dev/null
    elif [ -n "$SETSID" ]; then
        "$SETSID" "$SH" "$_self" "$1" >/dev/null 2>&1 </dev/null &
    else
        ( ( "$SH" "$_self" "$1" >/dev/null 2>&1 </dev/null & ) & )
    fi
    return 0
}

# JFFS/tmpfs 空间有限，日志必须截断（500 行 / 256KB）
trim_log() {
    [ -f "$LOG" ] || return 0
    # **必须原地覆盖，不能用 mv 轮换。**
    # 运行中的 phantom 进程是带着日志 fd 一直写（`>>"$LOG"`）的：一旦用
    # `mv 新文件 $LOG` 换掉 inode，它之后所有的输出都会写进一个没人引用的旧文件 ——
    # 页面和 SSH 都再也看不到隧道日志（只有重启隧道才恢复）。
    # 真机上踩过：清日志/截断之后，日志页停在「配置已保存…」，隧道却在满速跑。
    # `cat > "$LOG"` 是截断同一个 inode，append 语义保持不变。
    local lines=$(wc -l <"$LOG" 2>/dev/null | tr -d ' ')
    if [ -n "$lines" ] && [ "$lines" -gt 500 ] 2>/dev/null; then
        if tail -n 500 "$LOG" >"${LOG}.tmp" 2>/dev/null; then
            cat "${LOG}.tmp" >"$LOG" 2>/dev/null
        fi
        rm -f "${LOG}.tmp" 2>/dev/null
    fi
    local bytes=$(wc -c <"$LOG" 2>/dev/null | tr -d ' ')
    if [ -n "$bytes" ] && [ "$bytes" -gt 262144 ] 2>/dev/null; then
        if tail -c 200000 "$LOG" >"${LOG}.tmp" 2>/dev/null; then
            cat "${LOG}.tmp" >"$LOG" 2>/dev/null
        fi
        rm -f "${LOG}.tmp" 2>/dev/null
    fi
}

# ---------------------------------------------------------------- 字段校验

is_ip() {
    printf '%s' "$1" | grep -Eq '^[0-9]{1,3}(\.[0-9]{1,3}){3}$'
}

# URI 里的 host 必须是 IP：phantom 的 server.address 直接 parse::<SocketAddr>()，
# 不做 DNS 解析，填域名会在启动时报 Invalid server address。
uri_hostport() {
    s="${1#phantom://}"
    s="${s#*@}"
    s="${s%%\?*}"
    s="${s%%#*}"
    printf '%s' "$s"
}

resolve_host() {
    host="$1"
    if is_ip "$host"; then
        printf '%s' "$host"
        return 0
    fi
    ip=$(nslookup "$host" 2>/dev/null | awk '/^Address[ :]/ {print $NF}' | tail -n 1)
    if is_ip "$ip"; then
        printf '%s' "$ip"
        return 0
    fi
    printf '%s' "$host"
    return 1
}

# 把 URI 里的 host:port 换掉（保留公钥与 query）
uri_with_hostport() {
    uri="$1"
    newhp="$2"
    scheme_user="${uri%%@*}@"
    rest="${uri#*@}"
    query=""
    case "$rest" in *\?*) query="?${rest#*\?}"; rest="${rest%%\?*}" ;; esac
    frag=""
    case "$query" in *#*) frag="#${query#*#}"; query="${query%%#*}" ;; esac
    printf '%s%s%s%s' "$scheme_user" "$newhp" "$query" "$frag"
}

# 设置/移除 proto 参数（tcp 是默认值，显式移除即可）
uri_with_proto() {
    uri="$1"
    proto="$2"
    base="${uri%%\?*}"
    query=""
    case "$uri" in *\?*) query="${uri#*\?}" ;; esac
    frag=""
    case "$query" in *#*) frag="#${query#*#}"; query="${query%%#*}" ;; esac

    newq=""
    oldifs="$IFS"
    IFS='&'
    for kv in $query; do
        case "$kv" in proto=*) continue ;; esac
        [ -n "$kv" ] && newq="${newq}&${kv}"
    done
    IFS="$oldifs"
    [ "$proto" = "quic" ] && newq="${newq}&proto=quic"
    newq=$(printf '%s' "$newq" | sed 's/^&//')

    out="$base"
    [ -n "$newq" ] && out="${out}?${newq}"
    [ -n "$frag" ] && out="${out}${frag}"
    printf '%s' "$out"
}

# 域名白字符过滤：只保留 [a-z0-9.-_*]，其余丢弃，避免脏数据进入白名单文件
sanitize_domain() {
    printf '%s' "$1" | tr 'A-Z' 'a-z' | sed 's/[^a-z0-9._*-]//g'
}

# ---------------------------------------------------------------- 生成配置

write_domains() {
    wl=$(get_cfg whitelist)
    : >"$DOMAINS_FILE" 2>/dev/null
    oldifs="$IFS"
    IFS=','
    for d in $wl; do
        d=$(sanitize_domain "$d")
        [ -n "$d" ] && echo "$d" >>"$DOMAINS_FILE"
    done
    IFS="$oldifs"
    chmod 644 "$DOMAINS_FILE" 2>/dev/null
    return 0
}

write_toml() {
    mode=$(get_cfg mode)
    builtin_wl=$(get_cfg builtin_wl)
    [ -n "$mode" ] || mode="smart"
    [ -n "$builtin_wl" ] || builtin_wl="1"
    # TOML 只认 true/false，dbus 里存的是 1/0
    if [ "$builtin_wl" = "1" ]; then
        builtin_flag="true"
    else
        builtin_flag="false"
    fi

    cat >"$CONF_FILE" <<EOF
# 由 phantom_config.sh 自动生成，勿手工编辑。
# 服务器地址来自 dbus 的 phantom_uri，由 --server 传入并覆盖 servers 段，
# 所以这里不写 servers 段，避免两处不一致。
[client]
listen = "127.0.0.1:1080"
dns = "8.8.8.8:53"
dns_direct = "223.5.5.5:53"
mode = "${mode}"
metrics_listen = "127.0.0.1:9150"

[failover]
health_check_interval = 5
health_check_timeout = 5
failover_threshold = 2
graceful_migration = true

[hello]
timeout = 10

[rules]
builtin_proxy_whitelist = ${builtin_flag}
final_action = "direct"
EOF
    chmod 600 "$CONF_FILE" 2>/dev/null
    return 0
}

build_args() {
    tun_name=$(get_cfg tun_name);     [ -n "$tun_name" ] || tun_name="phantom0"
    tun_addr=$(get_cfg tun_addr);     [ -n "$tun_addr" ] || tun_addr="10.7.0.1/24"
    table=$(get_cfg table);           [ -n "$table" ] || table="200"
    lan_if=$(get_cfg lan_if);         [ -n "$lan_if" ] || lan_if="br0"
    dns_hijack=$(get_cfg dns_hijack); [ -n "$dns_hijack" ] || dns_hijack="1"

    set -- client -c "$CONF_FILE" --server "$1" \
        --tun --tun-name "$tun_name" --tun-addr "$tun_addr" \
        --gateway --table "$table"
    for iface in $lan_if; do
        set -- "$@" --lan-interface "$iface"
    done
    if [ "$dns_hijack" != "1" ]; then
        set -- "$@" --no-lan-dns-hijack
    fi
    printf '%s\n' "$*"
}

# ---------------------------------------------------------------- 进程管理

is_running() {
    [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE" 2>/dev/null)" 2>/dev/null
}

# metrics 是「隧道真的在工作」的唯一活证据。
fetch_metrics() {
    _url="http://127.0.0.1:${PHANTOM_METRICS_PORT:-9150}/metrics"
    if [ -n "$CURL" ]; then
        "$CURL" -s --max-time 2 "$_url" 2>/dev/null
    elif [ -n "$WGET" ]; then
        "$WGET" -q -O - -T 2 "$_url" 2>/dev/null
    fi
}

# **进程「活着」不等于「在工作」。**
# 隧道卡死（上游断开、relay 阻塞）时进程往往还会留在 ps 里，kill -0 照样成功，
# 但它已经不再转发任何包 —— 而它装的 ip rule / iptables 还指着 tun，
# 于是整个 LAN 的流量全灌进一个黑洞。表现出来就是「插件显示正在运行，
# 但全屋断网」，而且 start 会认为「已在运行」直接跳过，watchdog 也认为健康，
# 谁都不会去救它。所以健康判定必须带 metrics 探测。
is_healthy() {
    is_running || return 1
    _body=$(fetch_metrics 2>/dev/null)
    [ -n "$_body" ]
}

# 状态文件始终存在（内容是合法的「未运行」文档），
# 页面第一次轮询就是 200，不会刷一屏 404
write_zero_status() {
    printf '%s\n' '{"ts":0,"running":0,"up_rate":0,"down_rate":0,"total_up":0,"total_down":0,"udp_up":0,"udp_down":0,"conns":0,"direct":0,"proxy":0,"cpu":0}' >"$STATUS_JSON" 2>/dev/null
    chmod 644 "$STATUS_JSON" 2>/dev/null
}

cleanup_rules() {
    lan_if=$(get_cfg lan_if); [ -n "$lan_if" ] || lan_if="br0"
    table=$(get_cfg table);   [ -n "$table" ] || table="200"
    tun_name=$(get_cfg tun_name); [ -n "$tun_name" ] || tun_name="phantom0"
    for iface in $lan_if; do
        while ip rule del iif "$iface" lookup "$table" 2>/dev/null; do :; done
        while ip rule del iif "$iface" lookup main 2>/dev/null; do :; done
        # --gateway 还会给私网/组播地址装「直连例外」，进程异常死亡时同样会残留
        for net in 0.0.0.0/8 10.0.0.0/8 127.0.0.0/8 169.254.0.0/16 \
                   172.16.0.0/12 192.168.0.0/16 224.0.0.0/4 240.0.0.0/4; do
            while ip rule del to "$net" iif "$iface" lookup main 2>/dev/null; do :; done
        done
    done
    ip route flush table "$table" 2>/dev/null

    # iptables 侧同样要回滚：--gateway 插的 FORWARD ACCEPT 与 LAN 53 端口 DNAT
    # 在进程正常退出时由它自己删，僵死/被 SIGKILL 时不会 —— 残留的规则会把
    # 全屋流量丢进一个不存在的 tun（现象：插件是关的，却断网）。
    # DNAT 的目标地址取自生成的 client.toml（gateway 的 dns_sentinel），
    # 拿不到就按默认 8.8.8.8 试。
    sentinel=$(sed -n 's/^dns = "\([0-9.]*\):53"$/\1/p' "$CONF_FILE" 2>/dev/null | head -n 1)
    [ -n "$sentinel" ] || sentinel="8.8.8.8"
    for iface in $lan_if; do
        while iptables -D FORWARD -i "$iface" -o "$tun_name" -j ACCEPT 2>/dev/null; do :; done
        while iptables -D FORWARD -i "$tun_name" -o "$iface" -j ACCEPT 2>/dev/null; do :; done
        for proto in udp tcp; do
            while iptables -t nat -D PREROUTING -i "$iface" -p "$proto" --dport 53 \
                -j DNAT --to-destination "${sentinel}:53" 2>/dev/null; do :; done
        done
    done
    return 0
}

stop_status_loop() {
    if [ -f "$STATUS_PIDFILE" ]; then
        spid=$(cat "$STATUS_PIDFILE" 2>/dev/null)
        [ -n "$spid" ] && kill "$spid" 2>/dev/null
        rm -f "$STATUS_PIDFILE"
    fi
    return 0
}

stop() {
    stop_status_loop
    if ! is_running; then
        rm -f "$PIDFILE"
        log_line "服务未运行"
        # 这里**不能提前 return**：进程可能早就崩了/被 OOM 杀了，
        # 它装的 ip rule / iptables 不会自己消失 —— 那正是「全屋断网」的形态。
        cleanup_rules
        write_zero_status
        set_cfg last_act "已停止 $(date '+%m-%d %H:%M:%S')"
        return 0
    fi
    pid=$(cat "$PIDFILE" 2>/dev/null)
    # SIGTERM 会让 run_tun 正常返回，Gateway 的 Drop 才会回滚 ip rule/iptables
    kill "$pid" 2>/dev/null
    # 最多等 3 秒：这段等待会直接变成软件中心 POST 的响应时间
    # （页面虽然已改成异步，但等待越短体验越好）。
    i=0
    while [ $i -lt 30 ] && kill -0 "$pid" 2>/dev/null; do
        sleep 0.1 2>/dev/null || sleep 1
        i=$((i + 1))
    done
    if kill -0 "$pid" 2>/dev/null; then
        log_line "进程未在 3s 内退出，发送 SIGKILL"
        kill -9 "$pid" 2>/dev/null
    fi
    rm -f "$PIDFILE"
    # 无条件再清一次：优雅退出时由被控进程自己回滚是理想情况，
    # 但它卡死/被 SIGKILL 时就回滚不了，规则会永久留在内核里。
    cleanup_rules
    write_zero_status
    log_line "服务已停止"
    set_cfg last_act "已停止 $(date '+%m-%d %H:%M:%S')"
    return 0
}

start() {
    if is_running; then
        if is_healthy; then
            # 早退也必须补写 last_act / 拉起采样 / 同步定时任务：
            # 页面读的是 last_act，这里不写，页面就永远停在「正在启动…」，
            # 前端等不到结果 → 报「后台执行失败」，而隧道其实一直是好的。
            log_line "服务已在运行且健康（pid $(cat "$PIDFILE" 2>/dev/null)）"
            set_cfg last_act "已启动 $(date '+%m-%d %H:%M:%S')"
            start_status_loop
            register_cron
            return 0
        fi
        # 进程还在但 metrics 不通 = 僵死。它劫持的规则还指着 tun，
        # 必须先彻底拆掉（stop 会清 ip rule/iptables）再重新拉起，
        # 否则只是在黑洞上再叠一个。
        log_line "检测到进程僵死（metrics 无响应），强制重启"
        stop
    fi
    if [ ! -x "$BIN" ]; then
        log_line "错误：二进制不存在或不可执行：$BIN"
        set_cfg last_act "启动失败：缺少二进制"
        return 1
    fi

    uri=$(get_cfg uri)
    if [ -z "$uri" ]; then
        log_line "错误：未配置连接串（phantom_uri）"
        set_cfg last_act "启动失败：未配置连接串"
        return 1
    fi
    case "$uri" in
        phantom://*) ;;
        *)
            log_line "错误：连接串必须以 phantom:// 开头"
            set_cfg last_act "启动失败：连接串格式错误"
            return 1
            ;;
    esac

    # 域名必须解析成 IP（phantom 不做 DNS 解析）
    hp=$(uri_hostport "$uri")
    host="${hp%:*}"
    port="${hp##*:}"
    if [ -n "$host" ] && ! is_ip "$host"; then
        ip=$(resolve_host "$host")
        if [ -n "$ip" ] && [ "$ip" != "$host" ]; then
            log_line "服务端域名 ${host} 解析为 ${ip}"
            uri=$(uri_with_hostport "$uri" "${ip}:${port}")
            set_cfg uri "$uri"
        else
            log_line "警告：无法解析 ${host}，直接用域名启动很可能失败（请改为填写 IP）"
        fi
    fi

    proto=$(get_cfg protocol)
    [ -n "$proto" ] || proto="tcp"
    uri=$(uri_with_proto "$uri" "$proto")

    # 连接串含密钥，日志里只留 host 部分
    safe_uri=$(printf '%s' "$uri" | sed 's|^\(phantom://\)[^@]*@|\1***@|')
    log_line "启动 Phantom（模式 $(get_cfg mode)，协议 ${proto}，服务端 ${safe_uri}）"

    write_toml
    write_domains

    # OpenVPN 会在 Asuswrt 上带出 tun 模块，这里兜底加载一次
    [ -c /dev/net/tun ] || modprobe tun 2>/dev/null
    if [ ! -c /dev/net/tun ]; then
        log_line "错误：/dev/net/tun 不可用（请在 Web UI 开一次 OpenVPN 客户端或 insmod tun）"
        set_cfg last_act "启动失败：TUN 不可用"
        return 1
    fi

    log_level=$(get_cfg log_level)
    [ -n "$log_level" ] || log_level="info"

    ARGS=$(build_args "$uri")
    RUST_LOG="$log_level" \
    PHANTOM_PROXY_DOMAINS="$DOMAINS_FILE" \
    NO_COLOR=1 \
    "$BIN" $ARGS >>"$LOG" 2>&1 &
    echo $! >"$PIDFILE"

    # Hello 校验在装路由规则之前，给 3s 让失败能在这里暴露
    sleep 3
    if is_running; then
        log_line "启动成功（pid $(cat "$PIDFILE" 2>/dev/null)）"
        set_cfg last_act "已启动 $(date '+%m-%d %H:%M:%S')"
        start_status_loop
        register_cron
        return 0
    fi

    rm -f "$PIDFILE"
    log_line "启动失败，最近日志："
    tail -n 15 "$LOG" 2>/dev/null | while read -r l; do log_line "  $l"; done
    set_cfg last_act "启动失败 $(date '+%m-%d %H:%M:%S')，请看日志"
    return 1
}

start_status_loop() {
    [ -f "${SCRIPTS_DIR}/${module}_status.sh" ] || return 0
    stop_status_loop
    "$SH" "${SCRIPTS_DIR}/${module}_status.sh" >/dev/null 2>&1 &
    echo $! >"$STATUS_PIDFILE"
    return 0
}

restart() {
    stop
    sleep 1
    start
}

# ---------------------------------------------------------------- nat-start 兜底

# 转发面规则是否还在（ip rule + iptables 转发/DNS 劫持）。
# Asuswrt 在 nat-start 会重建 iptables，进程却还在跑 —— 那时插件显示「运行中」，
# LAN 流量却已经不再被导入隧道（表现为全屋断网/直连）。只有真缺规则才重启，
# 否则每次 NAT 重载都白抖一次全屋网络。
rules_installed() {
    lan_if=$(get_cfg lan_if); [ -n "$lan_if" ] || lan_if="br0"
    table=$(get_cfg table);   [ -n "$table" ] || table="200"
    tun_name=$(get_cfg tun_name); [ -n "$tun_name" ] || tun_name="phantom0"
    dns_hijack=$(get_cfg dns_hijack); [ -n "$dns_hijack" ] || dns_hijack="1"

    ip rule show 2>/dev/null | grep -qE "lookup[[:space:]]+${table}([[:space:]]|$)" || return 1
    for iface in $lan_if; do
        iptables -S FORWARD 2>/dev/null | grep -q -- "-i ${iface} -o ${tun_name} " || return 1
        iptables -S FORWARD 2>/dev/null | grep -q -- "-i ${tun_name} -o ${iface} " || return 1
        if [ "$dns_hijack" = "1" ]; then
            iptables -t nat -S PREROUTING 2>/dev/null \
                | grep -q -- "-i ${iface} -p udp --dport 53 -j DNAT" || return 1
        fi
    done
    return 0
}

start_nat() {
    if [ "$(get_cfg enable)" != "1" ]; then
        return 0
    fi
    if is_running && rules_installed; then
        log_line "nat-start：转发规则健在，无需重启"
        return 0
    fi
    log_line "nat-start：转发规则被冲掉（或进程已退出），重启补回"
    restart
}

# ---------------------------------------------------------------- 定时任务

register_cron() {
    [ -f "${SCRIPTS_DIR}/${module}_cron.sh" ] || return 0
    "$SH" "${SCRIPTS_DIR}/${module}_cron.sh" sync >/dev/null 2>&1
    return 0
}

# ---------------------------------------------------------------- 状态

status() {
    if is_running; then
        echo "phantom: running (pid $(cat "$PIDFILE" 2>/dev/null))"
        lan_if=$(get_cfg lan_if); [ -n "$lan_if" ] || lan_if="br0"
        table=$(get_cfg table);   [ -n "$table" ] || table="200"
        echo "--- ip rule ---"
        ip rule show 2>/dev/null | grep -E "lookup (main|${table})"
        echo "--- table ${table} ---"
        ip route show table "$table" 2>/dev/null
        echo "--- health ---"
        if is_healthy; then
            echo "healthy（metrics 可读）"
        else
            echo "STALE（进程在但 metrics 无响应 → 僵死，LAN 流量会被黑洞吞掉）"
        fi
        echo "--- metrics ---"
        fetch_metrics 2>/dev/null | grep '^phantom' || echo "(metrics 不可用)"
    else
        echo "phantom: not running"
        return 1
    fi
    return 0
}

# ---------------------------------------------------------------- action

apply() {
    # 本函数由软件中心的 POST 同步调用 —— 必须尽快返回，且不往 stdout 写东西
    PHANTOM_QUIET=1
    enable=$(get_cfg enable)
    if [ "$enable" = "1" ]; then
        set_cfg last_act "正在启动… $(date '+%m-%d %H:%M:%S')"
        log_line "配置已保存，后台启动隧道…"
        detach_run start
    else
        # 关闭是「用户等得起」的操作，但仍然只写日志、不回显
        stop
        set_cfg last_act "已保存并关闭 $(date '+%m-%d %H:%M:%S')"
    fi
    trim_log
    return 0
}

clear_log() {
    : >"$LOG" 2>/dev/null
    log_line "日志已清空"
    return 0
}

run_speedtest() {
    # 同样由 POST 同步调用：只做检查 + 派发，真正的下载放到后台
    # （5MB 样本在 3Mbps 链路上要十几秒，留在前台等于让页面等十几秒）
    PHANTOM_QUIET=1
    if [ ! -f "${SCRIPTS_DIR}/${module}_speedtest.sh" ]; then
        log_line "错误：缺少 phantom_speedtest.sh"
        return 1
    fi
    if ! is_running; then
        log_line "错误：隧道未运行，请先启动再测速"
        set_cfg last_act "测速失败：隧道未运行"
        return 1
    fi
    set_cfg speed_last "测速中… $(date '+%m-%d %H:%M:%S')"
    log_line "开始测速（经 SOCKS5 127.0.0.1:1080，测 phantom 转发吞吐上限）…"
    detach_run _speedtest
    return 0
}

# 后台实际执行测速
run_speedtest_now() {
    PHANTOM_QUIET=1
    "$SH" "${SCRIPTS_DIR}/${module}_speedtest.sh" >>"$LOG" 2>&1
    trim_log
    return 0
}

do_diag() {
    if [ -f "${SCRIPTS_DIR}/${module}_diag.sh" ]; then
        "$SH" "${SCRIPTS_DIR}/${module}_diag.sh"
    else
        echo "缺少 phantom_diag.sh"
    fi
}

# ---------------------------------------------------------------- 调用约定
#
# 两套调用方式都要认 —— 这里踩过一次「点提交卡死」，改之前先读完这段。
#
#   1) 直接调用（SSH / init.d / cron / watchdog）
#        /bin/sh phantom_config.sh <action> [arg]
#      init.d 的 S98phantom.sh 是软链，开机由 ks-wan-start.sh 以 `start` 调用；
#      `ks <0|1>` 是历史入口，真实固件上并不会被用到。
#
#   2) 软件中心 POST（页面点「提交」/「测速」/「清空日志」）
#        POST /_api/ {"id":N,"method":"phantom_config.sh","params":[1],"fields":{…}}
#      httpdb 先把 fields 写进 dbus，再执行
#        /koolshare/scripts/phantom_config.sh <id> 1
#      —— **$1 是请求 id，action 在 $2**。对照 ks_app_install.sh 的 `case $2 in`
#      与 clash_downyamlsel.sh 的 `case $2 in`；clash_getbasicyaml.sh 在 params
#      为空时仍然 `http_response $1`，可见 id 永远是第一个参数。
#
# 判据：$1 本身是已知 action 就当直接调用；否则只要 $2 是已知 action 就按
# 软件中心调用解析；再不然，只要 $1 是纯数字（请求 id 的形态）也按软件中心
# 调用处理——认不出 action 时同样先回包再报错，页面不会卡住。
KNOWN_ACTIONS="1 2 3 start_nat start stop restart status ks cron diag _speedtest _health"

is_known_action() {
    for _a in $KNOWN_ACTIONS; do
        [ "$1" = "$_a" ] && return 0
    done
    return 1
}

API_ID=""
ACTION="$1"
ARG2="$2"
if is_known_action "$1"; then
    : # 直接调用
elif [ -n "${2:-}" ] && is_known_action "$2"; then
    API_ID="$1"
    ACTION="$2"
    ARG2="${3:-}"
elif printf '%s' "$1" | grep -qE '^[0-9]{1,12}$'; then
    # 软件中心的请求 id 一定是纯数字。哪怕 action 认不出来，也必须回包 ——
    # 不回包页面就会一直等到 httpdb 超时，用户看到的是「卡住 + 后台执行失败」。
    API_ID="$1"
    ACTION="$2"
    ARG2="${3:-}"
fi

# 软件中心的响应体只认 JSON，脚本的 stdout 一律不往管道里写
[ -n "$API_ID" ] && PHANTOM_QUIET=1

# 回包必须在任何耗时动作之前，否则页面会一直等到 httpdb 超时
api_ack

case "$ACTION" in
    1)        apply ;;
    2)        clear_log ;;
    3)        run_speedtest ;;
    # 内部入口：由 detach_run 在后台调用，不要直接执行
    _speedtest) run_speedtest_now ;;
    # 内部入口：给看门狗用，判断进程是「真活着」还是「僵死」（只能输出一个词）
    _health)
        if is_healthy; then echo healthy; else echo unhealthy; fi
        exit 0
        ;;
    start)    start ;;
    stop)     stop ;;
    restart)  restart ;;
    status)   status ;;
    start_nat) start_nat ;;
    cron)     register_cron ;;
    diag)     do_diag ;;
    ks)
        # 兼容入口（真实开机路径见上文：S98phantom.sh start）
        if [ "$ARG2" = "1" ]; then
            if [ "$(get_cfg enable)" = "1" ]; then
                start
            else
                stop
            fi
        else
            stop
        fi
        ;;
    *)
        log_line "错误：未知动作 '${ACTION}'（软件中心调用：id=${API_ID:-无}）"
        [ "${PHANTOM_QUIET:-0}" = "1" ] \
            || echo "Usage: $0 {1|2|3|start|stop|restart|status|start_nat|cron|diag|ks <0|1>}"
        exit 1
        ;;
esac

trim_log
exit 0
