#!/usr/bin/env bash
# Phantom koolshare 插件 —— 真机在线核查（**纯只读**）
#
# 从开发机/同网段机器发起，通过 SSH 收集排障需要的信息。
# 不会启动隧道、不写任何配置、不动 ip rule / iptables，因此对 LAN 上
# 在线设备零影响。需要「真启动」验证时请在 Web 页面上操作。
#
# 用法（注意 ssh 的 -p 必须在 host 之前，写成 `ssh admin@host -p <SSH端口>` 是错的）：
#   bash client/koolshare/tools/live-check.sh --host <路由器IP> --port <SSH端口>
#
# 输出可直接整段回贴。最后会拿本机源码里的 ASP 与该机的对比（含皮肤 sed），
# 一眼看出「改了没推上去」。

set -uo pipefail

HOST=<路由器IP>
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
