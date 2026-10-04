<!DOCTYPE html PUBLIC "-//W3C//DTD XHTML 1.0 Transitional//EN" "http://www.w3.org/TR/xhtml1/DTD/xhtml1-transitional.dtd">
<html xmlns="http://www.w3.org/1999/xhtml">
<head>
<meta http-equiv="Content-Type" content="text/html; charset=utf-8" />
<meta HTTP-EQUIV="Pragma" CONTENT="no-cache">
<meta HTTP-EQUIV="Expires" CONTENT="-1">
<title>phantom-ping</title>
</head>
<!--
	会话探针页（Module_phantom.asp 专用，不对外展示、不进菜单）。

	**为什么需要它**：本固件的 httpd 在「登录会话已失效」时收到走 httpdb 通道的
	请求（/_api/…、/_temp/…）会直接 SIGSEGV —— 真机 syslog 里 130 多次
	`Comm: httpd` 崩溃、watchdog 反复 start_httpd，用户点「提交」那一刻正好崩了，
	浏览器只看到「提交失败（请求未完成）」，而配置根本没进 dbus。

	而 .asp 是 httpd 自己处理的：会话失效时它只会返回跳转到 Main_Login.asp 的
	HTML，不会崩。所以页面在访问任何 httpdb 通道之前，先取本文件探一次会话。

	页面靠正文里的 phantom-ping-ok 判定「会话仍然有效」。

	**文件名必须以 Module_ 开头**：httpd 里硬编码了 `Module_` 前缀 + `/koolshare/webs`
	（strings 里能看到 `isWebServer` / `websApply Updateing asp` / `Module_`），
	真机实测 `/phantom_ping.asp` 直接 404，而 `/Module_xxx.css` 由它的 webs 处理器
	接管（200）。页面另有兜底：本文件取不到时改用 Module_phantom.asp 本体做探针，
	同样只看「是否返回登录跳转」，不再依赖任何未验证的通道。
-->
<body>phantom-ping-ok</body>
</html>
