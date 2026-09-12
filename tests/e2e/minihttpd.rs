// E2E 测试专用 mini 静态文件服务器（std-only，rustc 单文件编译为 musl 静态二进制）
// 用途：phantom-net 内部 web 容器，验证「只有经 Phantom 隧道才能访问」。
// 编译：rustc --edition 2021 -O --target aarch64-unknown-linux-musl -o minihttpd minihttpd.rs
// 运行：minihttpd /www [port]   （默认监听 0.0.0.0:8080）
//
// 端口可选是为了本机回环测速：那边要一个「比客户端快得多」的源站，用 python
// http.server 会变成在测 python（实测只有 0.4 MB/s），把客户端上限压成噪声。
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::thread;

fn main() {
    let dir = std::env::args().nth(1).unwrap_or_else(|| "/www".to_string());
    let port: u16 = std::env::args()
        .nth(2)
        .and_then(|value| value.parse().ok())
        .unwrap_or(8080);
    let listener = TcpListener::bind(("0.0.0.0", port)).unwrap_or_else(|e| panic!("bind :{port}: {e}"));
    println!("minihttpd serving {dir} on 0.0.0.0:{port}");
    for stream in listener.incoming().flatten() {
        let d = dir.clone();
        thread::spawn(move || handle(stream, d));
    }
}

fn handle(stream: TcpStream, dir: String) {
    let mut stream = stream;
    let Ok(cloned) = stream.try_clone() else { return };
    let mut reader = BufReader::new(cloned);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return;
    }
    // 读完请求头（ Connection: close，不 keep-alive）
    let mut sink = String::new();
    loop {
        sink.clear();
        match reader.read_line(&mut sink) {
            Ok(0) => break,
            Ok(_) if sink == "\r\n" || sink == "\n" => break,
            Ok(_) => continue,
            Err(_) => return,
        }
    }
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() < 2 || parts[0] != "GET" {
        let _ = write_resp(&mut stream, 405, "Method Not Allowed", b"method not allowed\n");
        return;
    }
    let mut path = parts[1].split('?').next().unwrap_or("").trim_start_matches('/');
    if path.is_empty() {
        path = "index.html";
    }
    if path.contains("..") {
        let _ = write_resp(&mut stream, 403, "Forbidden", b"forbidden\n");
        return;
    }
    let full = Path::new(&dir).join(path);
    match std::fs::read(&full) {
        Ok(body) => {
            let _ = write_resp(&mut stream, 200, "OK", &body);
        }
        Err(_) => {
            let _ = write_resp(&mut stream, 404, "Not Found", b"not found\n");
        }
    }
}

fn write_resp(stream: &mut TcpStream, code: u16, reason: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {} {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        code,
        reason,
        body.len()
    );
    stream.write_all(head.as_bytes())?;
    stream.write_all(body)?;
    stream.flush()
}
