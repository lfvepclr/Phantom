//! HTTP proxy ingress with same-port protocol sniffing.
//!
//! The local listener serves SOCKS5 and HTTP on a single port: the first
//! byte decides (`0x05` is the SOCKS5 version marker, anything else is
//! treated as HTTP). Two HTTP forms are supported:
//!
//! - `CONNECT host:port HTTP/1.1` — a tunnel is opened and the stream is
//!   relayed verbatim after the `200 Connection Established` reply.
//! - Absolute-URI plain requests (`GET http://host/path HTTP/1.1`) — the
//!   request line is rewritten to origin form, proxy headers are stripped,
//!   `Connection: close` is forced, and the head plus any preloaded body
//!   bytes are injected into a fresh tunnel.
//!
//! Forcing `close` keeps the relay dumb: a keep-alive client would reuse
//! the connection for a different host, which a byte-level tunnel cannot
//! re-target.

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::{Bytes, BytesMut};
use phantom_core::protocol::codec::{
    FrameReader, FrameWriter, MessageRead, MessageWrite,
};
use phantom_core::protocol::{Frame, TargetAddr};
use phantom_core::transport::tcp::TcpTransport;
use phantom_core::{ClientConfig, PhantomError, ProxyAuthConfig, Result, TransportProtocol};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::watch;

use crate::failover::FailoverManager;
use crate::quic_pool::QuicPool;
use crate::socks5;
use crate::stats::TrafficStats;
use std::sync::Arc;

/// Request heads larger than this are rejected (431).
const MAX_HTTP_HEAD: usize = 16 * 1024;

const RESP_OK: &[u8] = b"HTTP/1.1 200 Connection Established\r\n\r\n";
const RESP_BAD_REQUEST: &[u8] = b"HTTP/1.1 400 Bad Request\r\nContent-Length: 0\r\n\r\n";
const RESP_BAD_GATEWAY: &[u8] = b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\n\r\n";
const RESP_AUTH_REQUIRED: &[u8] = b"HTTP/1.1 407 Proxy Authentication Required\r\nProxy-Authenticate: Basic realm=\"phantom\"\r\nContent-Length: 0\r\n\r\n";
const RESP_HEAD_TOO_LARGE: &[u8] =
    b"HTTP/1.1 431 Request Header Fields Too Large\r\nContent-Length: 0\r\n\r\n";

/// Unified inbound dispatcher. Peeks at the first byte without consuming
/// it: `0x05` is the SOCKS5 version marker, anything else goes to HTTP.
pub async fn handle_inbound(
    stream: TcpStream,
    config: &ClientConfig,
    failover: &FailoverManager,
    quic_pool: &QuicPool,
    local_secret: [u8; 32],
    stats: &Arc<TrafficStats>,
) -> Result<()> {
    let mut byte = [0u8; 1];
    let n = stream.peek(&mut byte).await.map_err(PhantomError::Io)?;
    if n == 0 {
        return Err(PhantomError::Protocol(
            "inbound closed before any request byte".to_string(),
        ));
    }
    if byte[0] == 0x05 {
        socks5::handle_socks5_connection(
            stream,
            config,
            failover,
            quic_pool,
            local_secret,
            stats,
        )
        .await
    } else {
        handle_http_connection(stream, config, failover, quic_pool, local_secret, stats).await
    }
}

struct ParsedRequest {
    target: TargetAddr,
    /// Head to inject into the tunnel (origin form for absolute URIs).
    /// `None` for CONNECT, whose head is answered locally.
    forward_head: Option<Vec<u8>>,
    is_connect: bool,
}

async fn handle_http_connection(
    mut stream: TcpStream,
    config: &ClientConfig,
    failover: &FailoverManager,
    quic_pool: &QuicPool,
    local_secret: [u8; 32],
    stats: &Arc<TrafficStats>,
) -> Result<()> {
    // 1. Read the request head, up to the CRLFCRLF terminator. Bytes past
    //    the terminator (a request body, a TLS ClientHello right behind a
    //    CONNECT) stay in `preloaded` and must not be lost.
    let mut buf = BytesMut::with_capacity(8192);
    let head_len = loop {
        let n = stream.read_buf(&mut buf).await.map_err(PhantomError::Io)?;
        if n == 0 {
            return Err(PhantomError::Protocol(
                "HTTP connection closed before head".to_string(),
            ));
        }
        if let Some(end) = find_head_end(&buf) {
            break end;
        }
        if buf.len() > MAX_HTTP_HEAD {
            let _ = stream.write_all(RESP_HEAD_TOO_LARGE).await;
            return Err(PhantomError::Protocol("HTTP head too large".to_string()));
        }
    };
    let head = buf[..head_len].to_vec();
    let preloaded = buf[head_len..].to_vec();

    // 2. Authenticate when the inbound is shared on a LAN. The header is
    //    stripped before forwarding (see parse_request), so credentials
    //    never reach the origin server.
    if !check_proxy_authorization(&head, config.client.proxy_auth.as_ref()) {
        let _ = stream.write_all(RESP_AUTH_REQUIRED).await;
        return Err(PhantomError::Protocol(
            "HTTP proxy authentication failed".to_string(),
        ));
    }

    // 3. Parse the request; malformed requests get a 400 and a close.
    let request = match parse_request(&head) {
        Ok(r) => r,
        Err(e) => {
            let _ = stream.write_all(RESP_BAD_REQUEST).await;
            return Err(e);
        }
    };
    tracing::info!(
        "HTTP {} → {} ({})",
        if request.is_connect { "CONNECT" } else { "proxy" },
        request.target,
        if request.is_connect { "tunnel" } else { "rewrite" }
    );

    // 4. Select server and establish the tunnel (same plumbing as SOCKS5).
    //    The two transports monomorphize to different frame reader/writer
    //    types, so each branch finishes on its own via the generic helper.
    let (server, migration_rx) = failover.select_server_with_migration()?;
    match server.protocol {
        TransportProtocol::Tcp => {
            let transport = TcpTransport::new(std::time::Duration::from_secs(10));
            match socks5::establish_tunnel(
                &transport,
                &server,
                &local_secret,
                &request.target,
                config.client.cipher,
            )
            .await
            {
                Ok((fr, fw, sid)) => {
                    finish_http_tunnel(stream, fr, fw, sid, request, preloaded, stats, migration_rx)
                        .await
                }
                Err(e) => {
                    tracing::info!("HTTP tunnel failed → {}: {}", request.target, e);
                    let _ = stream.write_all(RESP_BAD_GATEWAY).await;
                    Err(e)
                }
            }
        }
        TransportProtocol::Quic => {
            match socks5::establish_quic_tunnel(
                quic_pool,
                &server,
                &local_secret,
                &request.target,
                config.client.cipher,
            )
            .await
            {
                Ok((fr, fw, sid)) => {
                    finish_http_tunnel(stream, fr, fw, sid, request, preloaded, stats, migration_rx)
                        .await
                }
                Err(e) => {
                    tracing::info!("HTTP tunnel failed → {}: {}", request.target, e);
                    let _ = stream.write_all(RESP_BAD_GATEWAY).await;
                    Err(e)
                }
            }
        }
    }
}

/// Inject the rewritten head and any preloaded bytes, then hand the stream
/// to the shared byte relay (which also handles migration cutover).
async fn finish_http_tunnel<M, N>(
    mut stream: TcpStream,
    frame_reader: FrameReader<M>,
    mut frame_writer: FrameWriter<N>,
    stream_id: u32,
    request: ParsedRequest,
    preloaded: Vec<u8>,
    stats: &Arc<TrafficStats>,
    migration_rx: watch::Receiver<u64>,
) -> Result<()>
where
    M: MessageRead,
    N: MessageWrite,
{
    stats.record_tcp_connect();
    if request.is_connect {
        stream.write_all(RESP_OK).await.map_err(PhantomError::Io)?;
        stream.flush().await.map_err(PhantomError::Io)?;
    }
    if let Some(head) = request.forward_head {
        frame_writer
            .write_frame(&Frame::data(stream_id, Bytes::from(head)))
            .await?;
    }
    if !preloaded.is_empty() {
        frame_writer
            .write_frame(&Frame::data(stream_id, Bytes::from(preloaded)))
            .await?;
    }
    frame_writer.flush().await?;
    socks5::relay_socks5_tunnel(
        stream,
        frame_reader,
        frame_writer,
        stream_id,
        &request.target,
        stats,
        migration_rx,
    )
    .await
}

/// Verify `Proxy-Authorization: Basic base64(user:pass)` against the
/// configured credentials. Always true when no auth is configured; the
/// comparison is constant-time on the decoded `user:pass` bytes.
fn check_proxy_authorization(head: &[u8], auth: Option<&ProxyAuthConfig>) -> bool {
    let Some(auth) = auth else { return true };
    let Ok(text) = std::str::from_utf8(head) else {
        return false;
    };
    for line in text.split("\r\n").skip(1) {
        let Some((name, value)) = line.split_once(':') else {
            continue;
        };
        if !name.trim().eq_ignore_ascii_case("proxy-authorization") {
            continue;
        }
        let mut parts = value.trim().split_whitespace();
        let (Some(scheme), Some(token)) = (parts.next(), parts.next()) else {
            return false;
        };
        if !scheme.eq_ignore_ascii_case("basic") {
            return false;
        }
        let Ok(decoded) = STANDARD.decode(token) else {
            return false;
        };
        let expected = format!("{}:{}", auth.username, auth.password);
        return socks5::constant_time_eq(&decoded, expected.as_bytes());
    }
    false
}

/// Locate the end of the HTTP head (position just past CRLFCRLF).
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

/// Parse the request head into a tunnel target plus the head to forward.
fn parse_request(head: &[u8]) -> Result<ParsedRequest> {
    let text = std::str::from_utf8(head)
        .map_err(|_| PhantomError::Protocol("HTTP head is not UTF-8".to_string()))?;
    let mut lines = text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| PhantomError::Protocol("empty HTTP request".to_string()))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| PhantomError::Protocol("missing HTTP method".to_string()))?;
    let uri = parts
        .next()
        .ok_or_else(|| PhantomError::Protocol("missing request target".to_string()))?;
    let version = parts.next().unwrap_or("HTTP/1.1");

    if method.eq_ignore_ascii_case("CONNECT") {
        let target = parse_authority(uri, 443)?;
        return Ok(ParsedRequest {
            target,
            forward_head: None,
            is_connect: true,
        });
    }

    let rest = uri.strip_prefix("http://").ok_or_else(|| {
        PhantomError::Protocol(format!("proxy requires an absolute http:// URI, got {}", uri))
    })?;
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let target = parse_authority(authority, 80)?;

    // Rewrite to origin form; strip proxy metadata; force close so the
    // relay never has to re-target a keep-alive follow-up request.
    let mut out = format!("{} {} {}\r\n", method, path, version);
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let name = line.split(':').next().unwrap_or("");
        if name.eq_ignore_ascii_case("proxy-connection")
            || name.eq_ignore_ascii_case("proxy-authorization")
            || name.eq_ignore_ascii_case("connection")
        {
            continue;
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    out.push_str("Connection: close\r\n\r\n");

    Ok(ParsedRequest {
        target,
        forward_head: Some(out.into_bytes()),
        is_connect: false,
    })
}

/// Parse `host[:port]` / `v4[:port]` / `[v6][:port]` into a TargetAddr.
fn parse_authority(authority: &str, default_port: u16) -> Result<TargetAddr> {
    if let Some(rest) = authority.strip_prefix('[') {
        let end = rest
            .find(']')
            .ok_or_else(|| PhantomError::Protocol("bad IPv6 authority".to_string()))?;
        let ip: std::net::Ipv6Addr = rest[..end]
            .parse()
            .map_err(|e| PhantomError::Protocol(format!("bad IPv6 address: {}", e)))?;
        let port = match rest[end + 1..].strip_prefix(':') {
            Some(p) => p
                .parse()
                .map_err(|_| PhantomError::Protocol("bad port".to_string()))?,
            None => default_port,
        };
        return Ok(TargetAddr::IPv6(ip.octets(), port));
    }
    // Bare IPv6 without brackets carries no port.
    if authority.matches(':').count() > 1 {
        let ip: std::net::Ipv6Addr = authority
            .parse()
            .map_err(|e| PhantomError::Protocol(format!("bad IPv6 address: {}", e)))?;
        return Ok(TargetAddr::IPv6(ip.octets(), default_port));
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => {
            let port: u16 = p
                .parse()
                .map_err(|_| PhantomError::Protocol(format!("bad port in {}", authority)))?;
            (h, port)
        }
        None => (authority, default_port),
    };
    if let Ok(v4) = host.parse::<std::net::Ipv4Addr>() {
        return Ok(TargetAddr::IPv4(v4.octets(), port));
    }
    if host.is_empty() || host.len() > 253 {
        return Err(PhantomError::Protocol(format!("bad host: {}", host)));
    }
    Ok(TargetAddr::Domain(host.to_string(), port))
}
