#!/bin/sh
# Phantom 插件冒烟测试（无真机时的第一道防线）
#
# 模式一（推荐，需要容器引擎）：真实 busybox + 绝对路径
#   podman run --rm -v "$PWD:/src" -w /src alpine:3.19 \
#       sh client/koolshare/phantom/tests/smoke.sh
#
# 模式二（无容器引擎时）：假根前缀，把 /koolshare 等路径重写到临时目录
#   PHANTOM_SMOKE_ROOT=/tmp/phantom-smoke sh client/koolshare/phantom/tests/smoke.sh
#
# 安装形态：
#   PHANTOM_SMOKE_MODE=ks    软件中心模式（默认，装到 /koolshare，dbus 存配置）
#   PHANTOM_SMOKE_MODE=jffs  降级模式（无软件中心，装到 /jffs/phantom，文件存配置）
#
# 验证链路：安装 → 默认配置 → 皮肤 → 启动 → 状态采样 → 测速 →
#           定时任务 → 诊断 → 停止 → 卸载清理。
# 二进制用 tests/mock-bin/phantom 假替身，所以不依赖真机架构。

set -u

SRC_ROOT=$(cd "$(dirname "$0")/../../../.." && pwd)
PLUGIN_SRC="${SRC_ROOT}/client/koolshare/phantom"
MOCK_BIN_SRC="${PLUGIN_SRC}/tests/mock-bin"

PASS=0
FAIL=0

ok()   { PASS=$((PASS + 1)); echo "  [PASS] $*"; }
bad()  { FAIL=$((FAIL + 1)); echo "  [FAIL] $*"; }
check(){ if [ "$1" = "0" ]; then ok "$2"; else bad "$2"; fi; }
step() { echo ""; echo "== $*"; }

# BusyBox sed 与 BSD sed 的 -i 语法不兼容，统一走临时文件
sed_inplace() {
    sed "$1" "$2" >"$2.sedtmp" 2>/dev/null && mv "$2.sedtmp" "$2" || rm -f "$2.sedtmp"
}

# 见 phantom_config.sh：Asuswrt 上 /usr/sbin/sh 是 memaccess 的软链，不是 shell
if [ -x /bin/sh ]; then
    SH=/bin/sh
else
    SH=sh
fi

# ---------------------------------------------------------------- 模式

KS_DIR=/koolshare
WWW_TEMP=/www/_temp
SKIPD=/usr/bin/skipd
JFFS_DIR=/jffs
KS_MODE=ks
MODE=container

[ "${PHANTOM_SMOKE_MODE:-ks}" = "jffs" ] && KS_MODE=jffs

if [ -n "${PHANTOM_SMOKE_ROOT:-}" ]; then
    MODE=prefix
    ROOT="${PHANTOM_SMOKE_ROOT}"
    rm -rf "$ROOT"
    mkdir -p "$ROOT"
    KS_DIR="$ROOT/koolshare"
    WWW_TEMP="$ROOT/www/_temp"
    SKIPD="$ROOT/usr/bin/skipd"
    JFFS_DIR="$ROOT/jffs"
fi

if [ "$KS_MODE" = "jffs" ]; then
    # 降级安装：/jffs/phantom 下 res/ webs/ bin/ 与脚本同级
    INST_DIR="$JFFS_DIR/phantom"
    SCRIPTS_DIR_T="$INST_DIR"
    RUNTIME_T="$INST_DIR"
    UNINSTALL_T="$INST_DIR/uninstall.sh"
    HAVE_DBUS=0
else
    INST_DIR="$KS_DIR"
    SCRIPTS_DIR_T="$KS_DIR/scripts"
    RUNTIME_T="$KS_DIR/etc/phantom"
    UNINSTALL_T="$KS_DIR/scripts/uninstall_phantom.sh"
    HAVE_DBUS=1
fi
CONF_FILE_T="$RUNTIME_T/etc/phantom.conf"
CONF_SH="$SCRIPTS_DIR_T/phantom_config.sh"
# 运行期文件在 tmpfs 的 /tmp/upload（prefix 模式下脚本里的 /tmp 会被重写成 $ROOT/tmp）
# 必须放 /tmp/upload：httpd 不服务 docroot 下的 .txt，页面只能经 httpdb 的
# /_temp/ 读，而 /_temp/ 映射的正是 /tmp/upload。
if [ -n "${ROOT:-}" ]; then RUN_TMP="$ROOT/tmp"; else RUN_TMP="/tmp"; fi
RUN_LOG="$RUN_TMP/upload/phantom_log.txt"
RUN_STATUS="$RUN_TMP/upload/phantom_status.txt"
RUN_PID="$RUN_TMP/phantom.pid"
RUN_STATUS_PID="$RUN_TMP/phantom_status.pid"
LOG="$RUN_LOG"

echo "安装形态：${KS_MODE}   运行方式：${MODE}"

# ---------------------------------------------------------------- 配置读写

conf_read() {
    [ -f "$CONF_FILE_T" ] || return 0
    sed -n "s/^$1='\(.*\)'\$/\1/p" "$CONF_FILE_T" 2>/dev/null | head -n 1
}

conf_write() {
    key="$1"
    val=$(printf '%s' "$2" | tr -d "'")
    [ -f "$CONF_FILE_T" ] || { mkdir -p "$(dirname "$CONF_FILE_T")"; touch "$CONF_FILE_T"; }
    if grep -q "^${key}=" "$CONF_FILE_T" 2>/dev/null; then
        sed_inplace "s|^${key}=.*|${key}='${val}'|" "$CONF_FILE_T"
    else
        echo "${key}='${val}'" >>"$CONF_FILE_T"
    fi
}

getcfg() { if [ "$HAVE_DBUS" = "1" ]; then dbus get "phantom_$1" 2>/dev/null; else conf_read "$1"; fi; }
setcfg() { if [ "$HAVE_DBUS" = "1" ]; then dbus set "phantom_$1=$2" >/dev/null 2>&1; else conf_write "$1" "$2"; fi; }

# ---------------------------------------------------------------- 环境

step "准备模拟环境"
MOCK_BIN="$MOCK_BIN_SRC"
if [ "$KS_MODE" = "jffs" ]; then
    # 降级模式没有 skipd，所以 mock 目录里也不能有 dbus，
    # 否则插件会走 dbus 分支而测不到配置文件分支
    MOCK_BIN="${ROOT:-/tmp}/mock-bin-nodbus"
    rm -rf "$MOCK_BIN"
    mkdir -p "$MOCK_BIN"
    for m in "$MOCK_BIN_SRC"/*; do
        case "$(basename "$m")" in dbus) continue ;; esac
        cp -f "$m" "$MOCK_BIN/"
    done
    chmod 755 "$MOCK_BIN"/*
fi

mkdir -p "$JFFS_DIR" "$WWW_TEMP"
if [ "$KS_MODE" = "ks" ]; then
    mkdir -p "$KS_DIR/scripts" "$KS_DIR/webs" "$KS_DIR/res" "$KS_DIR/init.d" "$KS_DIR/bin"
    mkdir -p "$(dirname "$SKIPD")"
    touch "$SKIPD"
fi

if [ "$MODE" = "container" ]; then
    mkdir -p /dev/net 2>/dev/null
    [ -c /dev/net/tun ] || mknod /dev/net/tun c 10 200 >/dev/null 2>&1
    [ -c /dev/net/tun ] && ok "/dev/net/tun 就绪" || bad "/dev/net/tun 不可用"
    TUN_MARK=/dev/net/tun
else
    mkdir -p "$ROOT/dev/net"
    : >"$ROOT/dev/net/tun"
    TUN_MARK="$ROOT/dev/net/tun"
    ok "TUN 占位文件已建立（前缀模式用 -e 判定）"
fi

PATH="${MOCK_BIN}:${PATH}"
export PATH
# 插件的命令探测（cmd_path）优先按固定目录查找，测试里必须让它先看到 mock，
# 否则会命中系统自带的 curl 之类。真机上这个变量不设置，用脚本里的默认值。
export PHANTOM_BIN_DIRS="${MOCK_BIN}:/bin:/usr/bin:/sbin:/usr/sbin"
export MOCK_DBUS_FILE="${ROOT:-/tmp}/mock-dbus.txt"
export MOCK_CRON_FILE="${ROOT:-/tmp}/mock-cron.txt"
export MOCK_IP_FILE="${ROOT:-/tmp}/mock-ip.txt"
export MOCK_IPTABLES_FILE="${ROOT:-/tmp}/mock-iptables.txt"
# 脚本回包（/_resp/<id>）会被 mock curl 记到这里，用来断言软件中心契约
export PHANTOM_ACK_CAPTURE="${ROOT:-/tmp}/mock-ack.txt"
rm -f "$MOCK_DBUS_FILE" "$MOCK_CRON_FILE" "$MOCK_IP_FILE" "$MOCK_IPTABLES_FILE" \
      "$PHANTOM_ACK_CAPTURE" "${ROOT:-/tmp}/mock-metrics-n"

# 组装待安装的离线包（含 mock 二进制）
PKG="$(dirname "$INST_DIR")/pkg_phantom"
rm -rf "$PKG"
cp -rf "${PLUGIN_SRC}" "$PKG"
mkdir -p "$PKG/bin"
cp -f "${MOCK_BIN_SRC}/phantom" "$PKG/bin/phantom-aarch64"
cp -f "${MOCK_BIN_SRC}/phantom" "$PKG/bin/phantom-armv7"
chmod 755 "$PKG/bin"/phantom-* "$PKG/install.sh" "$PKG/uninstall.sh" "$PKG/scripts"/*.sh

if [ "$MODE" = "prefix" ]; then
    # 把脚本里的绝对路径重写到假根，避免测试污染真实 /koolshare、/tmp
    for f in "$PKG/install.sh" "$PKG/uninstall.sh" "$PKG/scripts"/*.sh; do
        # 注意顺序：/tmp/ 必须最先替换，否则后面插进去的 $ROOT 前缀会被二次替换
        sed -i.bak \
            -e "s|/tmp/|$ROOT/tmp/|g" \
            -e "s|/koolshare|$KS_DIR|g" \
            -e "s|/www/_temp|$WWW_TEMP|g" \
            -e "s|/usr/bin/skipd|$SKIPD|g" \
            -e "s|/jffs|$JFFS_DIR|g" \
            -e "s|\[ -c /dev/net/tun \]|[ -e $TUN_MARK ]|g" \
            -e "s|\[ ! -c /dev/net/tun \]|[ ! -e $TUN_MARK ]|g" \
            "$f"
        rm -f "$f.bak"
    done
    mkdir -p "$ROOT/tmp"
fi
ok "离线包已组装到 $PKG"

# ---------------------------------------------------------------- 安装

step "install.sh"
"$SH" "$PKG/install.sh" >"${ROOT:-/tmp}/install.log" 2>&1
check $? "install.sh 退出码为 0"
sed -n '1,25p' "${ROOT:-/tmp}/install.log"

for f in phantom_config.sh phantom_status.sh phantom_speedtest.sh phantom_cron.sh \
         phantom_diag.sh phantom_watchdog.sh; do
    [ -f "$SCRIPTS_DIR_T/$f" ] && ok "$f 已安装" || bad "$f 缺失"
done
[ -f "$INST_DIR/webs/Module_phantom.asp" ] && ok "Module_phantom.asp 已安装" || bad "Module_phantom.asp 缺失"
[ -f "$INST_DIR/res/phantom.css" ] && ok "phantom.css 已安装" || bad "phantom.css 缺失"
[ -x "$INST_DIR/bin/phantom" ] && ok "二进制已安装且可执行" || bad "二进制缺失"
[ -f "$RUNTIME_T/phantom.env" ] && ok "phantom.env 已生成" || bad "phantom.env 缺失"
if [ "$KS_MODE" = "ks" ]; then
    [ -L "$KS_DIR/init.d/S98phantom.sh" ] && ok "init.d 开机软链已建立" || bad "init.d 软链缺失"
    [ -L "$KS_DIR/init.d/N98phantom.sh" ] && ok "init.d nat-start 软链已建立（N98）" || bad "init.d nat-start 软链缺失"
else
    [ -f "$INST_DIR/etc/phantom.conf" ] && ok "降级模式配置文件已生成" || bad "配置文件缺失"
    grep -q "phantom_config.sh start" "$JFFS_DIR/scripts/services-start" 2>/dev/null \
        && ok "services-start 钩子已注册" || bad "services-start 钩子缺失"
    grep -q "phantom_config.sh start_nat" "$JFFS_DIR/scripts/nat-start" 2>/dev/null \
        && ok "nat-start 钩子已注册（start_nat：只在规则丢失时重启）" || bad "nat-start 钩子缺失"
    command -v dbus >/dev/null 2>&1 && bad "降级模式下 PATH 里不应有 dbus" || ok "降级模式无 dbus（走配置文件分支）"
fi

step "默认配置"
for key in enable mode lan_if tun_addr dns_hijack builtin_wl cron_enable cron_time watchdog log_level; do
    v=$(getcfg "$key")
    [ -n "$v" ] && ok "$key = $v" || bad "$key 无默认值"
done
[ "$(getcfg mode)" = "smart" ] && ok "默认模式为 smart" || bad "默认模式不是 smart"
[ "$(getcfg lan_if)" = "br0" ] && ok "默认 LAN 接口为 br0" || bad "默认 LAN 接口不是 br0"

step "页面自检"
ASP="$INST_DIR/webs/Module_phantom.asp"
# JS 里 params_inp / params_chk 列出的每个 id 都必须真实存在于 DOM：
# 少一个就会在 conf2obj() 里访问 null，表现为「页面点了没反应」，
# 而这种错误冒烟测试之外的任何检查都抓不到。
for list in params_inp params_chk; do
    missing=""
    for id in $(sed -n "/var $list = \[/,/\];/p" "$ASP" | grep -o "'phantom_[a-z_0-9]*'" | tr -d "'" | sort -u); do
        grep -q "id=\"$id\"" "$ASP" || missing="$missing $id"
    done
    if [ -n "$missing" ]; then
        bad "$list 引用了不存在的 DOM id:$missing"
    else
        ok "$list 的 DOM id 全部存在"
    fi
done
# 内联 JS 语法（有 node 才查，没装就跳过）
if command -v node >/dev/null 2>&1; then
    awk '/^<script>$/{f=1;next} /^<\/script>$/{f=0} f' "$ASP" >"${ROOT:-/tmp}/asp_inline.js"
    if node --check "${ROOT:-/tmp}/asp_inline.js" >/dev/null 2>&1; then
        ok "内联 JS 语法检查通过"
    else
        bad "内联 JS 语法错误：$(node --check "${ROOT:-/tmp}/asp_inline.js" 2>&1 | head -n 3 | tr '\n' ' ')"
    fi
fi

# 前端候选路径的第一条必须是 httpdb 的 /_temp/ 路由，且物理文件在 /tmp/upload：
# 本固件 httpd 不服务 docroot 下的 .txt（/phantom_status.txt 与固件自带的
# /Lang_Hdr.txt 同为 404），软链进 /koolshare/webs 的话页面永远读不到。
for pair in "STATUS_PATHS:/_temp/phantom_status.txt" "LOG_PATHS:/_temp/phantom_log.txt"; do
    var="${pair%%:*}"
    path="${pair#*:}"
    if grep -q "var ${var} = \['${path}'" "$ASP"; then
        ok "${var} 首选 httpdb 的 /_temp/ 路由（${path}）"
    else
        bad "${var} 首选路径不对（应为 ${path}）"
    fi
done

# 提交必须走异步 XHR：软件中心是同步执行后台脚本的（stop 最多等 3s + sleep 3），
# 同步 XHR 会把浏览器主线程一起冻住 —— 名字就叫「点提交后页面卡死没反应」。
awk '/^function post_action/,/^}/' "$ASP" >"${ROOT:-/tmp}/pa.js"
if grep -q 'async: true' "${ROOT:-/tmp}/pa.js"; then
    ok "post_action 使用异步 XHR"
else
    bad "post_action 未使用异步 XHR（提交会冻结页面）"
fi
if grep -q 'async: false' "${ROOT:-/tmp}/pa.js"; then
    bad "post_action 里仍有同步 XHR"
else
    ok "post_action 无同步 XHR"
fi

# 被软件中心 POST 同步调用的入口必须「立即返回 + 后台派发」。
# 曾经 apply() 在前台做 restart（stop 等 3s + sleep 3），而只要那个 HTTP 请求
# 不结束页面就一直是卡死的 —— 现在必须走 detach_run。
CFG="$SCRIPTS_DIR_T/phantom_config.sh"
if [ -f "$CFG" ]; then
    awk '/^apply\(\) \{/,/^\}/' "$CFG" >"${ROOT:-/tmp}/apply_fn.sh"
    if grep -q 'detach_run' "${ROOT:-/tmp}/apply_fn.sh"; then
        ok "apply() 通过 detach_run 后台启动（POST 立即返回）"
    else
        bad "apply() 未后台派发：POST 会一直阻塞，页面卡死"
    fi
    if grep -qE '^[[:space:]]*(restart|start)[[:space:]]*$' "${ROOT:-/tmp}/apply_fn.sh"; then
        bad "apply() 里仍有前台 restart/start"
    else
        ok "apply() 无前台 restart/start"
    fi
    awk '/^run_speedtest\(\) \{/,/^\}/' "$CFG" >"${ROOT:-/tmp}/speed_fn.sh"
    if grep -q 'detach_run' "${ROOT:-/tmp}/speed_fn.sh"; then
        ok "测速同样后台派发（不让 POST 等十几秒）"
    else
        bad "测速未后台派发"
    fi
fi

step "脚本自检：不允许 command -v / 裸 sh"
# 本固件的 busybox **没有 `command -v`（也没有 `type`）**。
# 用它做能力探测会得到 "command: not found"，判断静默失败 —— 曾经让
# 「是否能用 dbus」永远为假，插件退化成读空配置，表现为「提交后永不启动」。
bad_cv=""
for f in "$INST_DIR/install.sh" "$UNINSTALL_T" "$SCRIPTS_DIR_T"/phantom_*.sh; do
    [ -f "$f" ] || continue
    hits=$(grep -nE 'command[[:space:]]+-v' "$f" 2>/dev/null | grep -v ':[[:space:]]*#')
    [ -n "$hits" ] && bad_cv="${bad_cv}
--- ${f}
${hits}"
done
if [ -n "$bad_cv" ]; then
    bad "使用了 command -v（本固件不支持，会静默失败）：${bad_cv}"
else
    ok "未使用 command -v"
fi

step "脚本自检：无引号 heredoc 里不许出现反引号"
# 生成 phantom.env 的 heredoc 是**不带引号**的（内容要展开变量），里面的反引号
# 会被 ash 当命令替换真的执行一遍 —— 曾经把注释里的 `command -v` 跑了一次
# （真机报 "command: not found"）并把那段说明文字整个吃掉。
bt=$(awk '
    /<<-?[A-Z_][A-Z_0-9]*[ \t]*$/ {
        line=$0
        sub(/.*<<-?/, "", line)
        gsub(/[ \t]/, "", line)
        delim=line; in_h=1; next
    }
    in_h && $0 == delim { in_h=0; next }
    in_h && index($0, "`") > 0 { print FILENAME ":" FNR ": " $0 }
' "$INST_DIR/install.sh" "$UNINSTALL_T" "$SCRIPTS_DIR_T"/phantom_*.sh 2>/dev/null)
if [ -n "$bt" ]; then
    bad "无引号 heredoc 里有反引号（会被命令替换执行）：${bt}"
else
    ok "无引号 heredoc 里没有反引号"
fi

step "脚本自检：不允许裸 sh 调用"
# Asuswrt 上 /usr/sbin/sh 是指向 /bin/memaccess 的软链（不是 shell），
# 而 sshd/cron 的非交互 PATH 把 /usr/sbin 排在 /bin 前面 —— 真机上
# 任何裸 `sh xxx.sh` 都会跑到 memaccess 上。必须用 $SH（= /bin/sh）。
bare=""
for f in "$INST_DIR/install.sh" "$UNINSTALL_T" "$SCRIPTS_DIR_T"/phantom_*.sh; do
    [ -f "$f" ] || continue
    hits=$(grep -nE '(^|[^/.a-zA-Z0-9_-])sh[[:space:]]' "$f" 2>/dev/null \
           | grep -v ':[[:space:]]*#' | grep -v '/bin/sh')
    [ -n "$hits" ] && bare="${bare}
--- ${f}
${hits}"
done
if [ -n "$bare" ]; then
    bad "存在裸 sh 调用（真机会跑到 memaccess）：${bare}"
else
    ok "未发现裸 sh 调用"
fi

step "脚本自检：后台派发必须脱离进程组"
# 软件中心是同步执行后台脚本的：httpd 会等调用者所在的**整个进程组**结束，
# 超时后还会把那个进程组一起干掉。如果 detach_run 只用裸子 shell（`( ... & )`），
# 后台那个死循环的采样进程（phantom_status.sh）仍留在 httpd 的进程组里 →
#   1) POST 一直不返回 → 前端弹「后台执行失败」；
#   2) httpd 超时杀组时把正常工作的 tunnel 一并带走，留下 ip rule/iptables
#      残留在内核 → 全屋断网。
# 本机 busybox 没有编 setsid applet，只有 start-stop-daemon -b 能真正换进程组。
if [ -f "$CFG" ]; then
    awk '/^detach_run\(\) \{/,/^\}/' "$CFG" >"${ROOT:-/tmp}/detach_fn.sh"
    if grep -q '\$SSD' "${ROOT:-/tmp}/detach_fn.sh" \
       && grep -qE '\-S[[:space:]]+-b|^[[:space:]]*\-b[[:space:]]' "${ROOT:-/tmp}/detach_fn.sh"; then
        ok "detach_run 用 start-stop-daemon -b 后台化（子进程换新进程组）"
    else
        bad "detach_run 未用 start-stop-daemon -b：后台进程与 httpd 同组，POST 会超时并连带杀掉 tunnel"
    fi
    # -p 不给时 ssd 会按可执行名判重，而 /bin/sh 永远有进程在跑，它会直接
    # 报 "already running" 拒绝启动（真机实测过），所以 pidfile 是必需的。
    if grep -qE '\-p[[:space:]]+"?\$_ssdpid' "${ROOT:-/tmp}/detach_fn.sh"; then
        ok "detach_run 传了 pidfile（避开 ssd 按 /bin/sh 判重报 already running）"
    else
        bad "detach_run 未传 -p pidfile：ssd 会按可执行名判重，/bin/sh 永远在跑 → already running"
    fi
    if grep -qE '\-x[[:space:]]+"?\$SH' "${ROOT:-/tmp}/detach_fn.sh"; then
        ok "detach_run 用 \$SH 启动（不裸 sh）"
    else
        bad "detach_run 未用 \$SH 启动"
    fi
fi

step "皮肤适配（RT-AX86U Pro 应为 ASUSWRT）"
if grep -q "W3C rogcss" "$INST_DIR/webs/Module_phantom.asp"; then
    bad "ASUSWRT 皮肤应删除 rogcss 标记行"
else
    ok "ASUSWRT 皮肤已生效（rogcss 行已删除）"
fi
if grep -q "W3C rogcss" "$INST_DIR/res/phantom.css"; then
    bad "CSS 中仍残留 rogcss 标记行"
else
    ok "CSS 皮肤已切换"
fi

step "运行期文件与页面路径"
[ -n "$RUN_LOG" ] && ok "运行期日志在 tmpfs：$RUN_LOG" || bad "日志路径异常"
case "$RUN_LOG" in
    */upload/*) ok "运行期文件落在 /tmp/upload（httpdb 的 /_temp/ 映射目录）" ;;
    *) bad "运行期文件不在 /tmp/upload：httpd 不服务 docroot 下的 .txt，页面会读不到" ;;
esac
[ -f "$RUN_STATUS" ] && ok "初始状态文件已创建（首屏不会 404）" || bad "初始状态文件缺失"
if grep -q '"running":0' "$RUN_STATUS" 2>/dev/null; then
    ok "初始状态内容是合法的「未运行」文档"
else
    bad "初始状态文件内容异常"
fi
# 不应再往 docroot 铺 .txt 软链（那是本固件上恒 404 的死路）
if [ -e "$INST_DIR/webs/phantom_status.txt" ] || [ -L "$INST_DIR/webs/phantom_status.txt" ]; then
    bad "docroot 下仍铺了 .txt 软链（本固件 httpd 不服务 .txt，纯属噪音）"
else
    ok "docroot 下没有 .txt 软链（走 /_temp/ 通道）"
fi
# phantom.env 里不应再有已废弃的 PHANTOM_WWW_TEMP
if grep -q "PHANTOM_WWW_TEMP" "$RUNTIME_T/phantom.env" 2>/dev/null; then
    bad "phantom.env 仍带已废弃的 PHANTOM_WWW_TEMP"
else
    ok "phantom.env 已去掉 PHANTOM_WWW_TEMP"
fi
# 测速要走 SOCKS5，而固件自带的 curl 可能是 --disable-proxy 编译的，
# 所以必须单独探测一个带 SOCKS 的 curl 写进 phantom.env
if grep -q "^PHANTOM_CURL_SOCKS=" "$RUNTIME_T/phantom.env" 2>/dev/null; then
    cs=$(grep "^PHANTOM_CURL_SOCKS=" "$RUNTIME_T/phantom.env" | cut -d= -f2-)
    [ -n "$cs" ] && ok "已探测到带 SOCKS 支持的 curl：${cs}" || bad "PHANTOM_CURL_SOCKS 为空（测速会失败）"
else
    bad "phantom.env 缺少 PHANTOM_CURL_SOCKS（测速会因 curl 无 SOCKS 支持而失败）"
fi

# ---------------------------------------------------------------- 启动

step "配置并启动"
setcfg uri "phantom://AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA@1.2.3.4:443?psk=BBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB"
setcfg enable 1
"$SH" "$CONF_SH" 1 >"${ROOT:-/tmp}/apply.log" 2>&1
check $? "config.sh 1（应用并启动）退出码为 0"

sleep 1
[ -f "$RUN_PID" ] && ok "pidfile 已生成（${RUN_PID}）" || bad "pidfile 缺失"
[ -f "$RUNTIME_T/etc/phantom.toml" ] && ok "client.toml 已生成" || bad "client.toml 缺失"
[ -f "$RUNTIME_T/etc/proxy_domains.txt" ] && ok "白名单文件已生成" || bad "白名单文件缺失"

step "生成的 client.toml"
grep -q '^mode = "smart"' "$RUNTIME_T/etc/phantom.toml" && ok "mode 写入正确" || bad "mode 未写入"
if grep -q '\[\[servers\]\]' "$RUNTIME_T/etc/phantom.toml"; then
    bad "不应写 servers 段（服务器由 --server 提供）"
else
    ok "未写 servers 段，符合预期"
fi
grep -q 'builtin_proxy_whitelist = true' "$RUNTIME_T/etc/phantom.toml" && ok "内置白名单开关写入正确" || bad "内置白名单开关错误（TOML 需要 true/false）"

step "URI 处理与启动参数"
grep -q -- "--gateway" "$LOG" && ok "启动参数含 --gateway" || bad "启动参数缺少 --gateway"
grep -q -- "--lan-interface br0" "$LOG" && ok "启动参数含 --lan-interface br0" || bad "启动参数缺少 --lan-interface"
grep -q "1.2.3.4:443" "$LOG" && ok "服务端地址正确传递" || bad "服务端地址未传递"
grep -q "PHANTOM_PROXY_DOMAINS=$RUNTIME_T" "$LOG" && ok "PHANTOM_PROXY_DOMAINS 已注入" || bad "PHANTOM_PROXY_DOMAINS 未注入"
grep -q "NO_COLOR=1" "$LOG" && ok "NO_COLOR=1 已注入（日志无 ANSI）" || bad "NO_COLOR 未注入"
# mock 二进制会把收到的参数原样打印（真实二进制不会），所以只检查
# phantom 自己写的那行日志是否脱敏
if grep "启动 Phantom（" "$LOG" | grep -q "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA@"; then
    bad "日志中出现明文公钥，应脱敏"
else
    ok "日志中连接串已脱敏"
fi

step "QUIC 参数改写"
setcfg protocol quic
"$SH" "$CONF_SH" restart >/dev/null 2>&1
sleep 1
grep -q "proto=quic" "$LOG" && ok "URI 已追加 proto=quic" || bad "proto=quic 未写入 URI"
setcfg protocol tcp

step "状态采样"
"$SH" "$CONF_SH" restart >/dev/null 2>&1
sleep 5
if [ -f "$RUN_STATUS" ]; then
    ok "状态文件已生成"
    echo "  $(cat "$RUN_STATUS")"
    grep -q '"running":1' "$RUN_STATUS" && ok "状态为 running" || bad "状态不是 running"
    grep -q '"down_rate":[1-9]' "$RUN_STATUS" && ok "速率已算出（两次采样求差）" || bad "速率为 0，采样求差可能有问题"
else
    bad "状态文件缺失"
fi

step "软件中心调用约定（POST /_api/ 形态：\$1=请求 id，action 在 \$2）"
# 这里复现的就是「页面点提交」那条路径：httpdb 先把 fields 落 dbus，再执行
#   phantom_config.sh <id> <action>
# 并要求脚本 POST 回 /_resp/<id>，否则页面一直转圈、最后弹「后台执行失败」。
API_ACK="$PHANTOM_ACK_CAPTURE"
"$SH" "$CONF_SH" stop >/dev/null 2>&1
sleep 1
: >"$API_ACK"
"$SH" "$CONF_SH" 424242 1 >"${ROOT:-/tmp}/api-call.out" 2>&1
api_rc=$?
if [ "$api_rc" = "0" ]; then
    ok "软件中心形态 <id> 1 退出码为 0"
else
    bad "软件中心形态 <id> 1 退出码为 ${api_rc}（应先把 id 认出来）"
fi
grep -q "/_resp/424242\$" "$API_ACK" 2>/dev/null \
    && ok "已按约定回包 /_resp/<id>" || bad "没有回包 /_resp/<id>，页面会卡到超时"
grep -q "^424242\$" "$API_ACK" 2>/dev/null \
    && ok "回包 body 就是请求 id（前端按 result == id 判成功）" || bad "回包 body 不是请求 id"
if [ -s "${ROOT:-/tmp}/api-call.out" ]; then
    bad "软件中心调用往 stdout 写了内容（响应体只认 JSON）"
else
    ok "软件中心调用不往 stdout 写内容"
fi
i=0
while [ $i -lt 10 ]; do
    [ -f "$RUN_PID" ] && break
    sleep 1
    i=$((i + 1))
done
[ -f "$RUN_PID" ] && ok "软件中心形态确实拉起了隧道（而不是只回了个包）" \
    || bad "只回包没启动，页面会显示失败"

# 只给 id、没有 action：也必须回包（页面不卡），但要非零退出
: >"$API_ACK"
"$SH" "$CONF_SH" 555555 >/dev/null 2>&1
rc_noact=$?
grep -q "/_resp/555555\$" "$API_ACK" 2>/dev/null \
    && ok "只有 id 时仍回包（不会挂住页面）" || bad "只有 id 时不回包，页面会卡到超时"
[ "$rc_noact" != "0" ] && ok "只有 id 时非零退出" || bad "无法识别的调用应当非零退出"

# 认不出的 action 同样先回包再报错
: >"$API_ACK"
"$SH" "$CONF_SH" 666666 nosuchaction >/dev/null 2>&1
rc_bad=$?
grep -q "/_resp/666666\$" "$API_ACK" 2>/dev/null \
    && ok "未知 action 也先回包" || bad "未知 action 未回包"
[ "$rc_bad" != "0" ] && ok "未知 action 非零退出" || bad "未知 action 应当非零退出"

step "start_nat：只在转发规则丢失时重启"
setcfg enable 1
"$SH" "$CONF_SH" restart >/dev/null 2>&1
sleep 1
# 规则齐备：不应动进程
: >"$MOCK_IP_FILE"
: >"$MOCK_IPTABLES_FILE"
echo "rule iif br0 lookup 200 priority 32765" >>"$MOCK_IP_FILE"
echo "filter-FORWARD -i br0 -o phantom0 -j ACCEPT" >>"$MOCK_IPTABLES_FILE"
echo "filter-FORWARD -i phantom0 -o br0 -j ACCEPT" >>"$MOCK_IPTABLES_FILE"
echo "nat-PREROUTING -i br0 -p udp --dport 53 -j DNAT --to-destination 8.8.8.8:53" >>"$MOCK_IPTABLES_FILE"
pid_before=$(cat "$RUN_PID" 2>/dev/null)
"$SH" "$CONF_SH" start_nat >/dev/null 2>&1
sleep 1
pid_after=$(cat "$RUN_PID" 2>/dev/null)
if [ -n "$pid_before" ] && [ "$pid_before" = "$pid_after" ]; then
    ok "规则健在时不重启（pid 未变 ${pid_after}）"
else
    bad "规则健在却重启了（${pid_before} → ${pid_after}）"
fi
# 规则被冲掉（模拟 nat-start 重建 iptables）：必须重启补回
: >"$MOCK_IP_FILE"
: >"$MOCK_IPTABLES_FILE"
"$SH" "$CONF_SH" start_nat >/dev/null 2>&1
if tail -n 40 "$LOG" 2>/dev/null | grep -q "nat-start：转发规则被冲掉"; then
    ok "规则丢失时 start_nat 触发重启"
else
    bad "规则丢失时 start_nat 没有重启"
fi
sleep 1
pid_after2=$(cat "$RUN_PID" 2>/dev/null)
[ -n "$pid_after2" ] && ok "start_nat 之后隧道仍在跑（pid ${pid_after2}）" || bad "start_nat 之后隧道没了"

step "测速（后台派发，稍等结果）"
# 单位对齐检查：speed 是 bytes/s → kbps，天花板也要是 kbps。
# 曾经写成 kbps/（KB/s），真机上打印出「达到 786%」这种荒唐结论。
if awk '/^run_once\(\) \{/,/^\}/' "$SCRIPTS_DIR_T/phantom_speedtest.sh" >/dev/null 2>&1; then
    if grep -q 'limit_kbps' "$SCRIPTS_DIR_T/phantom_speedtest.sh" \
       && grep -q 'speed_kbps \* 100 / limit_kbps' "$SCRIPTS_DIR_T/phantom_speedtest.sh"; then
        ok "测速百分比单位对齐（kbps vs kbps）"
    else
        bad "测速百分比单位不对齐（曾出现过 786% 这种结果）"
    fi
fi
"$SH" "$CONF_SH" 3 >/dev/null 2>&1
# 测速已改成后台执行（不让 POST 等十几秒），所以这里要等结果落地
i=0
while [ $i -lt 10 ]; do
    grep -q "测速结果" "$LOG" 2>/dev/null && break
    sleep 1
    i=$((i + 1))
done
if grep -q "测速结果" "$LOG"; then
    ok "测速有结果输出"
    grep "测速结果" "$LOG" | tail -n 1 | sed 's/^/  /'
else
    bad "测速无结果（等待 ${i}s）"
fi
if [ "$HAVE_DBUS" = "1" ]; then
    sl=$(getcfg speed_last)
    case "$sl" in
        *MB/s*|*KB/s*) ok "测速结果已写入 dbus：${sl}" ;;
        *) bad "dbus speed_last 不是结果：${sl}" ;;
    esac
fi

step "定时任务"
setcfg cron_enable 1
"$SH" "$CONF_SH" cron >/dev/null 2>&1
cru l | grep -q phantom_watchdog && ok "看门狗定时任务已注册" || bad "看门狗定时任务缺失"
cru l | grep -q phantom_restart && ok "定时重启任务已注册" || bad "定时重启任务缺失"
setcfg cron_enable 0
"$SH" "$CONF_SH" cron >/dev/null 2>&1
if cru l | grep -q phantom_restart; then
    bad "关闭后定时重启任务仍存在"
else
    ok "关闭后定时重启任务已清理"
fi

step "诊断"
"$SH" "$CONF_SH" diag >"${ROOT:-/tmp}/diag.txt" 2>&1
grep -q "Phantom 诊断报告" "${ROOT:-/tmp}/diag.txt" && ok "诊断报告已生成（$(wc -l <"${ROOT:-/tmp}/diag.txt" | tr -d ' ') 行）" || bad "诊断报告生成失败"
if grep -q "uri      = phantom://\*\*\*@" "${ROOT:-/tmp}/diag.txt"; then
    ok "诊断报告对连接串脱敏"
else
    bad "诊断报告未脱敏"
fi

step "看门狗"
"$SH" "$SCRIPTS_DIR_T/phantom_watchdog.sh" >/dev/null 2>&1
ps -o pid= -p "$(cat "$RUN_PID" 2>/dev/null)" >/dev/null 2>&1 && ok "看门狗执行后进程仍在" || bad "进程丢失"

step "status 与 stop"
"$SH" "$CONF_SH" status >"${ROOT:-/tmp}/status.txt" 2>&1
grep -q "running" "${ROOT:-/tmp}/status.txt" && ok "status 报告运行中" || bad "status 未报告运行中"
"$SH" "$CONF_SH" stop >/dev/null 2>&1
sleep 1
[ -f "$RUN_PID" ] && bad "stop 后 pidfile 仍存在" || ok "stop 后 pidfile 已清理"
[ -f "$RUN_STATUS_PID" ] && bad "stop 后采样循环仍在" || ok "stop 后采样循环已退出"
grep -q '"running":0' "$RUN_STATUS" 2>/dev/null && ok "stop 后状态已重置为未运行" || bad "stop 后状态未重置为未运行"

# ---------------------------------------------------------------- 卸载

step "uninstall.sh"
# 排障用：PHANTOM_SMOKE_KEEP=1 时保留整棵假根，方便事后查看日志与产物
if [ -n "${PHANTOM_SMOKE_KEEP:-}" ]; then
    rm -rf "${ROOT}-keep"
    cp -rf "$(dirname "$INST_DIR")" "${ROOT}-keep" 2>/dev/null
    echo "  (已保留 ${ROOT}-keep)"
fi
"$SH" "$UNINSTALL_T" >/dev/null 2>&1
[ -f "$CONF_SH" ] && bad "卸载后 config.sh 仍存在" || ok "config.sh 已删除"
[ -f "$INST_DIR/bin/phantom" ] && bad "卸载后二进制仍存在" || ok "二进制已删除"
[ -f "$INST_DIR/webs/Module_phantom.asp" ] && bad "卸载后 ASP 仍存在" || ok "ASP 已删除"
if [ "$KS_MODE" = "ks" ]; then
    [ -L "$KS_DIR/init.d/S98phantom.sh" ] && bad "卸载后 init.d 软链仍存在" || ok "init.d 软链已删除"
    [ -n "$(getcfg enable)" ] && bad "卸载后 dbus 键仍存在" || ok "dbus 键已清理"
    [ -n "$(dbus get softcenter_module_phantom_install 2>/dev/null)" ] && bad "卸载后 softcenter 键仍存在" || ok "softcenter 键已清理"
else
    [ -d "$INST_DIR" ] && bad "卸载后安装目录仍存在" || ok "安装目录已清除"
    grep -q "phantom_config.sh" "$JFFS_DIR/scripts/services-start" 2>/dev/null \
        && bad "卸载后 services-start 仍残留 phantom 行" || ok "services-start 钩子已清理"
fi

# ---------------------------------------------------------------- 汇总

echo ""
echo "================================"
echo " 通过: ${PASS}   失败: ${FAIL}"
echo "================================"
[ "$FAIL" = "0" ] || exit 1
exit 0
