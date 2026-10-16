#!/bin/sh
# Phantom 一键网络体检（页面运维页的「开始体检」按钮 → action health）
#
# 它回答的是那种「感觉不对但说不出哪里不对」的问题，逐条给 ✅/⚠️/❌ 和处置建议：
#   1) 分流实况      模式 / ip rule / iptables / connmark / ipset 条目
#   2) 国内解析器池  逐台 ping + 实测解析，并说明当前优先用哪台、切换过几次
#   3) 分流学习链    白名单 IP 是怎么学到的、有没有被打断（unknown_rst）
#   4) IPv6          路由器是不是双栈（双栈而隧道只跑 v4 就是泄漏面）
#   5) WebRTC/真实IP 说明为什么测试站会看到家宽地址（这是分流设计使然）
#   6) 首连接耗时    冷/暖两次经 SOCKS5 的请求（"第一次打开某站点顿一下"）
#   7) 性能快照      CPU / fd / conntrack / flow-cache / 隧道停滞 / 重复 ACK
#
# 结果写 $PHANTOM_HEALTH（默认 /tmp/upload/phantom_health.txt），页面经 httpdb 的
# /_temp/phantom_health.txt 读得到。只读：不改配置、不动 ip rule/iptables、不重启隧道。

module=phantom

for candidate in /koolshare/etc/phantom/phantom.env /jffs/phantom/phantom.env; do
    if [ -f "$candidate" ]; then
        . "$candidate"
        break
    fi
done

SCRIPTS_DIR="${PHANTOM_SCRIPTS_DIR:-/koolshare/scripts}"
RUNTIME_DIR="${PHANTOM_RUNTIME_DIR:-/koolshare/etc/phantom}"
CONF_FILE="${PHANTOM_CONF:-${RUNTIME_DIR}/etc/phantom.toml}"
OUT="${PHANTOM_HEALTH:-/tmp/upload/phantom_health.txt}"
PIDFILE="${PHANTOM_PIDFILE:-/tmp/phantom.pid}"

# 见 phantom_config.sh：本机 busybox 没有 command -v/type，一律按绝对路径探测；
# 也不能用裸 sh（/usr/sbin/sh 是 memaccess）。
PHANTOM_BIN_DIRS="${PHANTOM_BIN_DIRS:-/bin /sbin /usr/bin /usr/sbin /koolshare/bin /opt/bin /usr/local/bin}"
cmd_path() {
    _c="$1"
    for _d in $(printf '%s' "$PHANTOM_BIN_DIRS" | tr ":" " "); do
        [ -x "$_d/$_c" ] && { printf '%s' "$_d/$_c"; return 0; }
    done
    return 1
}

DBUS="${PHANTOM_DBUS:-}"
[ -n "$DBUS" ] && [ -x "$DBUS" ] || DBUS=$(cmd_path dbus)
USE_DBUS=0
if [ "${PHANTOM_KS:-1}" = "1" ] && [ -n "$DBUS" ] && "$DBUS" get ${module}_version >/dev/null 2>&1; then
    USE_DBUS=1
fi
get_cfg() {
    if [ "$USE_DBUS" = "1" ]; then
        "$DBUS" get "${module}_$1" 2>/dev/null
    else
        sed -n "s/^$1='\(.*\)'\$/\1/p" "${RUNTIME_DIR}/etc/phantom.conf" 2>/dev/null | head -n 1
    fi
}

PING=$(cmd_path ping)
NSLOOKUP=$(cmd_path nslookup)
CURL=$(cmd_path curl)
# 测速/首连接必须走 SOCKS5，而**固件自带的 curl 多为 --disable-proxy 编译**
# （一调 --socks5-hostname 就报 "proxy support is disabled"），所以优先找
# koolshare 随包带的 curl-fancyss。
SOCKS_CURL=""
for _c in /koolshare/bin/curl-fancyss /koolshare/bin/curl; do
    [ -x "$_c" ] && SOCKS_CURL="$_c" && break
done

[ -d "$(dirname "$OUT")" ] || mkdir -p "$(dirname "$OUT")" 2>/dev/null

# 输出收集：既写文件也（非静默时）回显
TMP="${OUT}.tmp"
: >"$TMP"
say() { printf '%s\n' "$*" >>"$TMP"; }

OK="[OK]"
WARN="[!!]"
BAD="[XX]"

say "Phantom 网络体检  $(date '+%Y-%m-%d %H:%M:%S')"
say "=================================================="

# ---------------------------------------------------------------- 运行状态
running=0
pid=""
if [ -f "$PIDFILE" ]; then
    pid=$(cat "$PIDFILE" 2>/dev/null)
    [ -n "$pid" ] && kill -0 "$pid" 2>/dev/null && running=1
fi
enable=$(get_cfg enable)
say ""
say "一、插件状态"
say "  期望(开关)：$([ "$enable" = "1" ] && echo 开 || echo 关)   实际(进程)：$([ "$running" = "1" ] && echo "运行中 pid=$pid" || echo 未运行)"
if [ "$enable" = "1" ] && [ "$running" = "1" ]; then
    say "  $OK 开关与进程一致（当前实例由 $(get_cfg run_trigger) 拉起）"
elif [ "$enable" != "1" ] && [ "$running" != "1" ]; then
    say "  $OK 已按开关关闭，无残留进程"
else
    say "  $BAD 期望与实际不一致：请看页面「状态核对」，必要时点【一键修复】"
fi

# ---------------------------------------------------------------- 分流
lan_if=$(get_cfg lan_if); [ -n "$lan_if" ] || lan_if="br0"
tun_name=$(get_cfg tun_name); [ -n "$tun_name" ] || tun_name="phantom0"
table=$(get_cfg table); [ -n "$table" ] || table="200"
gw_mode=$(get_cfg gateway_mode); [ -n "$gw_mode" ] || gw_mode="kernel-split"

say ""
say "二、分流实况"
say "  模式：$gw_mode   LAN：$lan_if   TUN：$tun_name   路由表：$table"
# 只报"规则在不在"是不够的：这条规则**带不带掩码**决定了「只有首个 SYN 进隧道、
# 后续包走直连」这个真机上出现过的故障（固件 QoS 会往同一个 mark 写自己的位，
# 无掩码匹配只在 mark 恰好为 1 时命中）。所以这里连形态一起报。
rule_line=$(ip rule show 2>/dev/null | grep "lookup ${table}" | head -n 1)
if [ -z "$rule_line" ]; then
    say "  $BAD 路由规则缺失：LAN 流量不会进隧道（点提交或等 nat-start 钩子补回）"
else
    fw_token=$(printf '%s' "$rule_line" | sed -n 's/.*fwmark \([^ ]*\).*/\1/p')
    rule_show=$(printf '%s' "$rule_line" | sed 's/^[0-9]*:[[:space:]]*//')
    case "$fw_token" in
        */*) say "  $OK 路由规则在位：$rule_show" ;;
        '') say "  $WARN 路由规则在位但没有 fwmark 匹配（兼容模式的通配 iif 规则）：$rule_show" ;;
        *) say "  $BAD 路由规则**缺少掩码**（fwmark $fw_token）：固件 QoS 的位会让匹配失效 —— 「首个 SYN 进隧道、后续包走直连」。装新包即可修复" ;;
    esac
fi
fwd_ok=0
iptables -S FORWARD 2>/dev/null | grep -q -- "-i ${lan_if} -o ${tun_name} " && \
iptables -S FORWARD 2>/dev/null | grep -q -- "-i ${tun_name} -o ${lan_if} " && fwd_ok=1
[ "$fwd_ok" = "1" ] && say "  $OK 转发规则在位（FORWARD 双向 ACCEPT）" \
    || say "  $BAD 转发规则缺失：LAN↔TUN 的流量会被默认策略丢掉"
mangle_s=$(iptables -t mangle -S PREROUTING 2>/dev/null)
if printf '%s' "$mangle_s" | grep -q -- "--match-set"; then
    say "  $OK 白名单标记规则在位（ipset → MARK）"
    # 同一类"规则在、语义不对"的坑：`--set-mark` 是全掩码，会抹掉固件的 QoS 位。
    if printf '%s' "$mangle_s" | grep -- "--match-set" | grep -qv -- "--set-xmark"; then
        say "  $WARN 标记用的是无掩码写法（--set-mark）：会覆盖固件 QoS 的 mark 位，且与 connmark 恢复值不匹配；装新包即可修复"
    fi
else
    say "  $BAD 白名单标记规则缺失（kernel-split 的核心规则）"
fi
if iptables -t mangle -S PREROUTING 2>/dev/null | grep -q -- "CONNMARK --restore-mark"; then
    say "  $OK 只标记新连接（connmark 生效）：会话中途学到的地址不会打断在途连接"
else
    say "  $WARN 未启用 connmark：逐包标记，地址中途入集时在途连接会被 RST 打断"
fi

# ---------------------------------------------------------------- metrics
body=""
[ -n "$CURL" ] && body=$("$CURL" -s --max-time 3 http://127.0.0.1:9150/metrics 2>/dev/null)
metric() { [ -n "$body" ] && printf '%s' "$body" | sed -n "s/^$1 \([0-9]\{1,\}\)\$/\1/p" | head -n 1; }

ipset_n=$(metric phantom_whitelist_ipset_entries)
if [ -n "$ipset_n" ] && [ "$ipset_n" -gt 0 ] 2>/dev/null; then
    say "  $OK 白名单 ipset 现有 $ipset_n 条（含内置 CIDR 与 DNS 学到的地址）"
else
    say "  $WARN 读不到 ipset 条目数（隧道未运行？）"
fi

# ---------------------------------------------------------------- 国内解析器池
pool=$(sed -n 's/^dns_direct = "\(.*\)"$/\1/p' "$CONF_FILE" 2>/dev/null | head -n 1)
[ -n "$pool" ] || pool=$(get_cfg dns_direct)
say ""
say "三、国内解析器池（不在白名单里的域名用它解析）"
say "  配置：$pool"
active=$(metric phantom_dns_direct_active)
failovers=$(metric phantom_dns_direct_failovers_total)
timeouts=$(metric phantom_dns_direct_timeouts_total)
[ -n "$active" ] || active=0
[ -n "$failovers" ] || failovers=0
[ -n "$timeouts" ] || timeouts=0
say "  当前优先第 $((active + 1)) 台；累计切换 $failovers 次，放弃 $timeouts 次"
pool_count=0
oldifs="$IFS"; IFS=','
for _e in $pool; do
    IFS="$oldifs"
    [ -n "$_e" ] || continue
    pool_count=$((pool_count + 1))
    _ip="${_e%%:*}"
    # ping：可达性与 RTT（DNS 本身是 UDP，ICMP 通不代表一定可解析，所以下面再实测一次）
    _rtt=""
    if [ -n "$PING" ]; then
        _p=$("$PING" -c 2 -W 2 "$_ip" 2>/dev/null | tail -3)
        _rtt=$(printf '%s' "$_p" | sed -n 's#.*= [0-9.]*/\([0-9.]*\)/.*#\1#p' | head -n 1)
        _loss=$(printf '%s' "$_p" | sed -n 's/.*, \([0-9]*\)% packet loss.*/\1/p' | head -n 1)
    fi
    _res=""
    if [ -n "$NSLOOKUP" ]; then
        if "$NSLOOKUP" www.baidu.com "$_ip" >/dev/null 2>&1; then _res="解析正常"; else _res="解析失败"; fi
    fi
    if [ -n "$_loss" ] && [ "$_loss" != "0" ]; then
        say "  $BAD $_e  ICMP 丢包 ${_loss}%   $_res"
    elif [ -n "$_rtt" ]; then
        say "  $OK $_e  RTT ${_rtt}ms  $_res"
    else
        say "  $WARN $_e  无 ICMP 应答（可能被禁 ping）  $_res"
    fi
    IFS=','
done
IFS="$oldifs"
if [ "$pool_count" -lt 2 ]; then
    say "  $WARN 只配了 $pool_count 台：这一家抽风时整屋普通域名都解析不出来，建议至少三台"
fi
if [ "$timeouts" -gt 0 ] 2>/dev/null; then
    say "  $WARN 有 $timeouts 次查询在整池重试后仍无应答（看日志里 direct DNS 的相关告警）"
fi

# ---------------------------------------------------------------- 学习链
say ""
say "四、分流学习链"
say "  白名单 IP 的三个来源：内置 CIDR（Telegram/Google 段）、DNS 实时学习、"
say "  以及默认开启的加密 DNS 拦截（DoT/DoH，逼客户端回来问路由器）"
block_doh=$(get_cfg block_doh); [ -n "$block_doh" ] || block_doh="1"
[ "$block_doh" = "1" ] && say "  $OK 加密 DNS 拦截已开启" \
    || say "  $WARN 加密 DNS 拦截已关闭：客户端自带 DoH/DoT 时，被墙域名可能一直学不到"
unknown_rst=$(metric phantom_tun_unknown_flow_rst_total); [ -n "$unknown_rst" ] || unknown_rst=0
if [ "$unknown_rst" -gt 0 ] 2>/dev/null; then
    say "  $WARN 非 SYN 包进了 TUN 被 RST 累计 $unknown_rst 次（地址中途入集/隧道被重启的历史痕迹）"
else
    say "  $OK 没有未知流被 RST"
fi
say "  想逐条看某个域名走了哪条解析：把「日志级别」临时改成 info，日志里会出现 route/dns route 行"

# ---------------------------------------------------------------- IPv6 / WebRTC
say ""
say "五、泄漏面（IPv6 / 真实 IP）"
ipv6_service=""
NV="$([ -x /usr/sbin/nvram ] && echo /usr/sbin/nvram || (cmd_path nvram || true))"
[ -n "$NV" ] && ipv6_service=$("$NV" get ipv6_service 2>/dev/null)
v6_addr=$(ip -6 addr show 2>/dev/null | grep -c "scope global")
if [ "$v6_addr" = "0" ]; then
    say "  $OK 路由器没有全局 IPv6 地址（ipv6_service='${ipv6_service:-未知}'）：不存在 IPv6 绕过隧道的路径"
else
    say "  $WARN 路由器有 $v6_addr 个全局 IPv6 地址，而隧道只承载 IPv4："
    say "      客户端会优先用 IPv6 直连，被墙站点表现为「打不开/转圈」，需要时请在路由器关闭 IPv6"
fi
say "  · WebRTC（browserleaks/webrtc）看到的是家宽公网 IP：这是「默认直连」策略的必然结果"
say "    —— STUN(3478/19302) 不在白名单里，走的是直连。需要隐藏时在浏览器里禁用 WebRTC。"
say "  · DNS 泄漏测试（browserleaks/dns）显示大陆国旗：未匹配域名本来就该由国内解析器解析，"
say "    这是分流设计；只有白名单域名的解析才经隧道出口（远端 8.8.8.8）。"

# ---------------------------------------------------------------- 首连接耗时
say ""
say "六、首连接耗时（经 SOCKS5，测「第一次打开某站点」的代价）"
if [ "$running" != "1" ]; then
    say "  $WARN 隧道未运行，跳过"
elif [ -z "$SOCKS_CURL" ]; then
    say "  $WARN 本机没有带 SOCKS 支持的 curl（固件自带多为 --disable-proxy），跳过"
else
    _i=1
    while [ "$_i" -le 2 ]; do
        _w=$("$SOCKS_CURL" -s -o /dev/null -m 20 \
            -w '%{time_connect} %{time_appconnect} %{time_starttransfer} %{http_code}' \
            --socks5-hostname 127.0.0.1:1080 https://www.google.com/generate_204 2>/dev/null)
        say "  第 $_i 次：$_w   (connect / tls / ttfb / http)"
        _i=$((_i + 1))
    done
    say "  解读：第 1 次明显慢于第 2 次是正常的（要建隧道会话）；"
    say "        若第 2 次仍在 500ms 以上，看上面「国内解析器池」与「性能快照」两节。"
fi

# ---------------------------------------------------------------- 性能快照
say ""
say "七、性能快照"
cpu_overall=$(awk '{print $1}' /proc/loadavg 2>/dev/null)
say "  负载(1min)：${cpu_overall:-?}   内存可用：$(awk '/MemAvailable/{print int($2/1024) " MB"}' /proc/meminfo 2>/dev/null)"
if [ -n "$pid" ] && [ "$running" = "1" ]; then
    fd_used=$(ls /proc/"$pid"/fd 2>/dev/null | wc -l | tr -d ' ')
    fd_limit=$(awk '/Max open files/ {print $4}' /proc/"$pid"/limits 2>/dev/null | head -n 1)
    say "  phantom fd：${fd_used:-?} / ${fd_limit:-?}"
fi
ct=$(cat /proc/sys/net/netfilter/nf_conntrack_count 2>/dev/null)
[ -n "$ct" ] && say "  conntrack：$ct 条"
if [ -f /proc/fcache/nflist ]; then
    _fc_total=$(wc -l < /proc/fcache/nflist 2>/dev/null | tr -d ' ')
    _fc_sw=$(grep -c 4294967295 /proc/fcache/nflist 2>/dev/null | tr -d ' ')
    [ -n "$_fc_total" ] && [ "$_fc_total" -ge 2 ] 2>/dev/null && _fc_total=$((_fc_total - 2))
    [ -n "$_fc_sw" ] || _fc_sw=0
    _fc_hw=$(( ${_fc_total:-0} - _fc_sw ))
    [ "$_fc_hw" -lt 0 ] && _fc_hw=0
    say "  flow cache：$_fc_hw / ${_fc_total:-0} 条流走硬件加速（直连流量是否走内核快路径）"
fi
tw=$(metric phantom_tun_write_wait_max_ms); txq=$(metric phantom_tun_txq_peak_bytes)
dup=$(metric phantom_dup_acks_total); stalls=$(metric phantom_flow_stall_drops_total)
tube=$(metric phantom_tunnel_bytes_down)
say "  隧道：写入最长等待 ${tw:-?}ms，写队列峰值 ${txq:-?}B，重复 ACK ${dup:-?}，停滞放弃 ${stalls:-?}"
if [ -n "$tube" ]; then
    say "  隧道下行累计 $((tube / 1048576)) MB"
fi
say ""
say "体检结束。要点："
say "  · 状态/规则/connmark 有一处 ❌ 就先修那一处，其余结论都可能被它带偏"
say "  · 国内解析器池至少三台；切换次数持续增长说明第一台不稳"
say "  · 无线卡顿看 phantom_perf.log 的 wifi24/5_txretrans 与 rxcrc 增量"

mv "$TMP" "$OUT" 2>/dev/null
chmod 644 "$OUT" 2>/dev/null

# 非静默时回显（页面走 action health 时是静默的，结果通过 /_temp/ 读）
[ "${PHANTOM_QUIET:-0}" = "1" ] || cat "$OUT" 2>/dev/null
exit 0
