#!/usr/bin/env bash
# Phantom koolshare 插件 —— 真机在线核查（**纯只读**）
#
# 从开发机/同网段机器发起，通过 SSH 收集排障需要的信息。
# 不会启动隧道、不写任何配置、不动 ip rule / iptables，因此对 LAN 上
# 在线设备零影响。需要「真启动」验证时请在 Web 页面上操作。
#
# 用法（注意 ssh 的 -p 必须在 host 之前，写成 `ssh admin@host -p <SSH端口>` 是错的）：
#   bash client/koolshare/tools/live-check.sh --host <路由器IP> [--port <SSH端口>]
#
# 输出可直接整段回贴。最后会拿本机源码里的 ASP 与该机的对比（含皮肤 sed），
# 一眼看出「改了没推上去」。

set -uo pipefail

# 仓库里不留真实地址：主机必须由 --host 或环境变量给出。
HOST="${PHANTOM_ROUTER_HOST:-}"
PORT=22
USER=admin

usage() {
    sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'
    exit 0
}

while [ $# -gt 0 ]; do
    case "$1" in
        --host) HOST="$2"; shift 2 ;;
        --port) PORT="$2"; shift 2 ;;
        --user) USER="$2"; shift 2 ;;
        -h|--help) usage ;;
        *) echo "未知参数：$1（用 --help 看用法）" >&2; exit 1 ;;
    esac
done

if [ -z "$HOST" ]; then
    echo "缺少路由器地址：请用 --host <路由器IP>（或环境变量 PHANTOM_ROUTER_HOST）" >&2
    exit 1
fi

# 本机源码里的 ASP（用来和真机对比指纹）
SELF_DIR=$(cd "$(dirname "$0")/.." && pwd)
LOCAL_ASP="${SELF_DIR}/phantom/webs/Module_phantom.asp"

echo "==> 目标 ${USER}@${HOST}:${PORT}（只读核查）"

# 必须用 /bin/sh：Asuswrt 上 /usr/sbin/sh 是指向 /bin/memaccess 的软链，
# 而 sshd 非交互会话的 PATH 把 /usr/sbin 排在 /bin 前面，裸 `sh` 会跑到
# Broadcom 内存调试工具上（报 "Address xxx is invalid" / 打印 dw/dh/db 用法）。
#
# 同样地，本机 busybox **没有 `command -v`（也没有 `type`）**：用它做能力探测
# 只会得到 "command: not found"，于是脚本把「装了 dbus / 有 cru」误判成缺失。
# 所以这里一律按绝对路径探测。
OUT=$(ssh -p "$PORT" \
    -o ConnectTimeout=8 \
    -o StrictHostKeyChecking=accept-new \
    -o BatchMode=yes \
    "${USER}@${HOST}" '/bin/sh -s' <<'REMOTE' 2>&1
# ⚠️ 这段远程脚本整体被外面的 `OUT=$(ssh …)` 捕获，bash **按括号配对扫描**命令替换的
# 内容：body 里出现不成对的 `)`（最典型的是 `case` 的 `1)`）就会提前闭合命令替换，
# 报出与被改行毫不相干的 "syntax error near unexpected token ;;"。
# 所以这里**只能用 if/elif**，不要写 case；真要写就跟 `)` 配对的 `(` 一起写。
# （踩过两次：5.5 的 case 与 5.7 的 case。）
module=phantom

find_exe() {
    for d in /usr/bin /bin /usr/sbin /sbin /koolshare/bin /opt/bin /usr/local/bin; do
        [ -x "$d/$1" ] && { printf '%s' "$d/$1"; return 0; }
    done
    return 1
}

echo ""
echo "########## 1. 机型 / 固件 / 内核 ##########"
echo "productid : $(nvram get productid 2>/dev/null)"
echo "odmpid    : $(nvram get odmpid 2>/dev/null)"
echo "extendno  : $(nvram get extendno 2>/dev/null)"
echo "buildno   : $(nvram get buildno 2>/dev/null)"
echo "uname     : $(uname -a 2>/dev/null)"
echo "shell     : $([ -f /bin/ash ] && echo ash || echo sh)"
echo "skipd     : $([ -f /usr/bin/skipd ] && echo /usr/bin/skipd || echo '(缺失)')"
echo "dbus      : $(find_exe dbus || echo '(缺失)')"
echo "curl      : $(find_exe curl || echo '(缺失)')"
echo "cru       : $(find_exe cru || echo '(缺失)')"
echo "TUN       : $([ -c /dev/net/tun ] && echo /dev/net/tun || echo '(不可用)')"

echo ""
echo "########## 2. 安装文件指纹（与包内比对） ##########"
if [ -f /koolshare/webs/Module_phantom.asp ]; then
    echo "ASP md5   : $(md5sum /koolshare/webs/Module_phantom.asp 2>/dev/null | awk '{print $1}')"
    echo "ASP 行数  : $(wc -l < /koolshare/webs/Module_phantom.asp 2>/dev/null | tr -d ' ')"
    # 注意：这里必须取变量值，早期版本用 `grep -c` 数行数，把 rev=3 报成 2
    echo "UI rev    : $(sed -n "s/.*PHANTOM_UI_REV *= *'\([0-9]*\)'.*/\1/p" /koolshare/webs/Module_phantom.asp 2>/dev/null | head -n 1)"
    echo "皮肤      : $(grep -q 'W3C rogcss' /koolshare/webs/Module_phantom.asp && echo 'ROG（保留 rogcss 行）' || echo 'ASUSWRT（已删 rogcss 行）')"
else
    echo "错误：/koolshare/webs/Module_phantom.asp 不存在"
fi
echo "--- 脚本 ---"
ls -l /koolshare/scripts/phantom_*.sh 2>/dev/null || echo "(无)"
echo "--- 二进制 ---"
ls -l /koolshare/bin/phantom 2>/dev/null || echo "(无)"
echo "   版本   : $(/koolshare/bin/phantom --version 2>&1 | head -n 1)"
echo "--- 安装布局 ---"
cat /koolshare/etc/phantom/phantom.env 2>/dev/null || echo "(无 phantom.env)"

echo ""
echo "########## 3. 运行期文件（页面经 httpdb 的 /_temp/ 读） ##########"
echo "物理目录 /tmp/upload :"
ls -l /tmp/upload/phantom_log.txt /tmp/upload/phantom_status.txt 2>/dev/null || echo "(无 —— 未启动或仍是旧版本布局)"
echo "状态内容 : $(head -c 200 /tmp/upload/phantom_status.txt 2>/dev/null)"
echo "历史死软链（应全空，本机 httpd 不服务 docroot 下的 .txt）:"
ls -l /koolshare/webs/phantom_status.txt /koolshare/webs/phantom_log.txt 2>/dev/null || echo "(无)"
ls -l /www/_temp/phantom_log.txt /www/_temp/phantom_status.txt 2>/dev/null || echo "(无)"
echo "httpdb   : $(netstat -lntp 2>/dev/null | grep -c ':3030 ') 个监听（应为 1，127.0.0.1:3030）"

echo ""
echo "########## 3.5 httpd 健康（失效会话会把它打崩） ##########"
# 本固件的 httpd 在「登录会话已失效」时收到走 httpdb 通道的请求（/_api/、
# /_temp/）会直接 SIGSEGV，watchdog 约 20-30 秒后 start_httpd 把它拉起来。
# 崩溃瞬间的在途请求全部被重置 —— 页面点「提交」就是这个症状：弹
# 「提交失败（请求未完成）」，而 fields 根本没进 dbus（日志页也没有新行）。
#
# 插件侧已改成「.asp 会话预检 + 失效即停」：探针页不经过 httpdb，会话没了
# 就停掉全部轮询，不再反复把 httpd 打崩。这里用来确认现场状态。
echo "http_autologout : $(nvram get http_autologout 2>/dev/null) 分钟（ASUS 无操作自动登出）"
# 探针页名字必须是 Module_* —— httpd 只把 /Module_* 路由到 /koolshare/webs，
# 早期叫 phantom_ping.asp 的那个恒 404。
echo "会话探针页      : $([ -f /koolshare/webs/Module_phantom_ping.asp ] && echo /koolshare/webs/Module_phantom_ping.asp || echo '(缺失 —— 旧版页面没有会话预检，失效会话会打崩 httpd)')"
if [ -f /tmp/syslog.log ]; then
    crashes=$(grep -c 'Comm: httpd' /tmp/syslog.log 2>/dev/null)
    echo "httpd 崩溃次数  : ${crashes:-0}"
    if [ "${crashes:-0}" -gt 0 ]; then
        echo "最近一次崩溃    : $(grep 'Comm: httpd' /tmp/syslog.log 2>/dev/null | tail -n 1)"
    fi
    echo "watchdog 重启   : $(grep -c 'watchdog: start httpd' /tmp/syslog.log 2>/dev/null) 次"
    if [ "${crashes:-0}" -gt 0 ]; then
        echo "提示: 有崩溃记录。计数仍在增长 = 还有页面/App 拿着失效会话在轮询；"
        echo "      重新登录路由器 Web（必要时 killall httpd 让 watchdog 起一个干净的）即可收敛。"
    fi
else
    echo "httpd 崩溃次数  : (无 /tmp/syslog.log，跳过)"
fi

echo ""
echo "########## 4. 进程 / 钩子 / 规则残留（未启用时应全空） ##########"
pid=$(cat /tmp/phantom.pid 2>/dev/null)
echo "pidfile   : ${pid:-（无）}"
[ -n "$pid" ] && { kill -0 "$pid" 2>/dev/null && echo "进程      : running" || echo "进程      : 已退出"; }
ps 2>/dev/null | grep "[p]hantom" || echo "(无 phantom 进程)"
echo "init.d    :"
ls -l /koolshare/init.d/S98phantom.sh /koolshare/init.d/N98phantom.sh 2>/dev/null || echo "(无)"
echo "--- ip rule ---"
ip rule show 2>/dev/null | grep -E "lookup (main|200)" || echo "(无相关规则)"
echo "--- table 200 ---"
ip route show table 200 2>/dev/null || echo "(空)"
echo "--- iptables（残留会让「已停止」也断网）---"
iptables -S FORWARD 2>/dev/null | grep -i phantom || echo "(FORWARD 无 phantom)"
iptables -t nat -S PREROUTING 2>/dev/null | grep -i phantom || echo "(nat 无 phantom)"
echo "--- TUN ---"
ip link show phantom0 2>/dev/null | head -n 1 || echo "(无 phantom0)"

echo ""
echo "########## 5. 定时任务 ##########"
CRU=$(find_exe cru)
if [ -n "$CRU" ]; then
    "$CRU" l 2>/dev/null | grep phantom || echo "(无 phantom 定时任务)"
else
    echo "(没有 cru，插件会回退 crontab)"
fi

echo ""
echo "########## 5.5 开关真相（期望 vs 实际，关就是关） ##########"
# 真机上出现过「关掉插件 → 重启路由器 → 自己又开了」，而页面完全看不出来。
# 三条路径：开机钩子 start / cron 的 restart / 页面只显示开关值。插件侧已修，
# 这里用来核对现场：期望（dbus）与实际（进程 + 路由规则 + cron）必须一致。
DBUS0=$(find_exe dbus)
enable=$("$DBUS0" get phantom_enable 2>/dev/null)
trigger=$("$DBUS0" get phantom_run_trigger 2>/dev/null)
echo "phantom_enable  : ${enable:-（空）}"
echo "run_trigger     : ${trigger:-（空 —— 旧版本或从未启动）}"
if [ -f /tmp/phantom.pid ] && kill -0 "$(cat /tmp/phantom.pid 2>/dev/null)" 2>/dev/null; then
    proc_state="running"
else
    proc_state="stopped"
fi
echo "进程实际        : $proc_state"
if ip rule show 2>/dev/null | grep -qE "lookup[[:space:]]+200([[:space:]]|$)"; then
    rules_state="规则在位"
else
    rules_state="无规则"
fi
echo "规则实际        : $rules_state"
# 注意：这段远程脚本整体被外面的 `OUT=$(ssh …)` 捕获，bash 会按**括号配对**扫描
# 命令替换的内容 —— 所以这里不能出现不成对的 `)`（`case 1)` 就会提前闭合命令替换，
# 报出莫名其妙的 "syntax error near unexpected token ;;"）。用 if/elif 代替 case。
if [ "${enable:-0}" = "1" ]; then
    if [ "$proc_state" = "running" ]; then
        echo "结论            : 期望开、实际开 —— 一致"
    else
        echo "结论            : 期望开、但进程没起来（看日志最后一条动作）"
    fi
elif [ "$proc_state" = "running" ] || [ "$rules_state" = "规则在位" ]; then
    echo "结论            : **期望关、但仍有残留** —— 页面点「一键修复」或"
    echo "                  /bin/sh /koolshare/scripts/phantom_config.sh cleanup"
else
    echo "结论            : 期望关、实际关 —— 一致"
fi
if [ -n "$CRU" ] && [ "${enable:-0}" != "1" ] && "$CRU" l 2>/dev/null | grep -q "phantom_restart"; then
    echo "定时重启        : **插件已关闭却还挂着 phantom_restart**（明天到点会自己开回来）"
fi

echo ""
echo "########## 5.6 国内解析器池 / 分流内核侧 ##########"
echo "dns_direct      : $("$DBUS0" get phantom_dns_direct 2>/dev/null)"
echo "--- mangle PREROUTING（connmark = 只标记新连接）---"
iptables -t mangle -S PREROUTING 2>/dev/null | head -n 8 || echo "(读不到)"
if iptables -t mangle -S PREROUTING 2>/dev/null | grep -q "CONNMARK --restore-mark"; then
    echo "connmark        : 已启用（在途连接不会被中途入集的地址打断）"
else
    echo "connmark        : 未启用（逐包标记：地址中途入集时在途连接会被 RST）"
fi
echo "--- ipset ---"
ipset list phantom_proxy 2>/dev/null | head -n 8 || echo "(无 phantom_proxy)"
echo "--- 逐台实测（走物理网卡，不经隧道）---"
NS=$(find_exe nslookup)
PG=$(find_exe ping)
POOL=$("$DBUS0" get phantom_dns_direct 2>/dev/null)
[ -n "$POOL" ] || echo "  (dbus 无 phantom_dns_direct —— 这是升级前的旧版本；装上带解析器池的版本后这里会逐台实测)"
for ip in $(echo "$POOL" | tr ',' ' '); do
    host=${ip%%:*}
    [ -n "$host" ] || continue
    rtt=""
    [ -n "$PG" ] && rtt=$("$PG" -c 2 -W 2 "$host" 2>/dev/null | tail -3 | sed -n 's#.*= [0-9.]*/\([0-9.]*\)/.*#\1#p' | head -n 1)
    res=""
    if [ -n "$NS" ]; then
        "$NS" www.baidu.com "$host" >/dev/null 2>&1 && res="解析正常" || res="解析失败"
    fi
    echo "  $ip  rtt=${rtt:-?}ms  $res"
done

echo ""
echo "########## 5.7 性能采样（v2 表头）与无线/以太口 ##########"
if [ -f /tmp/upload/phantom_perf.log ]; then
    echo "表头  : $(head -n 1 /tmp/upload/phantom_perf.log)"
    echo "行数  : $(wc -l < /tmp/upload/phantom_perf.log | tr -d ' ')"
    echo "最近 3 行:"
    tail -n 3 /tmp/upload/phantom_perf.log
    # busybox 的 stat 不一定支持 -c：取不到就别报新鲜度，别用 0 算出天文数字。
    # 同样不能用 case —— 见上面「不能出现不成对右括号」的说明。
    mtime=$(stat -c %Y /tmp/upload/phantom_perf.log 2>/dev/null)
    if [ -n "$mtime" ] && [ "$mtime" -gt 0 ] 2>/dev/null; then
        echo "新鲜度: $(( $(date +%s) - mtime ))s 前更新（cron 每 5 分钟一次；明显更久说明采样任务没在跑）"
    else
        echo "新鲜度: (本机 stat 取不到 mtime，按上一行时间戳自行判断)"
    fi
else
    echo "(无 phantom_perf.log —— 旧版本或 cron 未注册)"
fi
echo "--- 无线空口（2.4G=eth6 / 5G=eth7）---"
WL=$(find_exe wl)
if [ -n "$WL" ]; then
    for i in eth6 eth7; do
        n=$("$WL" -i "$i" assoclist 2>/dev/null | wc -l | tr -d ' ')
        c=$("$WL" -i "$i" counters 2>/dev/null | grep -E "^(txframe|txretrans|rxcrc) ")
        echo "  $i  客户端=${n}"
        printf '%s\n' "$c" | sed 's/^/    /'
    done
else
    echo "(无 wl)"
fi
echo "--- 以太口 drop/err（eth0=WAN，eth2 真机上只到 100Mb/s）---"
# /proc/net/dev 的列：$1=face:  $2..$9=RX(bytes packets errs drop fifo frame compressed multicast)
#                       $10..$17=TX(bytes packets errs drop fifo colls carrier compressed)
cat /proc/net/dev 2>/dev/null | grep -E "eth0|eth2|eth7" | awk '{printf "  %-6s rx_err=%s rx_drop=%s tx_err=%s tx_drop=%s\n", $1, $4, $5, $11, $12}'

echo ""
echo "########## 6. 脚本语法自检 ##########"
fail=0
for f in /koolshare/scripts/phantom_*.sh; do
    [ -f "$f" ] || continue
    /bin/sh -n "$f" >/dev/null 2>&1 || { echo "SYNTAX FAIL: $f"; fail=1; }
done
[ "$fail" = "0" ] && echo "全部通过"

echo ""
echo "########## 7. 空间 ##########"
df -h /koolshare /jffs 2>/dev/null | tail -n 3
du -sh /koolshare/etc/phantom 2>/dev/null

echo ""
echo "########## 8. 日志尾部 ##########"
tail -n 30 /tmp/upload/phantom_log.txt 2>/dev/null || echo "(无日志)"

echo ""
echo "########## 9. dbus 键 ##########"
DBUS=$(find_exe dbus)
"$DBUS" list phantom 2>/dev/null | grep -v '^phantom_uri=' | head -n 40
echo "uri       : $("$DBUS" get phantom_uri 2>/dev/null | sed 's|^\(phantom://\)[^@]*@|\1***@|; s|psk=[^&]*|psk=***|')"

echo ""
echo "########## 核查结束 ##########"
REMOTE
)
rc=$?
printf '%s\n' "$OUT"

# ---------------------------------------------------------------- 本机对比

echo ""
echo "==> 与本地源码对比（皮肤 sed 之后再比，避免误报）"
if [ -f "$LOCAL_ASP" ]; then
    local_rev=$(sed -n "s/.*PHANTOM_UI_REV *= *'\([0-9]*\)'.*/\1/p" "$LOCAL_ASP" | head -n 1)
    remote_rev=$(printf '%s\n' "$OUT" | sed -n 's/^UI rev *: *\([0-9]*\).*/\1/p' | head -n 1)
    echo "本机 UI rev : ${local_rev:-?}"
    echo "真机 UI rev : ${remote_rev:-?}"
    [ -n "$local_rev" ] && [ "$local_rev" = "$remote_rev" ] \
        && echo "【一致】页面已是最新" \
        || echo "【不一致】页面没推上去 —— 重新 scp webs/Module_phantom.asp，再浏览器 Ctrl+F5"

    remote_md5=$(printf '%s\n' "$OUT" | sed -n 's/^ASP md5 *: *\([0-9a-f]*\).*/\1/p' | head -n 1)
    if [ -n "$remote_md5" ]; then
        # install.sh 按机型删掉非本机的皮肤行（本机 RT-AX86U Pro → ASUSWRT）
        local_md5=$(sed '/rogcss/d' "$LOCAL_ASP" | md5 2>/dev/null | awk '{print $NF}')
        [ -n "$local_md5" ] || local_md5=$(sed '/rogcss/d' "$LOCAL_ASP" | md5sum 2>/dev/null | awk '{print $1}')
        echo "本机 ASP md5（ASUSWRT 皮肤）: ${local_md5:-?}"
        echo "真机 ASP md5               : ${remote_md5}"
        [ -n "$local_md5" ] && [ "$local_md5" = "$remote_md5" ] \
            && echo "【一致】ASP 内容与本地源码一致" \
            || echo "【不一致】真机 ASP 与本地源码不同（若本机是 ROG/TUF 机型属正常）"
    fi
else
    echo "（找不到本地 ${LOCAL_ASP}，跳过对比）"
fi

echo "==> ssh exit=${rc}"
