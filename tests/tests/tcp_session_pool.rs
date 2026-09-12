//! Behaviour of the client's pre-handshaked TCP session pool.
//!
//! Every tunnelled TCP flow used to pay `connect(server)` + Noise handshake
//! (~2 RTT) before its first byte moved. The pool keeps authenticated sessions
//! ready so the next flow starts relaying immediately — and, crucially, a pooled
//! session that died while it waited must never surface as a failed user
//! connection.

use base64::{Engine, engine::general_purpose::STANDARD};
use phantom_client::socks5::open_tcp_tunnel;
use phantom_client::tcp_pool::TcpSessionPool;
use phantom_core::protocol::TargetAddr;
use phantom_core::{CipherPreference, ServerEntry, TransportProtocol};
use phantom_e2e::fixture::TestFixture;
use phantom_e2e::throughput::echo_data;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::copy_bidirectional;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Mutex;

/// Build the client-side view of the fixture's server.
fn server_entry(addr: SocketAddr, fixture: &TestFixture) -> ServerEntry {
    ServerEntry {
        name: "fixture".to_string(),
        address: addr.to_string(),
        public_key: STANDARD.encode(fixture.server_key.public),
        psk: fixture.psk.to_base64(),
        cipher: fixture.cipher_preference,
        protocol: TransportProtocol::Tcp,
    }
}

fn target_of(fixture: &TestFixture) -> TargetAddr {
    match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => TargetAddr::IPv4(ip.octets(), fixture.target_addr.port()),
        std::net::IpAddr::V6(_) => TargetAddr::IPv4([127, 0, 0, 1], fixture.target_addr.port()),
    }
}

/// Wait until the pool has at least `want` idle sessions (refills are spawned
/// with a small delay, so the test cannot just sleep a fixed amount).
async fn wait_for_idle(pool: &Arc<TcpSessionPool>, want: usize) -> bool {
    for _ in 0..100 {
        if pool.idle_count().await >= want {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    false
}

/// The second flow must be served by a session that was handshaked while the
/// first one was running — that is the whole point of the pool.
#[tokio::test]
async fn second_flow_reuses_a_pooled_session() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let server = server_entry(fixture.server_addr, &fixture);
    let target = target_of(&fixture);
    let pool = Arc::new(TcpSessionPool::new());

    pool.spawn_refill(
        server.clone(),
        fixture.client_key.secret,
        fixture.cipher_preference,
    );
    assert!(
        wait_for_idle(&pool, 1).await,
        "the pool never produced a ready session"
    );

    let (mut reader, mut writer, stream_id) = open_tcp_tunnel(
        &pool,
        &server,
        &fixture.client_key.secret,
        &target,
        fixture.cipher_preference,
    )
    .await
    .expect("pooled tunnel");

    assert_eq!(
        pool.stats().reused,
        1,
        "the flow should have been served by a pooled session"
    );

    // The session is real: data must survive the round trip through it.
    let data = b"pooled session carries data".to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "pooled session corrupted the stream");
}

/// Control handle for the shim in front of the server.
#[derive(Default)]
struct Shim {
    /// Live accepted connections, so the test can cut the idle ones.
    live: Mutex<Vec<tokio::task::AbortHandle>>,
}

impl Shim {
    /// Close every accepted connection that is still being forwarded.
    async fn cut_all(&self) {
        let live = self.live.lock().await;
        for handle in live.iter() {
            handle.abort();
        }
    }
}

/// A TCP forwarder that can cut its connections on demand. It stands in for the
/// network dropping an idle session (NAT rebind, server restart) while the pool
/// still believes the session is good.
async fn shim(upstream: SocketAddr) -> (SocketAddr, Arc<Shim>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.expect("bind shim");
    let addr = listener.local_addr().expect("shim addr");
    let shim = Arc::new(Shim::default());
    let state = Arc::clone(&shim);
    tokio::spawn(async move {
        loop {
            let Ok((down, _)) = listener.accept().await else {
                return;
            };
            let handle = tokio::spawn(async move {
                let Ok(mut up) = TcpStream::connect(upstream).await else {
                    return;
                };
                let mut down = down;
                let _ = copy_bidirectional(&mut down, &mut up).await;
            });
            state.live.lock().await.push(handle.abort_handle());
        }
    });
    (addr, shim)
}

/// A pooled session that died while it waited must cost the flow an extra
/// handshake — never an error.
#[tokio::test]
async fn dead_pooled_session_falls_back_to_a_fresh_connection() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let (shim_addr, shim) = shim(fixture.server_addr).await;
    let server = server_entry(shim_addr, &fixture);
    let target = target_of(&fixture);
    let pool = Arc::new(TcpSessionPool::new());

    pool.spawn_refill(
        server.clone(),
        fixture.client_key.secret,
        fixture.cipher_preference,
    );
    assert!(
        wait_for_idle(&pool, 1).await,
        "the pool never produced a ready session"
    );

    // Kill the session(s) the pool is holding: the handshake is done, the
    // connection looks healthy, and only using it can reveal that it is gone.
    shim.cut_all().await;
    tokio::time::sleep(Duration::from_millis(50)).await;

    let (mut reader, mut writer, stream_id) = open_tcp_tunnel(
        &pool,
        &server,
        &fixture.client_key.secret,
        &target,
        fixture.cipher_preference,
    )
    .await
    .expect("flow must recover from a dead pooled session");

    let data = b"recovered over a fresh session".to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(
        echoed, data,
        "the fallback connection did not carry the stream"
    );
}
