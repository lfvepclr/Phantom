//! SOCKS5 UDP ASSOCIATE acceptance tests.
//!
//! Covers the full client-side chain: UDP ASSOCIATE handshake → datagram
//! relay through the Phantom tunnel (TCP and QUIC variants) → per-target
//! flow table → RFC 1928 policy drops (FRAG != 0, foreign source address).

use phantom_client::failover::FailoverManager;
use phantom_client::quic_pool::QuicPool;
use phantom_client::socks5::handle_socks5_connection;
use phantom_client::stats::TrafficStats;
use phantom_core::crypto::{KeyPair, Psk};
use phantom_core::transport::TransportListener;
use phantom_core::transport::quic::{QuicAuth, create_server_endpoint};
use phantom_core::transport::tcp::TcpListener;
use phantom_core::{CipherPreference, ClientConfig, QuicConfig, ServerEntry, TransportProtocol};
use phantom_e2e::udp_echo::UdpEchoServer;
use phantom_server::handler::{handle_connection, handle_quic_connection};
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

/// Start a Phantom server on the TCP transport.
async fn start_tcp_server(cipher: CipherPreference) -> (SocketAddr, KeyPair, Psk) {
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
                        handle_connection(stream, secret, psk, &[], cipher, None).await;
                    });
                }
                Err(_) => break,
            }
        }
    });
    (addr, server_key, psk)
}

/// Start a Phantom server on the QUIC transport (Noise-over-QUIC).
async fn start_quic_server() -> (SocketAddr, KeyPair, Psk) {
    let server_key = KeyPair::generate().expect("server key");
    let psk = Psk::generate();
    let auth = QuicAuth::server(server_key.secret, psk.clone(), CipherPreference::Auto);
    let endpoint = create_server_endpoint(
        &"127.0.0.1:0".parse().unwrap(),
        &auth,
        &QuicConfig::default(),
    )
    .expect("QUIC server endpoint");
    let addr = endpoint.local_addr().expect("server addr");
    tokio::spawn(async move {
        while let Some(incoming) = endpoint.accept().await {
            if let Ok(conn) = incoming.await {
                tokio::spawn(async move {
                    handle_quic_connection(conn, &[], None).await;
                });
            }
        }
    });
    (addr, server_key, psk)
}

/// Start the client-side SOCKS5 ingress wired to the given server.
async fn start_socks5_front(
    server_addr: SocketAddr,
    server_key: &KeyPair,
    psk: &Psk,
    protocol: TransportProtocol,
    client_secret: [u8; 32],
) -> (SocketAddr, Arc<TrafficStats>) {
    let config = Arc::new(ClientConfig {
        servers: vec![ServerEntry {
            name: "udp-test".to_string(),
            address: server_addr.to_string(),
            public_key: server_key.public_key_base64(),
            psk: psk.to_base64(),
            cipher: CipherPreference::Auto,
            protocol,
        }],
        ..Default::default()
    });
    let failover = Arc::new(FailoverManager::new(&config).expect("failover manager"));
    let pool = Arc::new(QuicPool::new());
    let stats = TrafficStats::new();

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind socks5 listener");
    let addr = listener.local_addr().unwrap();
    let stats_ret = stats.clone();
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
                    handle_socks5_connection(stream, &config, &failover, &pool, client_secret, &stats)
                        .await;
            });
        }
    });
    (addr, stats_ret)
}

/// Complete SOCKS5 negotiation + UDP ASSOCIATE. Returns the control
/// connection (dropping it ends the association), the client UDP socket,
/// and the relay address from BND.ADDR/BND.PORT.
async fn udp_associate(socks5_addr: SocketAddr) -> (TcpStream, UdpSocket, SocketAddr) {
    let mut tcp = TcpStream::connect(socks5_addr).await.expect("connect socks5");
    tcp.write_all(&[0x05, 0x01, 0x00]).await.expect("greeting");
    let mut buf = [0u8; 2];
    tcp.read_exact(&mut buf).await.expect("greeting reply");
    assert_eq!(buf, [0x05, 0x00], "method negotiation failed");

    // UDP ASSOCIATE with DST.ADDR/DST.PORT = 0.0.0.0:0 (client does not know).
    tcp.write_all(&[0x05, 0x03, 0x00, 0x01, 0, 0, 0, 0, 0, 0])
        .await
        .expect("udp associate request");
    let mut reply = [0u8; 10];
    tcp.read_exact(&mut reply).await.expect("associate reply");
    assert_eq!(reply[0], 0x05, "bad reply version");
    assert_eq!(reply[1], 0x00, "associate rejected: rep={:#x}", reply[1]);
    assert_eq!(reply[3], 0x01, "expected IPv4 BND.ADDR");
    let relay_addr = SocketAddr::from(([reply[4], reply[5], reply[6], reply[7]], u16::from_be_bytes([reply[8], reply[9]])));
    assert_ne!(relay_addr.port(), 0, "relay port must be bound");

    let udp = UdpSocket::bind("127.0.0.1:0").await.expect("client udp socket");
    (tcp, udp, relay_addr)
}

/// Build a SOCKS5 UDP request datagram (IPv4 target only).
fn udp_request(target: SocketAddr, data: &[u8]) -> Vec<u8> {
    let ip = match target.ip() {
        std::net::IpAddr::V4(v4) => v4.octets(),
        _ => panic!("IPv4 only in tests"),
    };
    let mut pkt = Vec::with_capacity(10 + data.len());
    pkt.extend_from_slice(&[0x00, 0x00, 0x00, 0x01]);
    pkt.extend_from_slice(&ip);
    pkt.extend_from_slice(&target.port().to_be_bytes());
    pkt.extend_from_slice(data);
    pkt
}

/// Receive one datagram and parse the SOCKS5 UDP header: (source, payload).
async fn recv_udp(udp: &UdpSocket) -> (SocketAddr, Vec<u8>) {
    let mut buf = vec![0u8; 65536];
    let (n, _) = udp.recv_from(&mut buf).await.expect("recv udp");
    assert!(n >= 10, "udp datagram too short: {}", n);
    assert_eq!(&buf[..3], &[0x00, 0x00, 0x00], "RSV/FRAG must be zero");
    assert_eq!(buf[3], 0x01, "expected IPv4 source");
    let src = SocketAddr::from((
        [buf[4], buf[5], buf[6], buf[7]],
        u16::from_be_bytes([buf[8], buf[9]]),
    ));
    (src, buf[10..n].to_vec())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_associate_echo_over_tcp() {
    let _ = tracing_subscriber::fmt::try_init();

    let echo = UdpEchoServer::start().await;
    let (server_addr, server_key, psk) = start_tcp_server(CipherPreference::Aes256Gcm).await;
    let client_key = KeyPair::generate().expect("client key");
    let (socks5_addr, stats) = start_socks5_front(
        server_addr,
        &server_key,
        &psk,
        TransportProtocol::Tcp,
        client_key.secret,
    )
    .await;

    let (_control, udp, relay) = udp_associate(socks5_addr).await;

    // Two datagrams to the same target: the second must reuse the flow.
    for payload in [b"hello udp associate".as_slice(), b"second datagram".as_slice()] {
        udp.send_to(&udp_request(echo.addr, payload), relay)
            .await
            .expect("send datagram");
        let (src, echoed) = tokio::time::timeout(Duration::from_secs(3), recv_udp(&udp))
            .await
            .expect("echo timed out");
        assert_eq!(src, echo.addr, "echo must come from the target");
        assert_eq!(echoed, payload, "echo payload mismatch");
    }

    assert!(
        stats.udp_datagrams_up.load(Ordering::Relaxed) >= 2,
        "client must count upstream datagrams"
    );
    assert!(
        stats.udp_datagrams_down.load(Ordering::Relaxed) >= 2,
        "client must count downstream datagrams"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_associate_echo_over_quic() {
    let _ = tracing_subscriber::fmt::try_init();

    let echo = UdpEchoServer::start().await;
    let (server_addr, server_key, psk) = start_quic_server().await;
    let client_key = KeyPair::generate().expect("client key");
    let (socks5_addr, _stats) = start_socks5_front(
        server_addr,
        &server_key,
        &psk,
        TransportProtocol::Quic,
        client_key.secret,
    )
    .await;

    let (_control, udp, relay) = udp_associate(socks5_addr).await;

    let payload = b"udp over quic mux";
    udp.send_to(&udp_request(echo.addr, payload), relay)
        .await
        .expect("send datagram");
    let (src, echoed) = tokio::time::timeout(Duration::from_secs(3), recv_udp(&udp))
        .await
        .expect("echo timed out");
    assert_eq!(src, echo.addr);
    assert_eq!(echoed, payload);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn udp_associate_multi_target_and_policy_drops() {
    let _ = tracing_subscriber::fmt::try_init();

    let echo1 = UdpEchoServer::start().await;
    let echo2 = UdpEchoServer::start().await;
    let (server_addr, server_key, psk) = start_tcp_server(CipherPreference::ChaCha20Poly1305).await;
    let client_key = KeyPair::generate().expect("client key");
    let (socks5_addr, _stats) = start_socks5_front(
        server_addr,
        &server_key,
        &psk,
        TransportProtocol::Tcp,
        client_key.secret,
    )
    .await;

    let (_control, udp, relay) = udp_associate(socks5_addr).await;

    // Two different targets: each gets its own tunnel flow.
    for (target, payload) in [(echo1.addr, b"to echo one"), (echo2.addr, b"to echo two")] {
        udp.send_to(&udp_request(target, payload), relay)
            .await
            .expect("send datagram");
        let (src, echoed) = tokio::time::timeout(Duration::from_secs(3), recv_udp(&udp))
            .await
            .expect("echo timed out");
        assert_eq!(src, target, "response must carry the originating target");
        assert_eq!(echoed, payload);
    }

    // FRAG != 0 must be dropped silently (RFC 1928 §7: no fragmentation).
    let mut frag = udp_request(echo1.addr, b"fragmented");
    frag[2] = 0x01;
    udp.send_to(&frag, relay).await.expect("send frag");
    let mut buf = [0u8; 256];
    assert!(
        tokio::time::timeout(Duration::from_millis(300), udp.recv_from(&mut buf))
            .await
            .is_err(),
        "fragmented datagram must be dropped"
    );

    // A datagram from a foreign source address must be ignored: the
    // association is bound to the first-seen client address.
    let stranger = UdpSocket::bind("127.0.0.1:0").await.expect("stranger socket");
    stranger
        .send_to(&udp_request(echo1.addr, b"intruder"), relay)
        .await
        .expect("send stranger");
    let mut sbuf = [0u8; 256];
    assert!(
        tokio::time::timeout(Duration::from_millis(300), stranger.recv_from(&mut sbuf))
            .await
            .is_err(),
        "foreign source must be ignored"
    );

    // The association itself is still alive for the bound client.
    let payload = b"still alive";
    udp.send_to(&udp_request(echo1.addr, payload), relay)
        .await
        .expect("send final");
    let (src, echoed) = tokio::time::timeout(Duration::from_secs(3), recv_udp(&udp))
        .await
        .expect("final echo timed out");
    assert_eq!(src, echo1.addr);
    assert_eq!(echoed, payload);
}
