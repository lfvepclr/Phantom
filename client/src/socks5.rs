use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::BytesMut;
use phantom_core::CipherPreference;
use phantom_core::TransportProtocol;
use phantom_core::constants::MAX_FRAME_PAYLOAD;
use phantom_core::crypto::cipher::CipherSuite;
use phantom_core::crypto::session::CipherOffer;
use phantom_core::crypto::{NoiseInitiator, SessionReader, SessionWriter, split_after_handshake};
use phantom_core::protocol::codec::{
    FrameReader, FrameWriter, MessageRead, MessageWrite, PlainMessageReader, PlainMessageWriter,
};
use phantom_core::protocol::frame::FrameFlags;
use phantom_core::protocol::{Frame, TargetAddr};
use phantom_core::transport::Transport;
use phantom_core::transport::quic::QuicStream;
use phantom_core::transport::tcp::TcpTransport;
use phantom_core::{ClientConfig, PhantomError, ProxyAuthConfig, Result, ServerEntry};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::failover::FailoverManager;
use crate::quic_pool::QuicPool;
use crate::stats::TrafficStats;
use crate::udp_relay::{UdpFlowChannels, establish_udp_flow_quic, establish_udp_flow_tcp};
use crate::tcp_pool::TcpSessionPool;
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::sync::mpsc::UnboundedSender;

pub async fn handle_socks5_connection(
    mut socks5: TcpStream,
    config: &ClientConfig,
    failover: &FailoverManager,
    quic_pool: &QuicPool,
    tcp_pool: &Arc<TcpSessionPool>,
    local_secret: [u8; 32],
    stats: &Arc<TrafficStats>,
) -> Result<()> {
    // 1. SOCKS5 method negotiation (RFC1929 username/password when the
    //    inbound is shared on a LAN with `client.proxy_auth` configured).
    negotiate_method(&mut socks5, config.client.proxy_auth.as_ref()).await?;

    // 2. SOCKS5 request
    let (cmd, target, prerouted) = read_request(&mut socks5).await?;
    tracing::info!("SOCKS5 target: {} (cmd={:#x})", target, cmd);

    if cmd == SOCKS5_CMD_UDP_ASSOCIATE {
        return handle_udp_associate(socks5, config, failover, quic_pool, local_secret, stats)
            .await;
    }

    // 2b. Routing decision (default is DIRECT — Phantom only tunnels what the
    //     whitelist says needs it; see `crate::whitelist`).
    //
    //     The TUN transparent proxy reaches this listener for flows it has
    //     *already* routed. It marks those requests (`ATYP_PREROUTED`) because
    //     by the time the target is an IP here the domain context that drove
    //     the original decision is gone — re-deciding would silently turn a
    //     whitelisted destination back into a direct connection.
    let (decision_domain, decision_ip) = match &target {
        TargetAddr::Domain(d, _) => (Some(d.as_str()), None),
        TargetAddr::IPv4(octets, _) => (None, Some(IpAddr::from(*octets))),
        TargetAddr::IPv6(octets, _) => (None, Some(IpAddr::from(*octets))),
    };
    let target_port = match &target {
        TargetAddr::Domain(_, p) | TargetAddr::IPv4(_, p) | TargetAddr::IPv6(_, p) => *p,
    };
    let decision = if prerouted {
        // Only trusted for loopback callers (the TUN proxy), never for a
        // LAN-shared proxy where a remote client could force the tunnel.
        if !is_loopback_peer(&socks5) {
            return Err(PhantomError::Protocol(
                "pre-routed SOCKS5 request from a non-loopback peer".into(),
            ));
        }
        // The TUN path already logged this decision; keep the relay quiet so
        // one connection does not produce two route lines.
        tracing::debug!("route {} -> PROXY (decided by TUN)", target);
        crate::whitelist::RouteDecision {
            action: phantom_core::RuleAction::Proxy,
            reason: crate::whitelist::RouteReason::Whitelist,
        }
    } else {
        let router = crate::whitelist::shared(config);
        let decision = router.decide(decision_domain, decision_ip, target_port);
        tracing::info!(
            "route {} -> {} ({})",
            target,
            if decision.is_direct() {
                "DIRECT"
            } else {
                "PROXY"
            },
            decision.reason.as_str()
        );
        decision
    };

    if decision.is_direct() {
        stats.record_route_direct();
        return handle_direct_connect(socks5, &target, stats).await;
    }
    stats.record_route_proxy();

    // 3. Select server via failover manager (owned snapshot: the pool is
    // hot-reloadable, so we must not hold its lock across the relay). The
    // migration watcher is subscribed atomically with the selection so a
    // hard failover (`graceful_migration = false`) always reaches this relay.
    let (server, migration_rx) = failover.select_server_with_migration()?;

    // 4. Establish encrypted tunnel via selected transport protocol
    // Per-server cipher (URI `cipher=`) wins over the client-wide default.
    let effective_cipher = CipherPreference::effective_for(server.cipher, config.client.cipher);
    tracing::info!("Connecting to server {} ({})", server.name, server.address);
    match server.protocol {
        TransportProtocol::Tcp => {
            match open_tcp_tunnel(tcp_pool, &server, &local_secret, &target, effective_cipher).await
            {
                Ok((frame_reader, frame_writer, stream_id)) => {
                    tracing::info!(
                        "Tunnel established → {} (cipher={:?})",
                        target,
                        effective_cipher
                    );
                    stats.record_tcp_connect();
                    send_reply(&mut socks5, 0x00).await?;
                    relay_socks5_tunnel(
                        socks5,
                        frame_reader,
                        frame_writer,
                        stream_id,
                        &target,
                        stats,
                        migration_rx,
                    )
                    .await
                }
                Err(e) => {
                    tracing::info!("Tunnel failed → {}: {}", target, e);
                    // Datapath evidence: transport/handshake failures (Io,
                    // Timeout) mean the server itself is unreachable — feed
                    // failover immediately instead of waiting for the next
                    // health-probe tick. Protocol refusals (the server is up
                    // but rejected the target) must not count.
                    if matches!(
                        e,
                        phantom_core::PhantomError::Io(_) | phantom_core::PhantomError::Timeout
                    ) {
                        failover.report_datapath_failure(&server.name);
                    }
                    let _ = send_reply(&mut socks5, 0x05).await;
                    Err(e)
                }
            }
        }
        TransportProtocol::Quic => {
            // QUIC is authenticated at connection level (Noise inside QUIC),
            // so streams run the bare frame protocol over the pooled
            // connection — one stream per SOCKS5 tunnel.
            match establish_quic_tunnel(
                quic_pool,
                &server,
                &local_secret,
                &target,
                effective_cipher,
            )
            .await
            {
                Ok((frame_reader, frame_writer, stream_id)) => {
                    tracing::info!(
                        "Tunnel established → {} (cipher={:?})",
                        target,
                        effective_cipher
                    );
                    stats.record_tcp_connect();
                    send_reply(&mut socks5, 0x00).await?;
                    relay_socks5_tunnel(
                        socks5,
                        frame_reader,
                        frame_writer,
                        stream_id,
                        &target,
                        stats,
                        migration_rx,
                    )
                    .await
                }
                Err(e) => {
                    tracing::info!("Tunnel failed → {}: {}", target, e);
                    // Same datapath evidence as the TCP branch above.
                    if matches!(
                        e,
                        phantom_core::PhantomError::Io(_) | phantom_core::PhantomError::Timeout
                    ) {
                        failover.report_datapath_failure(&server.name);
                    }
                    let _ = send_reply(&mut socks5, 0x05).await;
                    Err(e)
                }
            }
        }
    }
}

/// Serve a SOCKS5 CONNECT locally, without touching the tunnel.
///
/// Used for the default (direct) route: domestic destinations connect from this
/// machine using the local resolver, which is both faster and keeps the VPS's
/// tiny uplink free.
async fn handle_direct_connect(
    mut socks5: TcpStream,
    target: &TargetAddr,
    stats: &Arc<TrafficStats>,
) -> Result<()> {
    let addr = match resolve_local(target).await {
        Ok(a) => a,
        Err(e) => {
            tracing::info!("Direct connect failed -> {}: {}", target, e);
            let _ = send_reply(&mut socks5, 0x04).await;
            return Ok(());
        }
    };

    let mut upstream = match TcpStream::connect(addr).await {
        Ok(s) => s,
        Err(e) => {
            tracing::info!("Direct connect failed -> {}: {}", target, e);
            let _ = send_reply(&mut socks5, 0x05).await;
            return Ok(());
        }
    };
    crate::net_tune::tune(&upstream);
    let bound = upstream
        .local_addr()
        .unwrap_or_else(|_| SocketAddr::new(IpAddr::from([0, 0, 0, 0]), 0));
    send_reply_addr(&mut socks5, 0x00, &bound).await?;
    tracing::info!("Direct connection established -> {}", target);

    let (up, down) = tokio::io::copy_bidirectional(&mut socks5, &mut upstream).await?;
    stats.record_tcp_up(up);
    stats.record_tcp_down(down);
    Ok(())
}

/// Resolve a target for a **direct** (non-tunnelled) connection.
///
/// Domains are resolved with the *local* resolver on purpose: direct
/// destinations are not censored, and local answers keep CDN locality. Proxied
/// destinations never reach this function, so censored domains are never
/// resolved locally (no poisoned cache entries).
pub(crate) async fn resolve_local(target: &TargetAddr) -> std::io::Result<SocketAddr> {
    match target {
        TargetAddr::Domain(host, port) => {
            let mut addrs = tokio::net::lookup_host((host.as_str(), *port)).await?;
            addrs.next().ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    format!("no address for {}", host),
                )
            })
        }
        TargetAddr::IPv4(octets, port) => Ok(SocketAddr::new(IpAddr::from(*octets), *port)),
        TargetAddr::IPv6(octets, port) => Ok(SocketAddr::new(IpAddr::from(*octets), *port)),
    }
}

const SOCKS5_METHOD_NONE: u8 = 0x00;
const SOCKS5_METHOD_USERPASS: u8 = 0x02;
const SOCKS5_METHOD_NO_ACCEPTABLE: u8 = 0xFF;

async fn negotiate_method(stream: &mut TcpStream, auth: Option<&ProxyAuthConfig>) -> Result<()> {
    let mut buf = [0u8; 2];
    stream
        .read_exact(&mut buf)
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 negotiation read failed: {}", e)))?;

    if buf[0] != 0x05 {
        return Err(PhantomError::Protocol(format!(
            "Not SOCKS5: version {}",
            buf[0]
        )));
    }

    let nmethods = buf[1] as usize;
    let mut methods = vec![0u8; nmethods];
    stream
        .read_exact(&mut methods)
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 methods read failed: {}", e)))?;

    // With credentials configured, only username/password is acceptable;
    // without, only no-auth is. Never downgrade across the two policies.
    let required = if auth.is_some() {
        SOCKS5_METHOD_USERPASS
    } else {
        SOCKS5_METHOD_NONE
    };
    if !methods.contains(&required) {
        let _ = stream.write_all(&[0x05, SOCKS5_METHOD_NO_ACCEPTABLE]).await;
        return Err(PhantomError::Protocol(
            "No acceptable SOCKS5 auth method".to_string(),
        ));
    }

    stream
        .write_all(&[0x05, required])
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 negotiation reply failed: {}", e)))?;

    if required == SOCKS5_METHOD_USERPASS {
        let auth = auth.expect("userpass method implies configured credentials");
        rfc1929_authenticate(stream, auth).await?;
    }

    Ok(())
}

/// RFC 1929 username/password sub-negotiation: VER(1) ULEN UNAME PLEN PASSWD.
async fn rfc1929_authenticate(stream: &mut TcpStream, expected: &ProxyAuthConfig) -> Result<()> {
    let mut hdr = [0u8; 2];
    stream
        .read_exact(&mut hdr)
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 auth read failed: {}", e)))?;
    if hdr[0] != 0x01 {
        return Err(PhantomError::Protocol(format!(
            "Bad SOCKS5 auth version: {}",
            hdr[0]
        )));
    }
    // ULEN/PLEN are one byte each, so both fields are naturally ≤255 bytes.
    let mut uname = vec![0u8; hdr[1] as usize];
    stream
        .read_exact(&mut uname)
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 auth read failed: {}", e)))?;
    let mut plen = [0u8; 1];
    stream
        .read_exact(&mut plen)
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 auth read failed: {}", e)))?;
    let mut passwd = vec![0u8; plen[0] as usize];
    stream
        .read_exact(&mut passwd)
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 auth read failed: {}", e)))?;

    let ok = constant_time_eq(&uname, expected.username.as_bytes())
        && constant_time_eq(&passwd, expected.password.as_bytes());
    let status = if ok { 0x00 } else { 0x01 };
    stream
        .write_all(&[0x01, status])
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 auth reply failed: {}", e)))?;
    if !ok {
        return Err(PhantomError::Protocol(
            "SOCKS5 username/password mismatch".to_string(),
        ));
    }
    Ok(())
}

/// Length-and-content comparison without early exit, so credential checks do
/// not leak a timing oracle to local-network attackers.
pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

const SOCKS5_CMD_CONNECT: u8 = 0x01;
const SOCKS5_CMD_UDP_ASSOCIATE: u8 = 0x03;

/// Phantom-internal address type: "the caller already applied the routing
/// policy". Followed by a regular address type byte (0x01/0x04/0x03).
///
/// Used by the TUN transparent proxy when it reaches the local SOCKS5 ingress,
/// which has no domain information left to re-derive the decision from.
pub const ATYP_PREROUTED: u8 = 0x80;

async fn read_request(stream: &mut TcpStream) -> Result<(u8, TargetAddr, bool)> {
    let mut header = [0u8; 4];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 request read failed: {}", e)))?;

    if header[0] != 0x05 {
        return Err(PhantomError::Protocol(format!(
            "Not SOCKS5: version {}",
            header[0]
        )));
    }

    // CONNECT (0x01) and UDP ASSOCIATE (0x03); BIND (0x02) is not supported.
    if header[1] != SOCKS5_CMD_CONNECT && header[1] != SOCKS5_CMD_UDP_ASSOCIATE {
        let _ = send_reply(stream, 0x07).await;
        return Err(PhantomError::Protocol(format!(
            "Unsupported SOCKS5 command: {}",
            header[1]
        )));
    }
    let cmd = header[1];

    // `ATYP_PREROUTED` is a Phantom-internal marker: the routing policy has
    // already been applied upstream (TUN proxy), so the address that follows
    // is relayed as-is instead of being judged again.
    let mut prerouted = false;
    let mut atyp = header[3];
    if atyp == ATYP_PREROUTED {
        prerouted = true;
        let mut inner = [0u8; 1];
        stream
            .read_exact(&mut inner)
            .await
            .map_err(PhantomError::Io)?;
        atyp = inner[0];
    }
    let target = match atyp {
        0x01 => {
            let mut addr = [0u8; 4];
            stream
                .read_exact(&mut addr)
                .await
                .map_err(PhantomError::Io)?;
            let mut port_buf = [0u8; 2];
            stream
                .read_exact(&mut port_buf)
                .await
                .map_err(PhantomError::Io)?;
            let port = u16::from_be_bytes(port_buf);
            TargetAddr::IPv4(addr, port)
        }
        0x03 => {
            let mut len_buf = [0u8; 1];
            stream
                .read_exact(&mut len_buf)
                .await
                .map_err(PhantomError::Io)?;
            let domain_len = len_buf[0] as usize;
            let mut domain = vec![0u8; domain_len];
            stream
                .read_exact(&mut domain)
                .await
                .map_err(PhantomError::Io)?;
            let domain_str = String::from_utf8(domain)
                .map_err(|e| PhantomError::Protocol(format!("Invalid domain: {}", e)))?;
            let mut port_buf = [0u8; 2];
            stream
                .read_exact(&mut port_buf)
                .await
                .map_err(PhantomError::Io)?;
            let port = u16::from_be_bytes(port_buf);
            TargetAddr::Domain(domain_str, port)
        }
        0x04 => {
            let mut addr = [0u8; 16];
            stream
                .read_exact(&mut addr)
                .await
                .map_err(PhantomError::Io)?;
            let mut port_buf = [0u8; 2];
            stream
                .read_exact(&mut port_buf)
                .await
                .map_err(PhantomError::Io)?;
            let port = u16::from_be_bytes(port_buf);
            TargetAddr::IPv6(addr, port)
        }
        _ => {
            return Err(PhantomError::Protocol(format!(
                "Unsupported address type: {}",
                atyp
            )));
        }
    };

    Ok((cmd, target, prerouted))
}

/// Whether the SOCKS5 peer is on loopback (the only place the internal
/// `ATYP_PREROUTED` marker is honoured).
fn is_loopback_peer(stream: &TcpStream) -> bool {
    stream
        .peer_addr()
        .map(|addr| addr.ip().is_loopback())
        .unwrap_or(false)
}

async fn send_reply(stream: &mut TcpStream, reply: u8) -> Result<()> {
    let reply_bytes: [u8; 10] = [0x05, reply, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
    stream
        .write_all(&reply_bytes)
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 reply failed: {}", e)))?;
    stream
        .flush()
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 flush failed: {}", e)))?;
    Ok(())
}

/// SOCKS5 reply carrying a concrete BND.ADDR/BND.PORT. UDP ASSOCIATE must
/// tell the client where to send datagrams, so the hardcoded 0.0.0.0:0 of
/// `send_reply` does not apply here.
async fn send_reply_addr(stream: &mut TcpStream, reply: u8, addr: &SocketAddr) -> Result<()> {
    let mut buf = vec![0x05, reply, 0x00];
    match addr.ip() {
        IpAddr::V4(v4) => {
            buf.push(0x01);
            buf.extend_from_slice(&v4.octets());
        }
        IpAddr::V6(v6) => {
            buf.push(0x04);
            buf.extend_from_slice(&v6.octets());
        }
    }
    buf.extend_from_slice(&addr.port().to_be_bytes());
    stream.write_all(&buf).await.map_err(PhantomError::Io)?;
    stream.flush().await.map_err(PhantomError::Io)?;
    Ok(())
}

/// Parse a SOCKS5 UDP request header: RSV(2) FRAG(1) ATYP+ADDR+PORT DATA.
/// Returns the target and the DATA slice. Fragments (FRAG != 0) and
/// malformed packets are dropped per RFC 1928 §7.
fn parse_udp_request(pkt: &[u8]) -> Option<(TargetAddr, &[u8])> {
    if pkt.len() < 4 || pkt[0] != 0 || pkt[1] != 0 || pkt[2] != 0 {
        return None;
    }
    let addr_len = match pkt[3] {
        0x01 => 1 + 4 + 2,
        0x03 => {
            if pkt.len() < 5 {
                return None;
            }
            2 + pkt[4] as usize + 2
        }
        0x04 => 1 + 16 + 2,
        _ => return None,
    };
    if pkt.len() < 3 + addr_len {
        return None;
    }
    let target = TargetAddr::decode(&pkt[3..3 + addr_len]).ok()?;
    Some((target, &pkt[3 + addr_len..]))
}

/// Wrap a datagram from the tunnel back into a SOCKS5 UDP request header
/// (RSV FRAG ATYP+ADDR+PORT DATA) for delivery to the local client.
fn wrap_udp_request(target: &TargetAddr, data: &[u8]) -> Vec<u8> {
    let addr = target.encode();
    let mut pkt = Vec::with_capacity(3 + addr.len() + data.len());
    pkt.extend_from_slice(&[0, 0, 0]);
    pkt.extend_from_slice(&addr);
    pkt.extend_from_slice(data);
    pkt
}

/// SOCKS5 UDP ASSOCIATE handler.
///
/// Binds a UDP socket on the TCP control connection's local IP, replies with
/// the bound address, then relays datagrams through per-target tunnel flows
/// (shared plumbing in `udp_relay`). Per RFC 1928: only datagrams from the
/// first-seen client address are accepted, and the association ends when the
/// control connection hits EOF.
async fn handle_udp_associate(
    mut socks5: TcpStream,
    config: &ClientConfig,
    failover: &FailoverManager,
    quic_pool: &QuicPool,
    local_secret: [u8; 32],
    stats: &Arc<TrafficStats>,
) -> Result<()> {
    let local_ip = socks5.local_addr().map_err(PhantomError::Io)?.ip();
    let udp = UdpSocket::bind(SocketAddr::new(local_ip, 0))
        .await
        .map_err(PhantomError::Io)?;
    let udp_addr = udp.local_addr().map_err(PhantomError::Io)?;
    let udp = Arc::new(udp);
    tracing::info!("UDP ASSOCIATE relay at {}", udp_addr);

    send_reply_addr(&mut socks5, 0x00, &udp_addr).await?;

    // Keyed by `TargetAddr::encode()` — TargetAddr has no Eq/Hash.
    let mut flows: HashMap<Vec<u8>, (TargetAddr, UnboundedSender<Vec<u8>>)> = HashMap::new();
    let mut client_addr: Option<SocketAddr> = None;
    let mut buf = vec![0u8; 65536];
    let mut ctl = [0u8; 16];

    loop {
        tokio::select! {
            res = socks5.read(&mut ctl) => {
                // Any EOF or error on the control connection ends the
                // association; payload bytes (there should be none) are ignored.
                match res {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {}
                }
            }
            res = udp.recv_from(&mut buf) => {
                let (n, src) = match res {
                    Ok(v) => v,
                    Err(e) => {
                        tracing::debug!("UDP relay recv error: {}", e);
                        break;
                    }
                };
                match client_addr {
                    None => client_addr = Some(src),
                    Some(addr) if addr != src => continue,
                    _ => {}
                }
                let (target, data) = match parse_udp_request(&buf[..n]) {
                    Some(v) => v,
                    None => continue,
                };
                stats.record_udp_up(data.len() as u64);

                let key = target.encode().to_vec();
                let mut established = false;
                if let Some((_, tx)) = flows.get(&key) {
                    if tx.send(data.to_vec()).is_ok() {
                        continue;
                    }
                    // Dead sender: the pump ended, re-establish below.
                    flows.remove(&key);
                    established = true;
                }

                let server = match failover.select_server() {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::debug!("UDP flow → {}: no server: {}", target, e);
                        continue;
                    }
                };
                let flow = match server.protocol {
                    TransportProtocol::Tcp => {
                        establish_udp_flow_tcp(&server, &local_secret, target.clone(), data.to_vec())
                            .await
                    }
                    TransportProtocol::Quic => {
                        establish_udp_flow_quic(
                            quic_pool,
                            &server,
                            &local_secret,
                            CipherPreference::effective_for(server.cipher, config.client.cipher),
                            target.clone(),
                            data.to_vec(),
                        )
                        .await
                    }
                };
                let UdpFlowChannels { outbound, inbound } = match flow {
                    Ok(f) => f,
                    Err(e) => {
                        tracing::debug!("UDP flow → {}: establish failed: {}", target, e);
                        continue;
                    }
                };
                if established {
                    tracing::debug!("UDP flow → {} re-established", target);
                }
                flows.insert(key, (target.clone(), outbound));

                // Inbound pump: tunnel datagrams → SOCKS5 UDP header → client.
                if let Some(ca) = client_addr {
                    let udp_in = Arc::clone(&udp);
                    let stats_in = Arc::clone(stats);
                    let mut inbound = inbound;
                    tokio::spawn(async move {
                        while let Some(d) = inbound.recv().await {
                            stats_in.record_udp_down(d.len() as u64);
                            let pkt = wrap_udp_request(&target, &d);
                            if udp_in.send_to(&pkt, ca).await.is_err() {
                                break;
                            }
                        }
                    });
                }
            }
        }
    }

    tracing::debug!("UDP ASSOCIATE closed (control connection EOF)");
    Ok(())
}

pub(crate) fn resolve_offer(cipher_preference: CipherPreference) -> CipherOffer {
    match cipher_preference {
        CipherPreference::Auto => CipherOffer::default_offer(),
        CipherPreference::Aes256Gcm => CipherOffer::new(vec![CipherSuite::Aes256Gcm]),
        CipherPreference::Aes128Gcm => CipherOffer::new(vec![CipherSuite::Aes128Gcm]),
        CipherPreference::Ascon128 => CipherOffer::new(vec![CipherSuite::Ascon128]),
        CipherPreference::ChaCha20Poly1305 => CipherOffer::new(vec![CipherSuite::ChaCha20Poly]),
    }
}

pub(crate) async fn establish_tunnel<T: Transport>(
    transport: &T,
    server: &ServerEntry,
    local_secret: &[u8; 32],
    target: &TargetAddr,
    cipher_preference: CipherPreference,
) -> Result<(
    FrameReader<SessionReader<tokio::io::ReadHalf<T::Stream>>>,
    FrameWriter<SessionWriter<tokio::io::WriteHalf<T::Stream>>>,
    u32,
)> {
    let addr: std::net::SocketAddr = server
        .address
        .parse()
        .map_err(|e| PhantomError::Config(format!("Invalid server address: {}", e)))?;

    let stream = transport.connect(&addr).await?;

    let remote_public = decode_public_key(&server.public_key)?;
    let initiator = NoiseInitiator::new(local_secret, &remote_public, server.decode_psk()?);
    let offer = resolve_offer(cipher_preference);
    let result = initiator.handshake(stream, &offer).await?;

    tracing::debug!("Cipher negotiated: {}", result.chosen_cipher);

    let (session_reader, session_writer) = split_after_handshake(
        result.stream,
        result.split_keys,
        result.chosen_cipher,
        result.is_initiator,
    );
    let mut frame_reader = FrameReader::new(session_reader);
    let mut frame_writer = FrameWriter::new(session_writer);

    let stream_id = syn_handshake(&mut frame_reader, &mut frame_writer, server, target).await?;
    Ok((frame_reader, frame_writer, stream_id))
}

/// QUIC variant: the connection from the pool is already Noise-authenticated,
/// so the stream goes straight to the frame protocol with plaintext
/// length-prefix framing.
pub(crate) async fn establish_quic_tunnel(
    pool: &QuicPool,
    server: &ServerEntry,
    local_secret: &[u8; 32],
    target: &TargetAddr,
    cipher_preference: CipherPreference,
) -> Result<(
    FrameReader<PlainMessageReader<tokio::io::ReadHalf<QuicStream>>>,
    FrameWriter<PlainMessageWriter<tokio::io::WriteHalf<QuicStream>>>,
    u32,
)> {
    let (send, recv) = pool
        .open_bi(
            server,
            local_secret,
            cipher_preference,
            std::time::Duration::from_secs(10),
        )
        .await?;
    let stream = QuicStream::new(send, recv);
    let (read_half, write_half) = tokio::io::split(stream);
    let mut frame_reader = FrameReader::new(PlainMessageReader::new(read_half));
    let mut frame_writer = FrameWriter::new(PlainMessageWriter::new(write_half));

    let stream_id = syn_handshake(&mut frame_reader, &mut frame_writer, server, target).await?;
    Ok((frame_reader, frame_writer, stream_id))
}

/// A tunnel ready to relay bytes: framed reader, framed writer, stream id.
/// Public so integration tests (and future clients) can drive the same path the
/// proxy uses instead of hand-rolling a handshake.
pub type TcpTunnel = (
    FrameReader<SessionReader<tokio::io::ReadHalf<TcpStream>>>,
    FrameWriter<SessionWriter<tokio::io::WriteHalf<TcpStream>>>,
    u32,
);

/// Open a TCP tunnel for `target`, reusing a pooled session when one is ready.
///
/// A cold tunnel costs two round trips before the first byte moves — TCP
/// connect plus the Noise handshake. The pool keeps authenticated sessions
/// waiting, so a flow that finds one starts relaying immediately and only pays
/// the SYN/ACK inside the tunnel (roughly one round trip).
///
/// A pooled session that fails mid-SYN is dropped and the flow retries on a
/// fresh connection. That is the half-open guard: the pool can be wrong about
/// whether a session is still alive, but the *flow* never notices.
pub async fn open_tcp_tunnel(
    pool: &Arc<TcpSessionPool>,
    server: &ServerEntry,
    local_secret: &[u8; 32],
    target: &TargetAddr,
    cipher_preference: CipherPreference,
) -> Result<TcpTunnel> {
    // Top the pool back up for the *next* flow before this one consumes
    // anything; the refill is spawned, so it never delays the caller.
    pool.spawn_refill(server.clone(), *local_secret, cipher_preference);

    if let Some(session) = pool.take(server, cipher_preference).await {
        let idle_ms = session.idle_ms();
        let mut frame_reader = FrameReader::new(session.reader);
        let mut frame_writer = FrameWriter::new(session.writer);
        match syn_handshake(&mut frame_reader, &mut frame_writer, server, target).await {
            Ok(stream_id) => {
                tracing::info!(
                    "Tunnel established → {} (cipher={:?}, pooled session, idle {} ms)",
                    target,
                    cipher_preference,
                    idle_ms
                );
                return Ok((frame_reader, frame_writer, stream_id));
            }
            Err(e) => {
                // The session died while it waited (server restart, NAT rebind,
                // idle timeout somewhere in the path). The flow simply pays the
                // handshake the pool was meant to save it.
                tracing::info!(
                    "Pooled session unusable for {} ({e}); reconnecting cold",
                    target
                );
            }
        }
    }

    let transport = TcpTransport::new(std::time::Duration::from_secs(10));
    establish_tunnel(
        &transport,
        server,
        local_secret,
        target,
        cipher_preference,
    )
    .await
}

/// Shared tunnel bootstrap: send SYN, expect ACK/RST. Identical on both
/// transports — only the message framing underneath differs.
pub(crate) async fn syn_handshake<M: MessageRead, N: MessageWrite>(
    frame_reader: &mut FrameReader<M>,
    frame_writer: &mut FrameWriter<N>,
    server: &ServerEntry,
    target: &TargetAddr,
) -> Result<u32> {
    let stream_id: u32 = 1;
    frame_writer
        .write_frame(&Frame::syn(stream_id, target.encode()))
        .await?;
    frame_writer.flush().await?;

    let response = frame_reader.read_frame().await?;
    if response.flags.contains(FrameFlags::RST) {
        return Err(PhantomError::ServerUnreachable {
            name: server.name.clone(),
        });
    }
    if !response.flags.contains(FrameFlags::ACK) {
        return Err(PhantomError::Protocol("Expected ACK".to_string()));
    }

    Ok(stream_id)
}

pub(crate) fn decode_public_key(b64: &str) -> Result<[u8; 32]> {
    let decoded = STANDARD
        .decode(b64.trim())
        .map_err(|e| PhantomError::Crypto(format!("Base64 decode failed: {}", e)))?;
    if decoded.len() != 32 {
        return Err(PhantomError::Crypto(format!(
            "Public key must be 32 bytes, got {}",
            decoded.len()
        )));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&decoded);
    Ok(key)
}

pub async fn relay_socks5_tunnel<M, N>(
    socks5: TcpStream,
    mut frame_reader: FrameReader<M>,
    mut frame_writer: FrameWriter<N>,
    stream_id: u32,
    target: &TargetAddr,
    stats: &Arc<TrafficStats>,
    mut migration_rx: tokio::sync::watch::Receiver<u64>,
) -> Result<()>
where
    M: MessageRead,
    N: MessageWrite,
{
    let (mut s5_read, mut s5_write) = tokio::io::split(socks5);

    let target_clone = target.clone();
    let to_tunnel = async {
        let mut buf = BytesMut::with_capacity(MAX_FRAME_PAYLOAD);
        let mut total_up: u64 = 0;
        loop {
            buf.clear();
            let n = s5_read.read_buf(&mut buf).await.map_err(PhantomError::Io)?;
            if n == 0 {
                break;
            }
            total_up += n as u64;
            stats.record_tcp_up(n as u64);
            let data = buf.split().freeze();
            frame_writer
                .write_frame(&Frame::data(stream_id, data))
                .await?;
        }
        let _ = frame_writer.write_frame(&Frame::fin(stream_id)).await;
        let _ = frame_writer.flush().await;
        tracing::info!("Relay done ↑ {} ({} bytes up)", target_clone, total_up);
        Ok::<_, PhantomError>(())
    };

    let target_clone = target.clone();
    let from_tunnel = async {
        let mut total_down: u64 = 0;
        loop {
            let frame = frame_reader.read_frame().await?;
            if frame.flags.contains(FrameFlags::DATA) {
                total_down += frame.payload.len() as u64;
                stats.record_tcp_down(frame.payload.len() as u64);
                s5_write
                    .write_all(&frame.payload)
                    .await
                    .map_err(PhantomError::Io)?;
            } else if frame.flags.contains(FrameFlags::FIN) || frame.flags.contains(FrameFlags::RST)
            {
                break;
            }
        }
        let _ = s5_write.shutdown().await;
        tracing::info!("Relay done ↓ {} ({} bytes down)", target_clone, total_down);
        Ok::<_, PhantomError>(())
    };

    let relay = async {
        tokio::try_join!(to_tunnel, from_tunnel)?;
        Ok::<(), PhantomError>(())
    };

    // With `failover.graceful_migration = false`, an active-server switch
    // bumps the migration epoch and in-flight tunnels must drop instead of
    // draining on the old server. Under the default graceful policy the
    // epoch never moves and this branch is inert.
    tokio::select! {
        res = relay => res?,
        _ = migration_rx.changed() => {
            tracing::info!("Tunnel → {} cut over: server migration", target);
            return Err(PhantomError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionAborted,
                "server migration",
            )));
        }
    }
    Ok(())
}
