//! HTTP proxy ingress acceptance tests.
//!
//! One local port speaks both SOCKS5 and HTTP via first-byte sniffing.
//! Covered: CONNECT tunnels, absolute-URI GET rewriting to origin form
//! (proxy headers stripped, `Connection: close` forced), and the sniffing
//! dispatch itself.

use base64::{Engine, engine::general_purpose::STANDARD as B64};
use phantom_client::failover::FailoverManager;
use phantom_client::http_proxy::handle_inbound;
use phantom_client::quic_pool::QuicPool;
use phantom_client::stats::TrafficStats;
use phantom_core::crypto::{KeyPair, Psk};
use phantom_core::transport::TransportListener;
use phantom_core::transport::tcp::TcpListener;
use phantom_core::{
    CipherPreference, ClientConfig, ProxyAuthConfig, ServerEntry, TransportProtocol,
};
use phantom_e2e::echo::{EchoMode, start_echo_server};
use phantom_e2e::socks5::Socks5Client;
use phantom_server::handler::handle_connection;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::oneshot;

/// Start the production-form ingress: one port, sniffing dispatch.
async fn start_inbound_front(
    server_addr: SocketAddr,
    server_key: &KeyPair,
    psk: &Psk,
    client_secret: [u8; 32],
) -> SocketAddr {
    start_inbound_front_with_auth(server_addr, server_key, psk, client_secret, None).await
}

async fn start_inbound_front_with_auth(
    server_addr: SocketAddr,
    server_key: &KeyPair,
    psk: &Psk,
    client_secret: [u8; 32],
    proxy_auth: Option<ProxyAuthConfig>,
) -> SocketAddr {
    let config = Arc::new(ClientConfig {
        servers: vec![ServerEntry {
            name: "http-test".to_string(),
            address: server_addr.to_string(),
            public_key: server_key.public_key_base64(),
            psk: psk.to_base64(),
            cipher: CipherPreference::Auto,
            protocol: TransportProtocol::Tcp,
        }],
        client: phantom_core::ClientSettings {
            proxy_auth,
            ..Default::default()
        },
        ..Default::default()
    });
    let failover = Arc::new(FailoverManager::new(&config).expect("failover manager"));
    let pool = Arc::new(QuicPool::new());
    let stats = TrafficStats::new();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind inbound listener");
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            let (config, failover, pool, stats) =
                (config.clone(), failover.clone(), pool.clone(), stats.clone());
            tokio::spawn(async move {
                let _ =
                    handle_inbound(stream, &config, &failover, &pool, client_secret, &stats).await;
            });
        }
    });
    addr
}

/// Start the Phantom server (TCP transport).
async fn start_tcp_server() -> (SocketAddr, KeyPair, Psk) {
    let server_key = KeyPair::generate().expect("server key");
    let psk = Psk::generate();
    let listener = TcpListener::bind(&"127.0.0.1:0".parse().unwrap())
        .await
        .expect("bind phantom server");
    let addr = listener.local_addr().unwrap();
    let secret = server_key.secret;
    let psk_clone = psk.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let psk = psk_clone.clone();
                    tokio::spawn(async move {
                        handle_connection(stream, secret, psk, &[], CipherPreference::Auto, None)
                            .await;
                    });
                }
                Err(_) => break,
            }
        }
    });
    (addr, server_key, psk)
}

/// A bare-TCP HTTP server that captures the first request head verbatim
/// (for rewrite assertions) and replies with a fixed 200 body.
struct RawHttpServer {
    addr: SocketAddr,
    head_rx: oneshot::Receiver<String>,
}

async fn start_raw_http_server(body: &'static str) -> RawHttpServer {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind raw http");
    let addr = listener.local_addr().unwrap();
    let (tx, rx) = oneshot::channel();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.expect("accept");
        let mut buf = vec![0u8; 65536];
        let mut head = Vec::new();
        loop {
            let n = s.read(&mut buf).await.expect("read");
            if n == 0 {
                return;
            }
            head.extend_from_slice(&buf[..n]);
            if head.windows(4).any(|w| w == b"\r\n\r\n") {
                break;
            }
        }
        let _ = tx.send(String::from_utf8_lossy(&head).to_string());
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        let _ = s.write_all(resp.as_bytes()).await;
    });
    RawHttpServer { addr, head_rx: rx }
}

/// Read a full HTTP response until EOF (Connection: close semantics).
async fn read_to_eof(stream: &mut TcpStream) -> String {
    let mut out = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf).await {
            Ok(0) | Err(_) => break,
            Ok(n) => out.extend_from_slice(&buf[..n]),
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_connect_tunnel_echo() {
    let _ = tracing_subscriber::fmt::try_init();

    let echo = start_echo_server(EchoMode::Echo).await;
    let (server_addr, server_key, psk) = start_tcp_server().await;
    let client_key = KeyPair::generate().expect("client key");
    let inbound = start_inbound_front(server_addr, &server_key, &psk, client_key.secret).await;

    let mut stream = TcpStream::connect(inbound).await.expect("connect inbound");
    let connect_req = format!("CONNECT {} HTTP/1.1\r\nHost: {}\r\n\r\n", echo.addr, echo.addr);
    stream
        .write_all(connect_req.as_bytes())
        .await
        .expect("send CONNECT");

    // The 200 reply head ends at CRLFCRLF; read incrementally.
    let mut reply = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = stream.read(&mut buf).await.expect("read CONNECT reply");
        reply.extend_from_slice(&buf[..n]);
        if reply.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    let reply_text = String::from_utf8_lossy(&reply);
    assert!(
        reply_text.starts_with("HTTP/1.1 200"),
        "CONNECT must be answered with 200, got: {}",
        reply_text
    );

    // The tunnel is now a raw byte pipe to the echo server.
    let payload = b"CONNECT tunnel payload";
    stream.write_all(payload).await.expect("write payload");
    let mut echoed = vec![0u8; payload.len()];
    stream.read_exact(&mut echoed).await.expect("read echo");
    assert_eq!(&echoed, payload);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_absolute_get_rewritten_to_origin_form() {
    let _ = tracing_subscriber::fmt::try_init();

    let origin = start_raw_http_server("rewritten-ok").await;
    let (server_addr, server_key, psk) = start_tcp_server().await;
    let client_key = KeyPair::generate().expect("client key");
    let inbound = start_inbound_front(server_addr, &server_key, &psk, client_key.secret).await;

    let mut stream = TcpStream::connect(inbound).await.expect("connect inbound");
    let request = format!(
        "GET http://{}/s?wd=phantom HTTP/1.1\r\nHost: {}\r\nProxy-Connection: keep-alive\r\nConnection: keep-alive\r\n\r\n",
        origin.addr, origin.addr
    );
    stream.write_all(request.as_bytes()).await.expect("send GET");

    let response = tokio::time::timeout(Duration::from_secs(3), read_to_eof(&mut stream))
        .await
        .expect("response timed out");
    assert!(
        response.starts_with("HTTP/1.1 200"),
        "expected 200, got: {}",
        response
    );
    assert!(response.ends_with("rewritten-ok"), "body mismatch: {}", response);

    // The origin must have seen the rewritten origin-form request.
    let head = origin.head_rx.await.expect("origin head captured");
    let request_line = head.lines().next().unwrap_or("");
    assert_eq!(
        request_line, "GET /s?wd=phantom HTTP/1.1",
        "request line must be rewritten to origin form"
    );
    assert!(
        !head.to_ascii_lowercase().contains("proxy-connection:"),
        "Proxy-Connection must be stripped, got:\n{}",
        head
    );
    assert!(
        head.to_ascii_lowercase().contains("connection: close"),
        "Connection must be forced to close, got:\n{}",
        head
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sniffing_dispatch_socks5_and_http_on_one_port() {
    let _ = tracing_subscriber::fmt::try_init();

    let echo = start_echo_server(EchoMode::Echo).await;
    let (server_addr, server_key, psk) = start_tcp_server().await;
    let client_key = KeyPair::generate().expect("client key");
    let inbound = start_inbound_front(server_addr, &server_key, &psk, client_key.secret).await;

    let target_ip = match echo.addr.ip() {
        std::net::IpAddr::V4(v4) => v4.octets(),
        _ => panic!("echo server must be IPv4"),
    };

    // Connection A: SOCKS5 (first byte 0x05).
    let mut socks = Socks5Client::connect_ipv4(inbound, target_ip, echo.addr.port())
        .await
        .expect("socks5 connect through sniffed port");
    let payload_a = b"socks5 path";
    socks.write_all(payload_a).await.expect("socks write");
    let mut echoed_a = vec![0u8; payload_a.len()];
    socks.read_exact(&mut echoed_a).await.expect("socks echo");
    assert_eq!(&echoed_a, payload_a, "SOCKS5 path broken");

    // Connection B: HTTP CONNECT on the same port.
    let mut http = TcpStream::connect(inbound).await.expect("connect inbound");
    let connect_req = format!("CONNECT {} HTTP/1.1\r\n\r\n", echo.addr);
    http.write_all(connect_req.as_bytes()).await.expect("send CONNECT");
    let mut reply = Vec::new();
    let mut buf = [0u8; 1024];
    loop {
        let n = http.read(&mut buf).await.expect("read reply");
        reply.extend_from_slice(&buf[..n]);
        if reply.windows(4).any(|w| w == b"\r\n\r\n") {
            break;
        }
    }
    assert!(String::from_utf8_lossy(&reply).starts_with("HTTP/1.1 200"));
    let payload_b = b"http path";
    http.write_all(payload_b).await.expect("http write");
    let mut echoed_b = vec![0u8; payload_b.len()];
    http.read_exact(&mut echoed_b).await.expect("http echo");
    assert_eq!(&echoed_b, payload_b, "HTTP path broken");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_relative_uri_gets_400() {
    let _ = tracing_subscriber::fmt::try_init();

    let (server_addr, server_key, psk) = start_tcp_server().await;
    let client_key = KeyPair::generate().expect("client key");
    let inbound = start_inbound_front(server_addr, &server_key, &psk, client_key.secret).await;

    let mut stream = TcpStream::connect(inbound).await.expect("connect inbound");
    // Origin-form requests are only valid for CONNECT-less forwarding when
    // the client knows the proxy is transparent — a proxy must reject them.
    stream
        .write_all(b"GET /relative HTTP/1.1\r\nHost: example.com\r\n\r\n")
        .await
        .expect("send relative GET");
    let response = tokio::time::timeout(Duration::from_secs(3), read_to_eof(&mut stream))
        .await
        .expect("response timed out");
    assert!(
        response.starts_with("HTTP/1.1 400"),
        "relative URI must be rejected with 400, got: {}",
        response
    );
}

// ---- proxy_auth: SOCKS5 RFC1929 + HTTP Basic (LAN sharing) ----

fn test_auth() -> ProxyAuthConfig {
    ProxyAuthConfig {
        username: "phantom".to_string(),
        password: "s3cret".to_string(),
    }
}

/// SOCKS5 greeting offering `methods`, then (when the server picks
/// userpass) the RFC1929 sub-negotiation. Returns the final method and
/// auth status bytes.
async fn socks5_handshake(
    stream: &mut TcpStream,
    methods: &[u8],
    credentials: Option<(&str, &str)>,
) -> (u8, Option<u8>) {
    let mut greeting = vec![0x05, methods.len() as u8];
    greeting.extend_from_slice(methods);
    stream.write_all(&greeting).await.expect("greeting");
    let mut reply = [0u8; 2];
    stream.read_exact(&mut reply).await.expect("method reply");
    assert_eq!(reply[0], 0x05);
    let method = reply[1];

    let mut status = None;
    if method == 0x02 {
        let (user, pass) = credentials.expect("server demanded userpass");
        let mut sub = vec![0x01, user.len() as u8];
        sub.extend_from_slice(user.as_bytes());
        sub.push(pass.len() as u8);
        sub.extend_from_slice(pass.as_bytes());
        stream.write_all(&sub).await.expect("sub-negotiation");
        let mut auth_reply = [0u8; 2];
        stream.read_exact(&mut auth_reply).await.expect("auth reply");
        assert_eq!(auth_reply[0], 0x01);
        status = Some(auth_reply[1]);
    }
    (method, status)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn socks5_rfc1929_auth_enforced() {
    let _ = tracing_subscriber::fmt::try_init();

    let echo = start_echo_server(EchoMode::Echo).await;
    let (server_addr, server_key, psk) = start_tcp_server().await;
    let client_key = KeyPair::generate().expect("client key");
    let inbound = start_inbound_front_with_auth(
        server_addr,
        &server_key,
        &psk,
        client_key.secret,
        Some(test_auth()),
    )
    .await;

    // 1. Offering only no-auth must be rejected at method selection.
    let mut s = TcpStream::connect(inbound).await.expect("connect");
    let (method, _) = socks5_handshake(&mut s, &[0x00], None).await;
    assert_eq!(method, 0xFF, "no-auth must be refused when auth is set");
    drop(s);

    // 2. Wrong password: method accepted, sub-negotiation fails.
    let mut s = TcpStream::connect(inbound).await.expect("connect");
    let (method, status) =
        socks5_handshake(&mut s, &[0x02], Some(("phantom", "wrong"))).await;
    assert_eq!(method, 0x02);
    assert_eq!(status, Some(0x01), "wrong password must fail auth");
    drop(s);

    // 3. Correct credentials: full CONNECT round-trip through the tunnel.
    let mut s = TcpStream::connect(inbound).await.expect("connect");
    let (method, status) =
        socks5_handshake(&mut s, &[0x00, 0x02], Some(("phantom", "s3cret"))).await;
    assert_eq!(method, 0x02);
    assert_eq!(status, Some(0x00), "correct credentials must pass");

    let mut req = vec![0x05, 0x01, 0x00, 0x01];
    match echo.addr.ip() {
        std::net::IpAddr::V4(v4) => req.extend_from_slice(&v4.octets()),
        _ => panic!("echo server must be IPv4"),
    }
    req.extend_from_slice(&echo.addr.port().to_be_bytes());
    s.write_all(&req).await.expect("CONNECT request");
    let mut reply = [0u8; 10];
    s.read_exact(&mut reply).await.expect("CONNECT reply");
    assert_eq!(reply[1], 0x00, "CONNECT must succeed after auth");

    let payload = b"authenticated socks5";
    s.write_all(payload).await.expect("write payload");
    let mut echoed = vec![0u8; payload.len()];
    s.read_exact(&mut echoed).await.expect("read echo");
    assert_eq!(&echoed, payload);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn http_basic_auth_enforced() {
    let _ = tracing_subscriber::fmt::try_init();

    let origin = start_raw_http_server("auth-ok").await;
    let (server_addr, server_key, psk) = start_tcp_server().await;
    let client_key = KeyPair::generate().expect("client key");
    let inbound = start_inbound_front_with_auth(
        server_addr,
        &server_key,
        &psk,
        client_key.secret,
        Some(test_auth()),
    )
    .await;

    // 1. Missing header → 407 with Proxy-Authenticate challenge.
    let mut s = TcpStream::connect(inbound).await.expect("connect");
    let req = format!("GET http://{}/ HTTP/1.1\r\nHost: {}\r\n\r\n", origin.addr, origin.addr);
    s.write_all(req.as_bytes()).await.expect("send GET");
    let resp = tokio::time::timeout(Duration::from_secs(3), read_to_eof(&mut s))
        .await
        .expect("407 timed out");
    assert!(resp.starts_with("HTTP/1.1 407"), "expected 407, got: {}", resp);
    assert!(resp.contains("Proxy-Authenticate: Basic"), "missing challenge");

    // 2. Wrong password → 407.
    let mut s = TcpStream::connect(inbound).await.expect("connect");
    let bad = B64.encode("phantom:nope");
    let req = format!(
        "GET http://{}/ HTTP/1.1\r\nHost: {}\r\nProxy-Authorization: Basic {}\r\n\r\n",
        origin.addr, origin.addr, bad
    );
    s.write_all(req.as_bytes()).await.expect("send GET");
    let resp = tokio::time::timeout(Duration::from_secs(3), read_to_eof(&mut s))
        .await
        .expect("407 timed out");
    assert!(resp.starts_with("HTTP/1.1 407"), "expected 407, got: {}", resp);

    // 3. Correct credentials → request forwarded, origin head captured.
    let mut s = TcpStream::connect(inbound).await.expect("connect");
    let good = B64.encode("phantom:s3cret");
    let req = format!(
        "GET http://{}/ HTTP/1.1\r\nHost: {}\r\nProxy-Authorization: Basic {}\r\n\r\n",
        origin.addr, origin.addr, good
    );
    s.write_all(req.as_bytes()).await.expect("send GET");
    let resp = tokio::time::timeout(Duration::from_secs(3), read_to_eof(&mut s))
        .await
        .expect("200 timed out");
    assert!(resp.starts_with("HTTP/1.1 200"), "expected 200, got: {}", resp);
    assert!(resp.ends_with("auth-ok"));

    // The credential header must never reach the origin.
    let head = origin.head_rx.await.expect("origin head captured");
    assert!(
        !head.to_ascii_lowercase().contains("proxy-authorization:"),
        "Proxy-Authorization must be stripped, got:\n{}",
        head
    );
}
