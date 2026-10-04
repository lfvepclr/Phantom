<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd">
<html xmlns="http://www.w3.org/1999/xhtml">
<html xmlns:v>
<head>
<meta http-equiv="X-UA-Compatible" content="IE=Edge"/>
<meta http-equiv="Content-Type" content="text/html; charset=utf-8" />
<meta HTTP-EQUIV="Pragma" CONTENT="no-cache">
<meta HTTP-EQUIV="Expires" CONTENT="-1">
<link rel="shortcut icon" href="images/favicon.png">
<link rel="icon" href="images/favicon.png">
<title>Phantom</title>
<link rel="stylesheet" type="text/css" href="index_style.css"/>
<link rel="stylesheet" type="text/css" href="form_style.css"/>
<link rel="stylesheet" type="text/css" href="usp_style.css"/>
<link rel="stylesheet" type="text/css" href="css/element.css">
<link rel="stylesheet" type="text/css" href="res/softcenter.css">
<link rel="stylesheet" type="text/css" href="res/phantom.css">
<script language="JavaScript" type="text/javascript" src="/js/jquery.js"></script>
<script language="JavaScript" type="text/javascript" src="/state.js"></script>
<script language="JavaScript" type="text/javascript" src="/popup.js"></script>
<script language="JavaScript" type="text/javascript" src="/help.js"></script>
<script language="JavaScript" type="text/javascript" src="/general.js"></script>
<script language="JavaScript" type="text/javascript" src="/res/softcenter.js"></script>
<style>
	.show-btn1, .show-btn2, .show-btn3 {
		font-size:10pt;
		color: #fff;
		padding: 10px 3.75px;
		border-radius: 5px 5px 0px 0px;
		width:8.42%;
		border-left: 1px solid #67767d;
		border-top: 1px solid #67767d;
		border-right: 1px solid #67767d;
		border-bottom: none;
		background: #67767d;
		border: 1px solid #91071f; /* W3C rogcss */
		background: none; /* W3C rogcss */
	}
	.show-btn1:hover, .show-btn2:hover, .show-btn3:hover, .active {
		border: 1px solid #2f3a3e;
		background: #2f3a3e;
		border: 1px solid #91071f; /* W3C rogcss */
		background: #91071f; /* W3C rogcss */
	}
	#log_content{
		outline: 1px solid #222;
		width:748px;
	}
	#phantom_switch, #tablet_1, #tablet_2, #phantom_log, #tablet_3 { border:1px solid #67767d; } /* W3C asuscss */
	#phantom_switch, #tablet_1, #tablet_2, #phantom_log, #tablet_3 { border:1px solid #91071f; } /* W3C rogcss */
	.input_option{
		vertical-align:middle;
		font-size:12px;
	}
	input[type=button]:focus {
		outline: none;
	}
</style>
<script>
// 前端修订号：页面右上角会显示，用来确认浏览器加载的是哪一版
// （改了 UI 就 +1，排查「改了没生效」时先看这个数字）
var PHANTOM_UI_REV = '8';
var dbus = {};
var _responseLen;
var noChange = 0;
var _statusTimer = null;
var _logTimer = null;
var _speedTimer = null;
var _startedTimer = null;

// ---------------------------------------------------------------- 会话预检
//
// **本固件有一枚地雷：httpd 在「登录会话已失效」时收到走 httpdb 通道的请求
// （/_api/…、/_temp/…）会直接 SIGSEGV。** 真机 syslog 里躺着 130 多次
// `Comm: httpd` 崩溃 + watchdog 反复 start_httpd；用户点「提交」那一刻正好崩了
// 一次，浏览器只看到「提交失败（请求未完成）」，而配置根本没进 dbus。
//
// 崩溃还会自我维持：httpd 一崩，watchdog 重启它 → 所有旧 token 一起失效 →
// 仍持有旧 token 的客户端下一次请求再把它打崩。本页每 2 秒轮询状态文件、每
// 5 秒轮询上次动作，正是这台「人肉 DoS」的主力；后台标签页被浏览器限流成
// 1 分钟一次，恰好解释了真机上「每分钟崩一次、连续两小时」的观测。
//
// 唯一安全的通道是 .asp：带失效 cookie 请求它只会拿到跳转登录页的 HTML，
// 不会崩。所以每次访问 httpdb 之前都先打一发探针，探针说会话没了就彻底停手。
var SESSION_LOST = false;
// 探针候选：第一条是专门的探针页，第二条是管理页本体（一定存在）。
// **文件名必须以 Module_ 开头** —— httpd 只把 /Module_* 路由到 /koolshare/webs
// （strings 里的 isWebServer / websApply / Module_）；实测 /phantom_ping.asp 直接
// 404，而 /Module_xxx 会被它的 webs 处理器接管。
var PING_PATHS = ['/Module_phantom_ping.asp', '/Module_phantom.asp'];
var PING_MARK = 'phantom-ping-ok';   // 探针页正文里的标记
var LOGIN_REDIRECT_MARK = 'window.top.location.href';  // 会话失效时 httpd 的跳转页
var _pingPath = 0;
var PROBE_MAX_FAILS = 6;             // 探针连续无响应几次才判定会话/服务不可用（约 15s）
var PROBE_RETRY_MS = 3000;           // httpd 被 watchdog 重启约需 20-30 秒，退避重试
var _probeBusy = false;
var _probeFails = 0;
var _probeTimer = null;
var _probeWaiters = [];

// 注意：这里只能列「页面上真实存在的 input/select/textarea 的 id」。
// phantom_cron_time 由 cron_hour + cron_minute 两个下拉合成，没有对应元素，
// 不能放进这个数组——否则 conf2obj() 会访问 null 并中断整个初始化。
var params_inp = ['phantom_uri', 'phantom_mode', 'phantom_protocol', 'phantom_lan_if',
                  'phantom_tun_name', 'phantom_tun_addr', 'phantom_table', 'phantom_whitelist',
                  'phantom_log_level', 'phantom_gateway_mode',
                  'phantom_server_up_mbps', 'phantom_server_down_mbps'];
var params_chk = ['phantom_enable', 'phantom_dns_hijack', 'phantom_builtin_wl',
                  'phantom_cron_enable', 'phantom_watchdog', 'phantom_block_doh'];

// 运行期文件的候选路径。
//
// 首选必须走 httpdb 的 /_temp/ 路由（物理目录 /tmp/upload）：
//   本固件 httpd **不服务 docroot 下的 .txt** —— /phantom_status.txt 与
//   固件自带的 /Lang_Hdr.txt 一样返回 404，与权限无关。koolshare 的通用做法是
//   把文本文件写到 /tmp/upload，再用 /_temp/<名字> 读（软件中心自己的
//   /_temp/soft_log.txt、fancyss 的 /_temp/ss_log.txt 都是这么做的）。
// 第二条是历史软链路径，只在别的固件上可能命中，纯粹兜底。
var statusPath = 0;
var STATUS_PATHS = ['/_temp/phantom_status.txt', '/phantom_status.txt'];
var logPath = 0;
var LOG_PATHS = ['/_temp/phantom_log.txt', '/phantom_log.txt'];

function gid(id) { return document.getElementById(id); }

function init() {
	// 每一步都独立兜底：任何一处出错都不能让开关和配置区一起消失
	try { show_menu(menu_hook); } catch (e) { console.log("show_menu: " + e); }
	try { generate_options(); } catch (e) { console.log("generate_options: " + e); }
	try { hook_visibility(); } catch (e) { console.log("hook_visibility: " + e); }
	get_dbus_data();
	get_run_status();
}

function conf2obj() {
	var i, el;
	for (i = 0; i < params_inp.length; i++) {
		el = gid(params_inp[i]);
		if (el && dbus[params_inp[i]]) { el.value = dbus[params_inp[i]]; }
	}
	for (i = 0; i < params_chk.length; i++) {
		el = gid(params_chk[i]);
		if (el && dbus[params_chk[i]]) { el.checked = dbus[params_chk[i]] == "1"; }
	}
	if (gid("phantom_version")) {
		gid("phantom_version").innerHTML = "当前版本：" + (dbus["phantom_version"] || "?") +
			'　<span style="color:#C8D2D6;font-size:11px;">UI r' + PHANTOM_UI_REV + '</span>';
	}
	if (dbus["phantom_whitelist"]) { update_wl_count(); }
	validate_uri();
	validate_tun();
}

function show_error(msg) {
	var el = gid("head_illustrate");
	if (el) { el.innerHTML = '<span style="color:#FF5252;">配置读取失败：' + msg + '</span>'; }
}

// ---------------------------------------------------------------- 会话预检

// 停掉所有轮询定时器（后台标签页 / 会话失效时调用）
function stop_pollers() {
	if (_statusTimer) { clearTimeout(_statusTimer); _statusTimer = null; }
	if (_logTimer) { clearTimeout(_logTimer); _logTimer = null; }
	if (_speedTimer) { clearTimeout(_speedTimer); _speedTimer = null; }
	if (_startedTimer) { clearTimeout(_startedTimer); _startedTimer = null; }
}

// 会话失效：彻底停手，并且**不再发出任何 httpdb 请求**（发一个崩一个）
function session_lost(detail) {
	if (SESSION_LOST) { return; }
	SESSION_LOST = true;
	_probeWaiters = [];
	stop_pollers();
	if (_probeTimer) { clearTimeout(_probeTimer); _probeTimer = null; }

	var b = gid("session_banner");
	if (b) {
		b.style.display = "";
		b.innerHTML = '⚠ 路由器登录会话已失效，已停止页面刷新与提交 —— '
			+ '继续请求会把路由器 Web 服务（httpd）打崩。请先 '
			+ '<a href="/Main_Login.asp">重新登录</a>，再回到本页'
			+ (detail ? '（' + detail + '）' : '') + '。隧道本身不受影响，仍在正常转发。';
	}
	var b1 = gid("apply_button-1"); if (b1) { b1.disabled = true; }
	var b2 = gid("apply_button-2"); if (b2) { b2.disabled = true; }
	var sb = gid("speed_btn");
	if (sb) { sb.onclick = null; sb.style.opacity = "0.45"; sb.style.pointerEvents = "none"; }
	var tip = gid("busy_tip");
	if (tip) { tip.style.display = "none"; }
	var rs = gid("run_status");
	if (rs) { rs.innerHTML = '<span class="phantom-badge phantom-badge-stopped">状态未知</span>'; }
}

function flush_probe(ok) {
	var ws = _probeWaiters, i;
	_probeWaiters = [];
	for (i = 0; i < ws.length; i++) {
		try { ws[i](ok); } catch (e) { console.log("probe cb: " + e); }
	}
}

// 会话探针：走 .asp（httpd 自己的会话检查），不经过 httpdb，因此不会触发崩溃。
function session_probe(cb) {
	if (SESSION_LOST) { return; }
	_probeWaiters.push(cb);
	if (_probeBusy) { return; }   // 同一时刻只打一发，多个等待者共享结果
	_probeBusy = true;
	run_probe();
}

function run_probe() {
	_probeTimer = null;
	$.ajax({
		url: PING_PATHS[_pingPath],
		type: 'GET',
		dataType: 'text',
		cache: false,
		success: function(body) {
			_probeBusy = false;
			if (is_login_redirect(body)) {
				// 拿到的不是页面而是登录跳转页 —— 会话确实没了
				session_lost('会话探针返回登录页');
				flush_probe(false);
				return;
			}
			if (_pingPath === 0 && (!body || body.indexOf(PING_MARK) < 0)) {
				// 探针页存在但内容不对（旧包残留 / 被清理过）—— 退回管理页本体
				use_ping_fallback();
				return;
			}
			_probeFails = 0;
			flush_probe(true);
		},
		error: function(xhr, textStatus) {
			if (_pingPath === 0 && xhr && xhr.status === 404) {
				// 本固件只把 /Module_* 交给 koolshare 的 webs 处理器：探针页取不到
				// （名字不对、旧包残留、被清理）就退回管理页本体，它一定存在。
				use_ping_fallback();
				return;
			}
			_probeFails++;
			if (_probeFails < PROBE_MAX_FAILS) {
				// httpd 可能刚被 watchdog 重启（真机约 20-30 秒），退避重试
				_probeTimer = setTimeout(run_probe, PROBE_RETRY_MS);
				return;
			}
			_probeBusy = false;
			session_lost('会话探针无响应（' + (textStatus || '') + '）');
			flush_probe(false);
		}
	});
}

// httpd 在会话失效时返回的是「跳到登录页」的 HTML（而不是 401/JSON）。
// 只认这个组合，避免把管理页里「重新登录」链接误判成会话失效。
function is_login_redirect(body) {
	return !!body && body.indexOf(LOGIN_REDIRECT_MARK) >= 0
		&& body.indexOf('Main_Login.asp') >= 0;
}

// 探针页不可用时固定回退到管理页本体（49KB，只在探针页缺失时才用）
function use_ping_fallback() {
	_pingPath = 1;
	_probeFails = 0;
	run_probe();   // 立刻用回退候选重试，等待者继续排队
}

// 所有走 httpdb 通道（/_api/、/_temp/）的请求都必须从这里进：
// 会话失效时那些请求会把 httpd 打崩，而 .asp 探针不会。
function probe_then(ok_fn, fail_fn) {
	if (SESSION_LOST) { return; }
	session_probe(function(ok) {
		if (ok) { ok_fn(); } else if (fail_fn) { fail_fn(); }
	});
}

// 后台标签页不轮询：浏览器会把定时器限流到 1 分钟一次，而失效会话的请求
// 正好会让 httpd 每分钟崩一次（真机连续崩了两小时就是这个形态）。
function hook_visibility() {
	if (!document.addEventListener) { return; }
	document.addEventListener("visibilitychange", function() {
		if (document.hidden) { stop_pollers(); return; }
		if (SESSION_LOST) { return; }
		get_run_status();
		if (gid("tablet_2") && gid("tablet_2").style.display !== "none") { get_log(); }
	}, false);
}

function get_dbus_data() {
	// 先探针再读配置：会话失效时这个 /_api/ 请求会把 httpd 打崩
	probe_then(fetch_dbus_data, function() {
		show_error("路由器登录会话已失效，无法读取配置。");
	});
}

function fetch_dbus_data() {
	$.ajax({
		type: "GET",
		url: "/_api/phantom",
		dataType: "json",
		cache: false,
		async: false,
		success: function(data) {
			if (data && data.result == -403) {
				session_lost('读配置被拒绝（-403）');
				return;
			}
			dbus = (data && data.result && data.result[0]) ? data.result[0] : {};
			// 分步 try/catch：conf2obj 里任何一个字段对不上 DOM，
			// 都不能连累后面的界面初始化（否则表现为「页面没反应」）
			try { conf2obj(); } catch (e) { console.log("conf2obj: " + e); }
			try { toggle_func(); } catch (e) { console.log("toggle_func: " + e); }
			try { update_visibility(); } catch (e) { console.log("update_visibility: " + e); }
			try { hook_event(); } catch (e) { console.log("hook_event: " + e); }
		},
		error: function(XmlHttpRequest, textStatus, errorThrown) {
			console.log(XmlHttpRequest.responseText);
			// 无响应也可能是 httpd 刚崩过：先探一次会话，会话还在就按普通读取失败提示
			session_probe(function(ok) {
				if (ok) {
					show_error("GET /_api/phantom 无响应（" + textStatus + "）。插件可能没装完整，请重新安装。");
				}
			});
		}
	});
}

function hook_event() {
	$("#phantom_enable").click(function() {
		if (gid('phantom_enable').checked) {
			dbus["phantom_enable"] = "1";
		} else {
			dbus["phantom_enable"] = "0";
		}
		update_visibility();
	});
}

function validate_uri() {
	var el = gid("phantom_uri");
	var tip = gid("uri_tip");
	if (!el) { return true; }
	var v = el.value;
	if (v === "" || v.indexOf("phantom://") === 0) {
		tip.innerHTML = "";
		el.className = "input_ss_table";
		return true;
	}
	tip.innerHTML = "连接串必须以 phantom:// 开头";
	el.className = "input_ss_table phantom-textarea-invalid";
	return false;
}

function validate_tun() {
	var el = gid("phantom_tun_addr");
	var tip = gid("tun_tip");
	if (!el) { return true; }
	var ok = /^\d{1,3}(\.\d{1,3}){3}\/\d{1,2}$/.test(el.value);
	tip.innerHTML = ok ? "" : "必须是合法 CIDR（如 10.7.0.1/24），且不得与 LAN 网段重叠";
	el.className = ok ? "input_ss_table" : "input_ss_table phantom-textarea-invalid";
	return ok;
}

// 白名单按「逗号 / 空格 / 换行」分隔 —— 页面占位符写的是「一行一个域名，或用
// 逗号分隔」，所以多行输入必须当成多条规则，而不是一条非法规则。
//
// 为什么必须在页面就折成单行：**dbus 存不了多行文本**。httpdb 不处理 JSON
// 转义，`"a.com\nb.com"` 会被原样落库成字面 `a.com\nb.com`（4 个可见字符的
// `\`+`n`），于是按逗号切只有一条 → 页面标红报错，脚本按逗号切会写出
// `a.comnb.com` 这种垃圾域名。发送前折成 `a.com,b.com` 就一切正常。
function split_whitelist(txt) {
	var parts = String(txt === undefined || txt === null ? "" : txt).split(/[\s,;]+/);
	var out = [], i, d;
	for (i = 0; i < parts.length; i++) {
		d = parts[i].replace(/^\s+|\s+$/g, "");
		if (d === "" || out.indexOf(d) >= 0) { continue; }   // 去空、去重
		out.push(d);
	}
	return out;
}

function normalize_whitelist(txt) { return split_whitelist(txt).join(","); }

function update_wl_count() {
	var parts = split_whitelist(gid("phantom_whitelist").value);
	var n = 0, bad = 0, i;
	for (i = 0; i < parts.length; i++) {
		if (/^[a-z0-9]([a-z0-9.\-_*]*[a-z0-9*])?$/i.test(parts[i])) { n++; } else { bad++; }
	}
	gid("wl_tip").innerHTML = "共 " + n + " 条" + (bad > 0 ? "，其中 " + bad + " 条格式不合法（已标红）" : "")
		+ "（多行/空格会被折成一行逗号分隔）";
	gid("phantom_whitelist").className = (bad > 0) ? "phantom-textarea phantom-textarea-invalid" : "phantom-textarea";
}

function generate_options() {
	var i, h = "";
	for (i = 0; i < 24; i++) {
		h += "<option value='" + i + "'>" + (i < 10 ? "0" + i : i) + "</option>";
	}
	gid("cron_hour").innerHTML = h;
	h = "";
	for (i = 0; i < 60; i += 5) {
		h += "<option value='" + i + "'>" + (i < 10 ? "0" + i : i) + "</option>";
	}
	gid("cron_minute").innerHTML = h;
	gid("cron_hour").value = "4";
	gid("cron_minute").value = "30";
}

function sync_cron_inputs() {
	var t = dbus["phantom_cron_time"] || "4:30";
	var p = t.split(":");
	gid("cron_hour").value = p[0] || "4";
	gid("cron_minute").value = p[1] || "30";
	if (dbus["phantom_cron_time"]) { return; }
	dbus["phantom_cron_time"] = t;
}

// ---------------------------------------------------------------- 状态

function fmt_rate(bps) {
	if (bps >= 1048576) { return (bps / 1048576).toFixed(2) + " MB/s"; }
	if (bps >= 1024) { return (bps / 1024).toFixed(1) + " KB/s"; }
	return bps + " B/s";
}

function fmt_bytes(b) {
	if (b >= 1073741824) { return (b / 1073741824).toFixed(2) + " GB"; }
	if (b >= 1048576) { return (b / 1048576).toFixed(2) + " MB"; }
	if (b >= 1024) { return (b / 1024).toFixed(1) + " KB"; }
	return b + " B";
}

function set_metric(id, text) {
	var el = gid(id);
	if (!el || el.innerHTML === text) { return; }
	el.innerHTML = text;
	el.className = "phantom-metric-value flash";
	setTimeout(function() { el.className = "phantom-metric-value"; }, 300);
}

function schedule_status(ms) {
	if (_statusTimer) { clearTimeout(_statusTimer); }
	_statusTimer = setTimeout(get_run_status, ms);
}

function get_run_status() {
	_statusTimer = null;
	if (SESSION_LOST) { return; }
	// 未启用时隧道一定没在跑，没必要每 2 秒去请求状态文件
	var on = gid("phantom_enable") && gid("phantom_enable").checked;
	if (!on) {
		render_stopped();
		schedule_status(5000);
		return;
	}
	if (document.hidden) { schedule_status(5000); return; }
	if (statusPath >= STATUS_PATHS.length) {
		render_stopped();
		schedule_status(5000);
		return;
	}
	// **必须先过会话探针**：会话失效时直接打 /_temp/ 会把 httpd 打崩
	probe_then(function() {
		$.ajax({
			url: status_url(),
			type: 'GET',
			dataType: 'text',
			async: true,
			cache: false,
			success: function(response) {
				var st = null;
				try { st = JSON.parse(response); } catch (e) { st = null; }
				if (!st) { render_stopped(); return; }
				// 「上次动作」随状态文件一起带回，不再单独轮询 /_api/phantom_last_act
				if (st.last_act !== undefined && gid("last_act")) {
					gid("last_act").innerHTML = st.last_act;
				}
				if (st.running === 1) {
					gid("run_status").innerHTML = '<span class="phantom-badge phantom-badge-running">运行中</span>';
					set_metric("m_down", fmt_rate(st.down_rate));
					set_metric("m_up", fmt_rate(st.up_rate));
					set_metric("m_total_down", fmt_bytes(st.total_down));
					set_metric("m_total_up", fmt_bytes(st.total_up));
					set_metric("m_conns", String(st.conns));
					set_metric("m_direct", String(st.direct));
					set_metric("m_proxy", String(st.proxy));
					set_metric("m_cpu", st.cpu + " %");
					set_metric("m_gw", gw_label(st.gw));
					set_metric("m_ipset", String(st.ipset || 0));
					set_metric("m_fd", (st.fd || 0) + " / " + (st.fdl || 0));
				} else {
					render_stopped();
				}
			},
			error: function() {
				// 这条路径读不到就换下一条（不同固件的 docroot 不一样）
				statusPath++;
				render_stopped();
			},
			complete: function() {
				if (!SESSION_LOST) { schedule_status(2000); }
			}
		});
	});
}

function render_stopped() {
	gid("run_status").innerHTML = '<span class="phantom-badge phantom-badge-stopped">未运行</span>';
	set_metric("m_down", "0 B/s");
	set_metric("m_up", "0 B/s");
	set_metric("m_conns", "0");
	set_metric("m_direct", "0");
	set_metric("m_proxy", "0");
	set_metric("m_cpu", "0 %");
	set_metric("m_gw", "-");
	set_metric("m_ipset", "0");
	set_metric("m_fd", "-");
}

// 生效的分流模式。内核分流 = 只有白名单目标进 TUN，直连走内核快路径；
// 兼容模式 = 全部进 TUN 由用户态判定（老固件没有 ipset 时会自动回退到这里）。
function gw_label(mode) {
	if (mode === "kernel-split") { return "内核分流"; }
	if (mode === "relay") { return "兼容模式"; }
	return "-";
}

// 「上次动作」以前每 5 秒单独轮询 /_api/phantom_last_act —— 那是每轮多一次
// 走 httpdb 的请求，会话失效时就多一次把 httpd 打崩的机会。现在由状态文件
// 带回（phantom_status.sh 把 dbus 的 last_act 写进 JSON），见 get_run_status()。
// 服务端的 /_api/phantom_last_act 与 dbus 键都保留，SSH / 其它工具照旧可用。

// ---------------------------------------------------------------- 提交

function collect_fields() {
	var dbus_new = {}, i, el;
	for (i = 0; i < params_inp.length; i++) {
		el = gid(params_inp[i]);
		if (el) { dbus_new[params_inp[i]] = el.value; }
	}
	for (i = 0; i < params_chk.length; i++) {
		el = gid(params_chk[i]);
		if (el) { dbus_new[params_chk[i]] = el.checked ? '1' : '0'; }
	}
	// 白名单折成单行逗号分隔：dbus 存不了多行（httpdb 会把 JSON 的 \n 原样落库，
	// 结果既是页面标红「格式不合法」，又是脚本写出 github.comnjetbrains.com 这类垃圾域名）
	if (dbus_new["phantom_whitelist"] !== undefined) {
		dbus_new["phantom_whitelist"] = normalize_whitelist(dbus_new["phantom_whitelist"]);
	}
	dbus_new["phantom_cron_time"] = gid("cron_hour").value + ":" + gid("cron_minute").value;
	return dbus_new;
}

// 提交期间给出可见进度，并禁用按钮，避免用户重复点击
function set_busy(on, msg) {
	// 会话失效后按钮永久禁用：此时的任何 httpdb 请求都会把 httpd 打崩
	if (SESSION_LOST) { on = false; }
	var tip = gid("busy_tip");
	if (tip) {
		tip.style.display = on ? "" : "none";
		if (msg) { tip.innerHTML = msg; }
	}
	var off = on || SESSION_LOST;
	var b1 = gid("apply_button-1");
	if (b1) { b1.disabled = off; }
	var b2 = gid("apply_button-2");
	if (b2) { b2.disabled = off; }
}

// 必须用异步 XHR。
// 软件中心是**同步执行**后台脚本的：phantom_config.sh 1 会走
// stop（最多等 5 秒让进程回滚路由）+ sleep 3（等隧道起来）。
// 同步 XHR 会把浏览器主线程一起冻住 —— 表现就是「点提交后页面卡死没反应」。
//
// params 一律传字符串：httpdb 的调用约定是
//   POST {"id":N,"method":"phantom_config.sh","params":["1"],"fields":{…}}
// → 先把 fields 落 dbus → 再执行 `phantom_config.sh <id> <params...>`。
// 因此 **$1 是请求 id，action 在 $2**，脚本必须回包 /_resp/<id>，否则前端
// 一直转圈、最后弹「后台执行失败」。其它插件（ks_app_install.sh、
// clash_downyamlsel.sh）都是这个形状，这里保持一致。
//
// 这段代码同时负责「httpd 崩溃」的自救：真机上 httpd 会因为失效会话的
// httpdb 请求而 SIGSEGV（watchdog 约 20-30 秒后把它拉起来），提交正好撞上
// 时浏览器只会看到「提交失败（请求未完成）」，而 fields 一个都没进 dbus。
// 所以：提交前先探针；撞上崩溃时不要拿旧会话盲目重试（那只会再崩一次），
// 而是等它恢复、确认会话仍在、换新请求 id 再试；最后仍无响应时用 dbus 里
// 的实际值核对「到底生效没有」，不把已生效的提交误报成失败。
var POST_MAX_ATTEMPTS = 2;      // 首发 + 1 次重试（每次都用新的请求 id）
var POST_RETRY_DELAY_MS = 5000; // 等 watchdog 把 httpd 拉起来

function post_action(flag, after, busyMsg, attempt) {
	if (SESSION_LOST) { return; }
	if (attempt === undefined) { attempt = 1; }
	var id = parseInt(Math.random() * 100000000);
	var postData = {"id": id, "method": "phantom_config.sh", "params": [String(flag)], "fields": collect_fields()};
	if (attempt === 1) {
		set_busy(true, busyMsg || "正在应用配置，请稍候…（启动隧道需要几秒）");
	}
	probe_then(function() {
		$.ajax({
			url: "/_api/",
			cache: false,
			async: true,
			type: "POST",
			dataType: "json",
			data: JSON.stringify(postData),
			success: function(response) {
				set_busy(false);
				if (response && response.result == id) {
					if (after) { after(); }
					return;
				}
				if (response && response.result == -403) {
					session_lost('提交被拒绝（-403）');
					return;
				}
				alert("后台执行失败（result=" + ((response && response.result !== undefined) ? response.result : "空") + "），请看日志页");
				get_log();
			},
			error: function(xhr, textStatus) {
				if (attempt < POST_MAX_ATTEMPTS) {
					set_busy(true, "路由器 Web 服务中断（httpd 崩溃后由 watchdog 重启），正在等它恢复并重试…");
					setTimeout(function() {
						// 探针确认会话仍然有效才重试；会话已失效就只提示重新登录
						probe_then(function() {
							post_action(flag, after, busyMsg, attempt + 1);
						}, function() { set_busy(false); });
					}, POST_RETRY_DELAY_MS);
					return;
				}
				set_busy(true, "没有收到回应，正在核对配置是否已经生效…");
				verify_applied(postData.fields, function(applied) {
					set_busy(false);
					if (applied) {
						alert("提交已生效（响应在路由器 Web 服务重启时丢失，但配置已经写入）。");
						if (after) { after(); }
						return;
					}
					alert("提交失败（请求未完成）。\nHTTP " + ((xhr && xhr.status) ? xhr.status : "无响应")
						+ "（" + (textStatus || "") + "）\n"
						+ "若页面顶部提示「登录会话已失效」，请重新登录后再提交；\n"
						+ "否则请用 SSH 排查：\n/bin/sh /koolshare/scripts/phantom_config.sh 1");
				});
			}
		});
	});
}

// 提交（或重试）都没收到回应时，用 dbus 里的实际值判断到底生效没有：
// 请求可能已经送达、只是响应在 httpd 崩溃时丢了 —— 那种情况不能让用户
// 以为是失败，否则他会重复提交。
function verify_applied(fields, cb) {
	probe_then(function() {
		$.ajax({
			url: "/_api/phantom",
			type: "GET",
			dataType: "json",
			cache: false,
			success: function(data) {
				var cur = (data && data.result && data.result[0]) ? data.result[0] : {};
				var keys = ["phantom_whitelist", "phantom_enable", "phantom_uri",
				            "phantom_mode", "phantom_cron_time"];
				var match = true, i, k, a;
				for (i = 0; i < keys.length; i++) {
					k = keys[i];
					if (fields[k] === undefined) { continue; }
					a = (cur[k] === undefined || cur[k] === null) ? "" : String(cur[k]);
					if (a !== String(fields[k])) { match = false; }
				}
				cb(match);
			},
			error: function() { cb(false); }
		});
	}, function() { cb(false); });
}

// 当前可用的状态文件路径（两条候选都试完则为 null）
function status_url() {
	if (statusPath >= STATUS_PATHS.length) { return null; }
	return STATUS_PATHS[statusPath];
}

// 提交后轮询「隧道是否起来了」。
// POST 现在毫秒级返回（启动在后台跑），所以由前端负责把结果呈现出来：
// 起来了就提示成功，超时就提示去看日志，而不是让页面一直转圈。
var _waitLeft = 0;
function wait_started(maxSec) {
	if (_startedTimer) { clearTimeout(_startedTimer); _startedTimer = null; }
	_waitLeft = maxSec;
	poll_started();
}

function poll_started() {
	_startedTimer = null;
	if (SESSION_LOST) { return; }
	var url = status_url();
	if (!gid("phantom_enable").checked) { set_busy(false); return; }
	if (url === null) {
		set_busy(true, "读不到状态文件，请切到「查看日志」确认。");
		return;
	}
	if (_waitLeft-- <= 0) {
		set_busy(true, "启动较慢或失败，请切到「查看日志」确认。");
		return;
	}
	// 状态文件同样走 httpdb 通道，必须先过会话探针
	probe_then(function() {
		$.ajax({
			url: url,
			type: 'GET',
			dataType: 'text',
			cache: false,
			success: function(response) {
				var st = null;
				try { st = JSON.parse(response); } catch (e) { st = null; }
				if (st && st.running === 1) {
					set_busy(true, "✔ 隧道已启动");
					set_metric("m_down", fmt_rate(st.down_rate));
					set_metric("m_up", fmt_rate(st.up_rate));
					setTimeout(function() { set_busy(false); }, 1500);
					return;
				}
				_startedTimer = setTimeout(poll_started, 1000);
			},
			error: function() {
				if (!SESSION_LOST) { _startedTimer = setTimeout(poll_started, 1000); }
			}
		});
	});
}

// 测速结果同样轮询：后台下载完会写 phantom_speed_last
var _speedLeft = 0;
function poll_speed_result(maxSec) {
	_speedLeft = maxSec;
	poll_speed();
}

function poll_speed() {
	_speedTimer = null;
	if (SESSION_LOST) { return; }
	if (_speedLeft-- <= 0) {
		gid("speed_result").innerHTML = "测速完成（结果见日志）";
		return;
	}
	probe_then(function() {
		$.ajax({
			type: "GET",
			url: "/_api/phantom_speed_last",
			dataType: "json",
			cache: false,
			success: function(data) {
				if (data && data.result == -403) {
					session_lost('测速通道被拒绝（-403）');
					return;
				}
				var s = (data && data.result) ? data.result[0] : null;
				var v = (s && s["phantom_speed_last"]) ? s["phantom_speed_last"] : "";
				if (v && v.indexOf("测速中") !== 0) {
					gid("speed_result").innerHTML = v;
					return;
				}
				_speedTimer = setTimeout(poll_speed, 2000);
			},
			error: function() { _speedTimer = setTimeout(poll_speed, 2000); }
		});
	});
}

function save(flag) {
	if (SESSION_LOST) { return; }
	if (!validate_uri() || !validate_tun()) {
		alert("配置有误，请先修正标红的输入项");
		return;
	}
	// 折成单行再提交：所见即所存（dbus 里就是这一行逗号分隔，避免 httpdb 把
	// 换行原样落库后又被当成一条非法规则）
	gid("phantom_whitelist").value = normalize_whitelist(gid("phantom_whitelist").value);
	update_wl_count();
	dbus["phantom_enable"] = gid("phantom_enable").checked ? "1" : "0";
	post_action(flag, function() {
		get_log();
		if (gid("phantom_enable").checked) {
			wait_started(20);
		} else {
			setTimeout("refreshpage();", 1500);
		}
	});
}

function speedtest() {
	if (SESSION_LOST) { return; }
	gid("speed_result").innerHTML = "测速中…";
	post_action(3, function() {
		get_log();
		poll_speed_result(20);
	}, "测速中…（下载 5MB 样本，约 10 秒，期间页面可正常操作）");
}

function clear_log() {
	if (SESSION_LOST) { return; }
	post_action(2, function() { get_log(); }, "正在清空日志…");
}

// ---------------------------------------------------------------- 日志

function get_log() {
	// 单定时器：切到日志 tab 与提交成功回调都会调 get_log()，
	// 不收敛就会各起一条 1.5s 轮询，白白多一倍请求。
	if (_logTimer) { clearTimeout(_logTimer); _logTimer = null; }
	if (SESSION_LOST) { return; }
	var retArea = gid("log_content_text");
	if (logPath >= LOG_PATHS.length) {
		retArea.value = "读取不到日志文件（已试过：" + LOG_PATHS.join("、") + "）。\n"
			+ "请用 SSH 查看：/bin/sh /koolshare/scripts/phantom_config.sh diag";
		return;
	}
	if (document.hidden) { return; }
	// 日志同样走 httpdb 通道（/_temp/），会话失效时必须先停手
	probe_then(function() {
		$.ajax({
			url: LOG_PATHS[logPath],
			type: 'GET',
			dataType: 'html',
			async: true,
			cache: false,
			success: function(response) {
				if (_responseLen == response.length) { noChange++; } else { noChange = 0; }
				if (noChange > 200) { return false; }
				retArea.value = response;
				retArea.scrollTop = retArea.scrollHeight;
				_responseLen = response.length;
				_logTimer = setTimeout(get_log, 1500);
			},
			error: function() {
				// 换下一条候选路径；全试完时 get_log() 会显示 SSH 提示并停止轮询
				logPath++;
				get_log();
			}
		});
	});
}

// ---------------------------------------------------------------- 界面切换

function toggle_func() {
	$('.show-btn1').addClass('active');
	$(".show-btn1").click(function() { switch_tab(1); });
	$(".show-btn2").click(function() { switch_tab(2); get_log(); });
	$(".show-btn3").click(function() { switch_tab(3); });
}

function switch_tab(n) {
	var i;
	for (i = 1; i <= 3; i++) {
		$('.show-btn' + i).removeClass('active');
		gid("tablet_" + i).style.display = (i === n) ? "" : "none";
	}
	$('.show-btn' + n).addClass('active');
	gid("apply_button-1").style.display = (n === 1) ? "" : "none";
	gid("apply_button-2").style.display = (n === 2) ? "" : "none";
}

function update_visibility() {
	var on = gid("phantom_enable").checked;
	gid("tablet_show").style.display = on ? "" : "none";
	gid("tablet_1").style.display = on ? "" : "none";
	gid("last_act_tr").style.display = on ? "" : "none";
	if (gid("off_hint")) { gid("off_hint").style.display = on ? "none" : ""; }
	if (on) { sync_cron_inputs(); }
}

function menu_hook() {
	tabtitle[tabtitle.length - 1] = new Array("", "phantom");
	tablink[tablink.length - 1] = new Array("", "Module_phantom.asp");
}

function reload_Soft_Center() {
	location.href = "/Module_Softcenter.asp";
}
</script>
</head>
<body onload="init();">
<div id="TopBanner"></div>
<div id="Loading" class="popup_bg"></div>
<table class="content" align="center" cellpadding="0" cellspacing="0">
	<tr>
		<td width="17">&nbsp;</td>
		<td valign="top" width="202">
			<div id="mainMenu"></div>
			<div id="subMenu"></div>
		</td>
		<td valign="top">
			<div id="tabMenu" class="submenuBlock"></div>
			<table width="98%" border="0" align="left" cellpadding="0" cellspacing="0" style="display: block;">
				<tr>
					<td align="left" valign="top">
						<div>
							<table width="760px" border="0" cellpadding="5" cellspacing="0" bordercolor="#6b8fa3" class="FormTitle" id="FormTitle">
								<tr>
									<td bgcolor="#4D595D" colspan="3" valign="top">
										<div>&nbsp;</div>
										<div style="float:left;" class="formfonttitle" style="padding-top: 12px">Phantom 透明网关</div>
										<div style="float:right; width:15px; height:25px;margin-top:10px"><img id="return_btn" onclick="reload_Soft_Center();" align="right" style="cursor:pointer;position:absolute;margin-left:-30px;margin-top:-25px;" title="返回软件中心" src="/images/backprev.png" onMouseOver="this.src='/images/backprevclick.png'" onMouseOut="this.src='/images/backprev.png'" onerror="this.style.display='none'"></img></div>
										<div style="margin:30px 0 10px 5px;" class="splitLine"></div>
										<div style="margin-left:5px;" id="head_illustrate">
											<li><em>Phantom</em> 把路由器变成透明网关：LAN 内设备无需任何配置，命中白名单的流量自动经加密隧道出网。</li>
										</div>
										<!-- 会话失效横幅：ASUS 默认 30 分钟无操作自动登出，而失效会话的
										     httpdb 请求会把本固件的 httpd 打崩，所以一旦探针发现会话没了
										     就停掉全部轮询并在这里提示重新登录 -->
										<div id="session_banner" class="phantom-warn" style="display:none;margin:8px 0 0 5px;"></div>
										<div id="phantom_switch" style="margin:5px 0px 0px 0px;">
											<table width="100%" border="1" align="center" cellpadding="4" cellspacing="0" bordercolor="#6b8fa3" class="FormTable">
												<thead>
												<tr>
													<td colspan="2">Phantom - 开关 / 状态</td>
												</tr>
												</thead>
												<tr id="switch_tr">
													<th>
														<label>开启 Phantom</label>
													</th>
													<td colspan="2">
														<div class="switch_field" style="display:table-cell">
															<label for="phantom_enable">
																<input id="phantom_enable" class="switch" type="checkbox" style="display: none;">
																<div class="switch_container" >
																	<div class="switch_bar"></div>
																	<div class="switch_circle transition_style">
																		<div></div>
																	</div>
																</div>
															</label>
														</div>
														<div style="display:table-cell;float: left;margin-left:270px;margin-top:-32px;position: absolute;padding: 5.5px 0px;">
															<span id="run_status"><span class="phantom-badge phantom-badge-stopped">未运行</span></span>
														</div>
														<div id="phantom_version" style="padding-top:5px;margin-right:50px;margin-top:-30px;float: right;"></div>
													</td>
												</tr>
												<tr id="last_act_tr" style="display: none;">
													<th>上次动作</th>
													<td><span id="last_act"></span></td>
												</tr>
											</table>
											<div id="off_hint" class="phantom-warn" style="margin:8px 0 0 8px;">
												操作顺序：打开上面的开关 → 填写连接串 → 点底部【提交】。提交后隧道才会启动。
											</div>
										</div>
										<div id="tablet_show" style="display: none;">
											<table style="margin:10px 0px 0px 0px;border-collapse:collapse" width="100%" height="37px">
												<tr>
													<td cellpadding="0" cellspacing="0" style="padding:0" border="1" bordercolor="#222">
														<input id="show_btn1" class="show-btn1" style="cursor:pointer" type="button" value="服务配置"/>
														<input id="show_btn2" class="show-btn2" style="cursor:pointer" type="button" value="查看日志"/>
														<input id="show_btn3" class="show-btn3" style="cursor:pointer" type="button" value="帮助信息"/>
													</td>
												</tr>
											</table>
										</div>

										<!-- ==================== tab1: 服务配置 ==================== -->
										<div id="tablet_1" style="display: none;">
											<!-- 状态总览 -->
											<table width="100%" border="0" align="center" cellpadding="4" cellspacing="0" class="phantom-card">
												<tr><td colspan="4" class="phantom-title">实时状态</td></tr>
												<tr>
													<td colspan="4">
														<table class="phantom-metrics">
															<tr>
																<td class="phantom-metric">
																	<div class="phantom-metric-label">↓ 下行速率</div>
																	<div class="phantom-metric-value" id="m_down">0 B/s</div>
																</td>
																<td class="phantom-metric">
																	<div class="phantom-metric-label">↑ 上行速率</div>
																	<div class="phantom-metric-value" id="m_up">0 B/s</div>
																</td>
																<td class="phantom-metric">
																	<div class="phantom-metric-label">累计下行</div>
																	<div class="phantom-metric-value" id="m_total_down">0 B</div>
																</td>
																<td class="phantom-metric">
																	<div class="phantom-metric-label">累计上行</div>
																	<div class="phantom-metric-value" id="m_total_up">0 B</div>
																</td>
															</tr>
															<tr>
																<td class="phantom-metric">
																	<div class="phantom-metric-label">连接数</div>
																	<div class="phantom-metric-value" id="m_conns">0</div>
																</td>
																<td class="phantom-metric">
																	<div class="phantom-metric-label">直连</div>
																	<div class="phantom-metric-value" id="m_direct">0</div>
																</td>
																<td class="phantom-metric">
																	<div class="phantom-metric-label">走隧道</div>
																	<div class="phantom-metric-value" id="m_proxy">0</div>
																</td>
																<td class="phantom-metric">
																	<div class="phantom-metric-label">CPU</div>
																	<div class="phantom-metric-value" id="m_cpu">0 %</div>
																</td>
															</tr>
															<tr>
																<td class="phantom-metric">
																	<div class="phantom-metric-label">分流模式</div>
																	<div class="phantom-metric-value" id="m_gw">-</div>
																</td>
																<td class="phantom-metric">
																	<div class="phantom-metric-label">白名单条目</div>
																	<div class="phantom-metric-value" id="m_ipset">0</div>
																</td>
																<td class="phantom-metric">
																	<div class="phantom-metric-label">fd 使用</div>
																	<div class="phantom-metric-value" id="m_fd">-</div>
																</td>
																<td class="phantom-metric"></td>
															</tr>
														</table>
														<div class="phantom-hint">每 2 秒自动刷新。累计值为本次启动以来的计数，重启隧道会归零。「内核分流」= 只有白名单目标进隧道，直连走内核快路径；「兼容模式」= 全部进隧道由用户态判定（老固件没有 ipset 时自动回退）。</div>
													</td>
												</tr>
											</table>

											<!-- 服务配置 -->
											<table width="100%" border="0" align="center" cellpadding="4" cellspacing="0" class="phantom-card">
												<tr><td colspan="2" class="phantom-title">服务配置</td></tr>
												<tr>
													<th>连接串</th>
													<td>
														<input type="password" id="phantom_uri" value="" class="input_ss_table" style="width:420px;"
														       placeholder="phantom://公钥@服务器IP:端口?psk=..."
														       autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"
														       readonly onFocus="this.removeAttribute('readonly');" onblur="validate_uri();">
														<a type="button" class="ks_btn" style="width:auto;padding:5px 10px;" onclick="var e=gid('phantom_uri'); e.type = (e.type=='password')?'text':'password';">显示</a>
														<div class="phantom-hint" id="uri_tip"></div>
														<div class="phantom-warn">服务端必须是 IP:Port（phantom 不做 DNS 解析）；连接串含密钥，请妥善保存。</div>
													</td>
												</tr>
												<tr>
													<th>代理模式</th>
													<td>
														<select id="phantom_mode" class="input_option">
															<option value="smart">smart（默认：命中白名单才走隧道）</option>
															<option value="proxy">proxy（全部走隧道）</option>
															<option value="direct">direct（全部直连）</option>
														</select>
													</td>
												</tr>
												<tr>
													<th>传输协议</th>
													<td>
														<select id="phantom_protocol" class="input_option">
															<option value="tcp">TCP</option>
															<option value="quic">QUIC</option>
														</select>
													</td>
												</tr>
												<tr>
													<th>分流模式</th>
													<td>
														<select id="phantom_gateway_mode" class="input_option">
															<option value="kernel-split">内核分流（推荐：只有白名单进隧道，直连走硬件快路径）</option>
															<option value="relay">兼容模式（全部进隧道，用户态判定，较慢）</option>
														</select>
														<div class="phantom-hint">内核分流靠 ipset + fwmark 在内核里就把直连流量摘出去，硬件 NAT 加速才用得上；少数「自己解析域名（DoH）或写死 IP」的被墙应用需要兼容模式才能兜住。</div>
													</td>
												</tr>
												<tr>
													<th>LAN 接口</th>
													<td><input type="text" id="phantom_lan_if" value="br0" class="input_ss_table" style="width:160px;">
														<span class="phantom-hint">空格分隔，访客网络加 br1</span></td>
												</tr>
												<tr>
													<th>TUN 名称</th>
													<td><input type="text" id="phantom_tun_name" value="phantom0" class="input_ss_table" style="width:160px;"></td>
												</tr>
												<tr>
													<th>TUN 地址</th>
													<td><input type="text" id="phantom_tun_addr" value="10.7.0.1/24" class="input_ss_table" style="width:160px;" onblur="validate_tun();">
														<div class="phantom-hint" id="tun_tip"></div></td>
												</tr>
												<tr>
													<th>路由表 ID</th>
													<td><input type="text" id="phantom_table" value="200" class="input_ss_table" style="width:80px;"></td>
												</tr>
												<tr>
													<th>DNS 劫持</th>
													<td><input type="checkbox" id="phantom_dns_hijack" class="input" style="vertical-align:middle;">
														<span class="phantom-hint">把 LAN 的 53 端口导入隧道，域名类分流规则依赖它；关闭后本地主机名仍可解析，但域名规则失效</span></td>
												</tr>
												<tr>
													<th>拦截加密 DNS</th>
													<td><input type="checkbox" id="phantom_block_doh" class="input" style="vertical-align:middle;">
														<span class="phantom-hint">拦掉 LAN 侧的 DoT(853) 与已知 DoH 解析器：客户端一旦绕过路由器做加密解析，被墙域名的 IP 就进不了白名单，那些站点会直连失败。代价是 <i>1.1.1.1</i> 这类解析器的网页版也打不开。</span></td>
												</tr>
												<tr>
													<th>日志级别</th>
													<td>
														<select id="phantom_log_level" class="input_option">
															<option value="warn">warn（默认：只记关键事件）</option>
															<option value="info">info（每条连接/路由一行，排障用）</option>
															<option value="debug">debug（排障，CPU 占用更高）</option>
															<option value="error">error</option>
														</select>
													</td>
												</tr>
											</table>

											<!-- 白名单 -->
											<table width="100%" border="0" align="center" cellpadding="4" cellspacing="0" class="phantom-card">
												<tr><td colspan="2" class="phantom-title">分流白名单</td></tr>
												<tr>
													<th>内置被墙域名表</th>
													<td><input type="checkbox" id="phantom_builtin_wl" class="input" style="vertical-align:middle;">
														<span class="phantom-hint">内置约 4.4k 条被墙域名（随版本更新）</span></td>
												</tr>
												<tr>
													<th>自定义域名</th>
													<td>
														<textarea id="phantom_whitelist" class="phantom-textarea" spellcheck="false"
														          placeholder="一行一个域名，或用逗号分隔，例如：&#10;example.com&#10;sub.example.org"
														          onkeyup="update_wl_count();"></textarea>
														<div class="phantom-hint" id="wl_tip"></div>
														<div class="phantom-hint">Phantom 默认直连，只有命中白名单的目标才进隧道。多行/空格会自动折成一行逗号分隔（路由器配置库只能存单行）。保存后隧道生效。</div>
													</td>
												</tr>
											</table>

											<!-- 运维 -->
											<table width="100%" border="0" align="center" cellpadding="4" cellspacing="0" class="phantom-card">
												<tr><td colspan="2" class="phantom-title">运维</td></tr>
												<tr>
													<th>定时重启</th>
													<td>
														<input type="checkbox" id="phantom_cron_enable" class="input" style="vertical-align:middle;">
														<select id="cron_hour" class="input_option"></select> 时
														<select id="cron_minute" class="input_option"></select> 分
													</td>
												</tr>
												<tr>
													<th>看门狗</th>
													<td><input type="checkbox" id="phantom_watchdog" class="input" style="vertical-align:middle;">
														<span class="phantom-hint">每 5 分钟检查一次，进程退出时自动拉起</span></td>
												</tr>
												<tr>
													<th>服务端带宽</th>
													<td>
														上行 <input type="text" id="phantom_server_up_mbps" value="3" class="input_ss_table" style="width:50px;"> Mbps
														&nbsp;&nbsp;下行 <input type="text" id="phantom_server_down_mbps" value="5" class="input_ss_table" style="width:50px;"> Mbps
														<div class="phantom-hint">只用于解读测速结果。方向是反的：客户端<b>下载</b>占的是服务端<b>上行</b>，客户端<b>上传</b>占的是服务端<b>下行</b>。</div>
													</td>
												</tr>
												<tr>
													<th>测速</th>
													<td>
														<a id="speed_btn" type="button" class="ks_btn" style="width:auto;padding:5px 14px;" onclick="speedtest();">开始测速</a>
														<span id="speed_result" class="phantom-hint" style="margin-left:10px;"></span>
														<div class="phantom-hint">经 SOCKS5 127.0.0.1:1080 下载测速（默认 5MB 样本），得到的是 phantom 用户态转发吞吐上限；不含 LAN→TUN 的 NAT 转发路径，因此不等于客户端实测网速。日志里会给出「达到服务端带宽上限的百分之多少」的结论。</div>
													</td>
												</tr>
											</table>
										</div>

										<!-- ==================== tab2: 日志 ==================== -->
										<div id="tablet_2" style="display: none;">
											<div id="phantom_log" style="margin-top:-1px;display:block;overflow:hidden;">
												<textarea cols="63" rows="20" wrap="on" readonly="readonly" id="log_content_text" class="phantom-log"
												          autocomplete="off" autocorrect="off" autocapitalize="off" spellcheck="false"></textarea>
											</div>
										</div>

										<!-- ==================== tab3: 帮助 ==================== -->
										<div id="tablet_3" style="display: none;">
											<table style="margin:-1px 0px 0px 0px;" width="100%" border="1" align="center" cellpadding="4" cellspacing="0" bordercolor="#6b8fa3" class="FormTable">
												<tr>
													<td>
														<ul>
															<li><b>连接串</b>：在服务端执行 <i>phantom server</i> 后，启动目录的 <i>server.toml</i> 顶部注释里有一行完整的 <i>phantom://</i> 链接，复制过来即可。</li>
															<li><b>服务端地址必须是 IP</b>：phantom 直接按 SocketAddr 解析，填域名会启动失败（脚本会尝试用 nslookup 兜底，但不保证成功）。</li>
															<li><b>LAN 客户端完全断网</b>：多半是 TUN 地址与 LAN 网段重叠，改一个不冲突的网段后重启。</li>
															<li><b>隧道正常但 LAN 走直连</b>：确认 LAN 接口名（多数机型是 br0），用 <i>ip link show</i> 核对。</li>
															<li><b>只有域名规则不生效</b>：打开 DNS 劫持；关闭时本地主机名仍可解析，但域名分流不工作。</li>
															<li><b>iptables 规则消失</b>：Asuswrt 在 nat-start 会重建规则表，插件已注册 nat-start 钩子自动补回。</li>
															<li><b>SSH 排障</b>：<i>/bin/sh /koolshare/scripts/phantom_config.sh diag</i> 一键输出诊断；<i>/bin/sh -x ... start</i> 看详细过程；日志 <i>/tmp/upload/phantom_log.txt</i>（重启后清空，页面经 <i>/_temp/phantom_log.txt</i> 读）。</li>
															<li><b>注意</b>：本机固件的 <i>/usr/sbin/sh</i> 是 Broadcom 调试工具（<i>memaccess</i>）而非 shell，所以命令里请写<b>绝对路径 /bin/sh</b>，否则会报 <i>Address xxx is invalid</i>。</li>
														</ul>
													</td>
												</tr>
											</table>
										</div>

										<div id="apply_button" class="apply_gen">
											<div id="busy_tip" class="phantom-warn" style="display:none;text-align:center;margin-bottom:6px;"></div>
											<input id="apply_button-1" class="button_gen" type="button" onclick="save(1)" value="提交">
											<input id="apply_button-2" class="button_gen" type="button" onclick="clear_log()" value="清空日志" style="display: none;">
										</div>
										<div class="KoolshareBottom" style="margin-top:50px;">
											论坛技术支持: <a href="https://koolshare.cn" target="_blank"> <i><u>https://koolshare.cn</u></i></a><br />
											GitHub: <a href="https://github.com/koolshare/rogsoft" target="_blank"><i><u>https://github.com/koolshare</u></i></a><br />
											Phantom 客户端形态：macOS / Android / HarmonyOS / CLI / koolshare 插件
										</div>
									</td>
								</tr>
							</table>
						</div>
					</td>
				</tr>
			</table>
		</td>
		<td width="10" align="center" valign="top"></td>
	</tr>
</table>
<div id="footer"></div>
</body>
</html>
