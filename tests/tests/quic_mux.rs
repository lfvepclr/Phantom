//! QUIC multiplexing acceptance test.
//!
//! Verifies the two properties the Noise-over-QUIC rework promised:
//!
//! 1. **One handshake, many tunnels.** N concurrent SOCKS5 connections must
//!    share a single pooled QUIC connection — the server-side accept counter
//!    (one increment per completed Noise handshake) must stay at 1.
//! 2. **No stream crosstalk.** Every tunnel exchanges a unique deterministic
//!    payload with the echo server; any frame landing on the wrong QUIC
//!    stream corrupts another tunnel's byte pattern and fails the test.

use phantom_client::failover::FailoverManager;
use phantom_client::quic_pool::QuicPool;
use phantom_client::socks5::handle_socks5_connection;
use phantom_core::crypto::{KeyPair, Psk};
use phantom_core::transport::quic::{QuicAuth, create_server_endpoint};
use phantom_core::{CipherPreference, ClientConfig, QuicConfig, ServerEntry, TransportProtocol};
use phantom_e2e::echo::{EchoMode, start_echo_server};
use phantom_e2e::socks5::Socks5Client;
use phantom_server::handler::handle_quic_connection;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Deterministic per-task payload: any byte leaking across streams flips the
/// pattern and is caught by the exact comparison.
fn task_payload(task_id: usize, len: usize) -> Vec<u8> {
    let mut payload = vec![0u8; len];
    for (i, byte) in payload.iter_mut().enumerate() {
        *byte = (task_id as u32 ^ (i as u32).wrapping_mul(0x9e37)) as u8;
    }
    payload
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn quic_tunnels_share_one_connection_without_crosstalk() {
    const N: usize = 8;
    const PAYLOAD_LEN: usize = 32 * 1024;

    let _ = tracing_subscriber::fmt::try_init();

    // --- Target ---
    let echo = start_echo_server(EchoMode::Echo).await;
    let echo_addr = echo.addr;

    // --- Phantom server (Noise-over-QUIC) ---
    let server_key = KeyPair::generate().expect("server key");
    let psk = Psk::generate();
    let client_key = KeyPair::generate().expect("client key");

    let server_auth = QuicAuth::server(server_key.secret, psk.clone(), CipherPreference::Auto);
    let server_endpoint = create_server_endpoint(
        &"127.0.0.1:0".parse().unwrap(),
        &server_auth,
        &QuicConfig::default(),
    )
    .expect("QUIC server endpoint");
    let server_addr = server_endpoint.local_addr().expect("server addr");

    // One increment per completed Noise handshake (i.e. per QUIC connection).
    let handshake_count = Arc::new(AtomicUsize::new(0));
    {
        let handshake_count = handshake_count.clone();
        let allowed = vec![client_key.public];
        tokio::spawn(async move {
            while let Some(incoming) = server_endpoint.accept().await {
                match incoming.await {
                    Ok(conn) => {
                        handshake_count.fetch_add(1, Ordering::SeqCst);
                        let allowed = allowed.clone();
                        tokio::spawn(async move {
                            handle_quic_connection(conn, &allowed, None).await;
                        });
                    }
                    Err(e) => {
                        tracing::debug!("QUIC handshake failed: {}", e);
                    }
                }
            }
        });
    }

    // --- Phantom client (SOCKS5 front, pooled QUIC back) ---
    let config = Arc::new(ClientConfig {
        servers: vec![ServerEntry {
            name: "quic-test".to_string(),
            address: server_addr.to_string(),
            public_key: server_key.public_key_base64(),
            psk: psk.to_base64(),
            cipher: CipherPreference::Auto,
            protocol: TransportProtocol::Quic,
        }],
        ..Default::default()
    });
    let failover = Arc::new(FailoverManager::new(&config).expect("failover manager"));
    let pool = Arc::new(QuicPool::new());
    let stats = phantom_client::TrafficStats::new();
    let local_secret = client_key.secret;

    let socks5_listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind socks5 listener");
    let socks5_addr = socks5_listener.local_addr().unwrap();
    tokio::spawn(async move {
        loop {
            let (stream, _) = match socks5_listener.accept().await {
                Ok(s) => s,
                Err(_) => break,
            };
            let (config, failover, pool) = (config.clone(), failover.clone(), pool.clone());
            let stats = stats.clone();
            tokio::spawn(async move {
                let _ = handle_socks5_connection(
                    stream,
                    &config,
                    &failover,
                    &pool,
                    local_secret,
                    &stats,
                )
                .await;
            });
        }
    });

    // --- N concurrent SOCKS5 tunnels over what must be one QUIC connection ---
    let target_ip = match echo_addr.ip() {
        std::net::IpAddr::V4(v4) => v4.octets(),
        _ => panic!("echo server must be IPv4 for Socks5Client::connect_ipv4"),
    };
    let target_port = echo_addr.port();

    let mut handles = Vec::with_capacity(N);
    for task_id in 0..N {
        handles.push(tokio::spawn(async move {
            let mut stream = Socks5Client::connect_ipv4(socks5_addr, target_ip, target_port)
                .await
                .expect("socks5 connect");

            let payload = task_payload(task_id, PAYLOAD_LEN);
            stream.write_all(&payload).await.expect("write payload");

            let mut echoed = vec![0u8; payload.len()];
            stream.read_exact(&mut echoed).await.expect("read echo");

            assert_eq!(
                echoed, payload,
                "task {} received bytes belonging to another stream",
                task_id
            );
        }));
    }
    for handle in handles {
        handle.await.expect("tunnel task panicked");
    }

    // Every tunnel succeeded, and the server completed exactly one Noise
    // handshake: the pool multiplexed all N SOCKS5 connections over a single
    // QUIC connection.
    assert_eq!(
        handshake_count.load(Ordering::SeqCst),
        1,
        "{} concurrent SOCKS5 tunnels must share exactly one QUIC connection",
        N
    );
}

/// A client presenting the wrong PSK must not complete a single handshake:
/// the prologue binds the PSK before the first message is sealed, so the
/// server's accept loop never sees a connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn quic_wrong_psk_never_completes_handshake() {
    let _ = tracing_subscriber::fmt::try_init();

    let server_key = KeyPair::generate().expect("server key");
    let server_psk = Psk::generate();
    let wrong_psk = Psk::generate();

    let server_auth = QuicAuth::server(server_key.secret, server_psk, CipherPreference::Auto);
    let server_endpoint = create_server_endpoint(
        &"127.0.0.1:0".parse().unwrap(),
        &server_auth,
        &QuicConfig::default(),
    )
    .expect("QUIC server endpoint");
    let server_addr = server_endpoint.local_addr().expect("server addr");

    let handshake_count = Arc::new(AtomicUsize::new(0));
    {
        let handshake_count = handshake_count.clone();
        tokio::spawn(async move {
            while let Some(incoming) = server_endpoint.accept().await {
                if incoming.await.is_ok() {
                    handshake_count.fetch_add(1, Ordering::SeqCst);
                }
            }
        });
    }

    let client_key = KeyPair::generate().expect("client key");
    let mut server = ServerEntry {
        name: "quic-wrong-psk".to_string(),
        address: server_addr.to_string(),
        public_key: server_key.public_key_base64(),
        psk: wrong_psk.to_base64(),
        cipher: CipherPreference::Auto,
        protocol: TransportProtocol::Quic,
    };
    server.address = server_addr.to_string();

    let result = phantom_client::quic_pool::connect_once(
        &server,
        &client_key.secret,
        CipherPreference::Auto,
        std::time::Duration::from_secs(5),
    )
    .await;
    assert!(result.is_err(), "wrong PSK must fail the Noise handshake");

    // Give the server a moment in case a connection slipped through.
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    assert_eq!(
        handshake_count.load(Ordering::SeqCst),
        0,
        "wrong PSK must be rejected before any connection is established"
    );
}
