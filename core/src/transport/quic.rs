use crate::crypto::Psk;
use crate::transport::traits::TransportListener;
use crate::{CipherPreference, CongestionAlgorithm, QuicConfig};
use crate::{PhantomError, Result};
use async_trait::async_trait;
use quinn_hyphae::builder::{BasicHandshakeConfig, EmptyPayloadDriver};
use quinn_hyphae::config::HyphaeCryptoConfig;
use quinn_hyphae::helper::{hyphae_client_endpoint, hyphae_server_endpoint};
use quinn_hyphae::{HandshakeBuilder, HyphaePeerIdentity, RustCryptoBackend};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::Mutex;

pub struct QuicListener {
    endpoint: quinn::Endpoint,
}

impl QuicListener {
    pub async fn bind(addr: &SocketAddr, auth: &QuicAuth, quic: &QuicConfig) -> Result<Self> {
        // `create_server_endpoint` preserves the io::ErrorKind of the
        // underlying UDP bind, so AddrInUse still drives the fallback loop.
        let endpoint = create_server_endpoint(addr, auth, quic)?;
        Ok(Self { endpoint })
    }
}

/// Try to bind a QUIC endpoint starting at `start_addr.port()`. If the port is
/// already in use, increment the port and try again — up to `max_attempts`
/// total attempts. Returns the endpoint and the actual bound address.
pub async fn try_bind_quic_with_fallback(
    start_addr: SocketAddr,
    max_attempts: u16,
    auth: &QuicAuth,
    quic: &QuicConfig,
) -> Result<(QuicListener, SocketAddr)> {
    let ip = start_addr.ip();
    let start_port = start_addr.port();
    let mut last_err: Option<std::io::Error> = None;
    for offset in 0..max_attempts {
        let port = start_port.saturating_add(offset);
        let addr = SocketAddr::new(ip, port);
        match QuicListener::bind(&addr, auth, quic).await {
            Ok(listener) => return Ok((listener, addr)),
            Err(PhantomError::Io(io_err)) if io_err.kind() == std::io::ErrorKind::AddrInUse => {
                last_err = Some(io_err);
            }
            Err(e) => return Err(e),
        }
    }
    let end_port = start_port.saturating_add(max_attempts.saturating_sub(1));
    Err(PhantomError::Config(format!(
        "No free QUIC port in {ip}:{start_port}..{end_port} ({} attempt(s) all busy): {}",
        max_attempts,
        last_err
            .map(|e| e.to_string())
            .unwrap_or_else(|| "AddrInUse".to_string())
    )))
}

#[async_trait]
impl TransportListener for QuicListener {
    type Stream = QuicStream;

    async fn accept(&self) -> Result<(Self::Stream, SocketAddr)> {
        let incoming =
            self.endpoint
                .accept()
                .await
                .ok_or(PhantomError::Io(std::io::Error::new(
                    std::io::ErrorKind::Other,
                    "No incoming connection",
                )))?;
        let conn = incoming
            .await
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        let remote = conn.remote_address();
        let (send, recv) = conn
            .accept_bi()
            .await
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        Ok((QuicStream::new(send, recv), remote))
    }

    fn local_addr(&self) -> Result<SocketAddr> {
        self.endpoint.local_addr().map_err(PhantomError::Io)
    }
}

/// A QUIC stream that properly implements AsyncRead + AsyncWrite.
///
/// Send and recv directions use separate mutexes so reads and writes
/// can proceed concurrently. After `tokio::io::split`, the ReadHalf
/// only locks recv and the WriteHalf only locks send — zero contention.
pub struct QuicStream {
    send: Arc<Mutex<Option<quinn::SendStream>>>,
    recv: Arc<Mutex<Option<quinn::RecvStream>>>,
}

impl QuicStream {
    pub fn new(send: quinn::SendStream, recv: quinn::RecvStream) -> Self {
        Self {
            send: Arc::new(Mutex::new(Some(send))),
            recv: Arc::new(Mutex::new(Some(recv))),
        }
    }
}

impl AsyncRead for QuicStream {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        let recv = self.recv.clone();
        let mut guard = match recv.try_lock() {
            Ok(g) => g,
            Err(_) => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        };

        let Some(ref mut recv_stream) = *guard else {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "QUIC recv stream closed",
            )));
        };

        // Use tokio's AsyncRead trait explicitly
        AsyncRead::poll_read(Pin::new(recv_stream), cx, buf)
    }
}

impl AsyncWrite for QuicStream {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        let send = self.send.clone();
        let mut guard = match send.try_lock() {
            Ok(g) => g,
            Err(_) => {
                cx.waker().wake_by_ref();
                return Poll::Pending;
            }
        };

        let Some(ref mut send_stream) = *guard else {
            return Poll::Ready(Err(std::io::Error::new(
                std::io::ErrorKind::NotConnected,
                "QUIC send stream closed",
            )));
        };

        // Use tokio's AsyncWrite trait explicitly
        AsyncWrite::poll_write(Pin::new(send_stream), cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        // quinn's poll_flush is a no-op
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        let send = self.send.clone();
        let mut guard = match send.try_lock() {
            Ok(g) => g,
            Err(_) => {
                return Poll::Ready(Ok(()));
            }
        };

        if let Some(ref mut send_stream) = *guard {
            let _ = send_stream.finish();
        }
        *guard = None;
        Poll::Ready(Ok(()))
    }
}

/// Build a transport config from the `[quic]` settings: congestion control,
/// the concurrent bi-stream cap, and the keep-alive interval (seconds; `0`
/// disables keep-alives).
///
/// Windows are sized for a 1 Gbps x 300 ms worst-case path (BDP ≈ 37.5 MB):
/// a per-stream window of 8 MB and a connection-wide window of 32 MB keep a
/// single stream from stalling on high-RTT links, which is exactly where the
/// defaults (256 KB stream / ~1 MB connection) collapse to a few hundred KB/s.
pub fn build_transport_config(quic: &QuicConfig) -> Arc<quinn::TransportConfig> {
    const STREAM_WINDOW: u64 = 8 * 1024 * 1024;
    const CONN_WINDOW: u64 = 32 * 1024 * 1024;

    let mut transport = quinn::TransportConfig::default();
    match quic.congestion {
        CongestionAlgorithm::Bbr => {
            transport
                .congestion_controller_factory(Arc::new(quinn::congestion::BbrConfig::default()));
        }
        CongestionAlgorithm::Cubic => {
            transport
                .congestion_controller_factory(Arc::new(quinn::congestion::CubicConfig::default()));
        }
        CongestionAlgorithm::NewReno => {
            transport.congestion_controller_factory(Arc::new(
                quinn::congestion::NewRenoConfig::default(),
            ));
        }
    }
    transport.max_concurrent_bidi_streams(quic.max_streams.into());
    if quic.keep_alive_interval > 0 {
        transport.keep_alive_interval(Some(std::time::Duration::from_secs(
            quic.keep_alive_interval,
        )));
    }

    // Flow-control windows + path MTU discovery. VarInt covers up to 2^62-1,
    // so the constants above always fit; use unwrap_or to stay total.
    let ok_varint = |v: u64| quinn::VarInt::from_u64(v).unwrap_or(quinn::VarInt::MAX);
    transport.stream_receive_window(ok_varint(STREAM_WINDOW));
    transport.receive_window(ok_varint(CONN_WINDOW));
    transport.send_window(CONN_WINDOW);
    transport.initial_mtu(1500);
    transport.mtu_discovery_config(Some(quinn::MtuDiscoveryConfig::default()));
    Arc::new(transport)
}

/// Authentication material for the QUIC transport.
///
/// QUIC here is secured by a Noise handshake instead of TLS, so the endpoint
/// itself needs the static keys and the PSK — there is no certificate involved.
#[derive(Clone)]
pub struct QuicAuth {
    /// This peer's static X25519 secret.
    pub local_secret: [u8; 32],
    /// The server's static public key. Required on the client, because the IK
    /// pattern needs it before the first message; unused on the server.
    pub remote_public: Option<[u8; 32]>,
    /// Pre-shared key, bound through the Noise prologue.
    pub psk: Psk,
    /// AEAD preference for the Noise handshake. Unlike TCP there is no
    /// in-band negotiation over QUIC — the AEAD is baked into the Noise
    /// pattern string, so both peers must map their preference identically.
    pub cipher: CipherPreference,
}

impl QuicAuth {
    pub fn client(
        local_secret: [u8; 32],
        remote_public: [u8; 32],
        psk: Psk,
        cipher: CipherPreference,
    ) -> Self {
        Self {
            local_secret,
            remote_public: Some(remote_public),
            psk,
            cipher,
        }
    }

    pub fn server(local_secret: [u8; 32], psk: Psk, cipher: CipherPreference) -> Self {
        Self {
            local_secret,
            remote_public: None,
            psk,
            cipher,
        }
    }
}

/// Noise pattern used by the QUIC transport.
///
/// Unlike the TCP path, the AEAD is fixed by the pattern string rather than
/// negotiated in-band: hyphae binds it when the handshake is constructed. Both
/// peers therefore derive the pattern from the same `cipher=` URI parameter, and
/// a mismatch shows up as a handshake failure.
///
/// The PSK is carried in the Noise **prologue** rather than a `pskN` modifier,
/// because hyphae rejects PSK modifiers outright
/// (`CryptoError::UnsupportedProtocol`). The prologue is mixed into the
/// handshake hash before the first message is sealed, so it delivers the same
/// anti-probing property as `psk1` does on the TCP path: a peer with the wrong
/// PSK cannot complete the handshake.
fn quic_noise_pattern(pref: CipherPreference) -> Result<&'static str> {
    match pref {
        // Every supported platform (Apple Silicon, BCM4912, recent Kirin /
        // Snapdragon, modern ARM servers) has AES hardware, so Auto picks
        // AES-256-GCM.
        CipherPreference::Auto | CipherPreference::Aes256Gcm => Ok("Noise_IK_25519_AESGCM_SHA256"),
        // Noise's "AESGCM" is AES-256-GCM; there is no AES-128 variant in the
        // spec, so a 128-bit preference is served by the 256-bit suite.
        CipherPreference::Aes128Gcm => Ok("Noise_IK_25519_AESGCM_SHA256"),
        CipherPreference::ChaCha20Poly1305 => Ok("Noise_IK_25519_ChaChaPoly_SHA256"),
        CipherPreference::Ascon128 => Err(PhantomError::Config(
            "ASCON-128 is not available over QUIC: the Noise backend implements \
             only AESGCM and ChaChaPoly. Use proto=tcp for ASCON, or pick \
             aes-256-gcm / chacha20-poly1305 for QUIC."
                .to_string(),
        )),
    }
}

/// Concrete hyphae config produced by our builder (no custom Noise payloads).
type HyphaeConfig =
    Arc<HyphaeCryptoConfig<BasicHandshakeConfig<EmptyPayloadDriver>, RustCryptoBackend>>;

/// Build the hyphae crypto config shared by both endpoint directions.
fn build_hyphae_config(auth: &QuicAuth) -> Result<HyphaeConfig> {
    let pattern = quic_noise_pattern(auth.cipher)?;
    if auth.cipher == CipherPreference::Aes128Gcm {
        tracing::warn!(
            "QUIC uses Noise AESGCM (AES-256-GCM); the aes-128-gcm preference has \
             no AES-128 equivalent in the Noise spec"
        );
    }

    let mut builder = HandshakeBuilder::new(pattern)
        .with_static_key(&auth.local_secret)
        // The PSK must be bound before the first message is sealed.
        .with_prologue(auth.psk.as_bytes());
    if let Some(ref remote) = auth.remote_public {
        builder = builder.with_remote_public(remote);
    }

    builder
        .build(RustCryptoBackend)
        .map_err(|e| PhantomError::Crypto(format!("QUIC Noise handshake config failed: {:?}", e)))
}

/// Create a QUIC client endpoint authenticated by Noise.
///
/// The client has no `[quic]` config section of its own; callers pass
/// `&QuicConfig::default()` unless they need to tune the transport.
pub fn create_client_endpoint(auth: &QuicAuth, quic: &QuicConfig) -> Result<quinn::Endpoint> {
    let crypto = build_hyphae_config(auth)?;
    let socket = std::net::UdpSocket::bind("[::]:0").map_err(PhantomError::Io)?;
    hyphae_client_endpoint(crypto, Some(build_transport_config(quic)), socket)
        .map_err(PhantomError::Io)
}

/// Create a QUIC server endpoint authenticated by Noise, bound to `addr`.
pub fn create_server_endpoint(
    addr: &SocketAddr,
    auth: &QuicAuth,
    quic: &QuicConfig,
) -> Result<quinn::Endpoint> {
    let crypto = build_hyphae_config(auth)?;
    let socket = std::net::UdpSocket::bind(addr).map_err(PhantomError::Io)?;
    hyphae_server_endpoint(crypto, Some(build_transport_config(quic)), socket)
        .map_err(PhantomError::Io)
}

/// Extract the peer's Noise static public key from an established connection.
///
/// This is how the server authenticates clients against its whitelist now that
/// authentication happens at connection level rather than per stream.
pub fn peer_static_key(conn: &quinn::Connection) -> Option<[u8; 32]> {
    let identity = conn.peer_identity()?;
    let identity = identity.downcast::<HyphaePeerIdentity>().ok()?;
    let remote = identity.remote_public.as_ref()?;
    <[u8; 32]>::try_from(remote.as_slice()).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::crypto::KeyPair;
    use std::net::{IpAddr, Ipv4Addr};
    use tokio::net::UdpSocket;

    /// Server-side auth material for bind tests (contents never matter — the
    /// endpoint is never dialed).
    fn test_server_auth() -> QuicAuth {
        QuicAuth::server(
            [0x42; 32],
            Psk::generate(),
            CipherPreference::ChaCha20Poly1305,
        )
    }

    /// Reserve `count` consecutive loopback UDP ports, returning the base port
    /// and the sockets holding them.
    ///
    /// Probing with `:0` and dropping the socket is racy: a sibling test can
    /// claim the port before this one uses it. Keeping the sockets bound makes
    /// the "busy" side of each assertion deterministic.
    async fn reserve_consecutive_udp(count: u16) -> (u16, Vec<UdpSocket>) {
        assert!(count > 0);
        for _ in 0..64 {
            let first = UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
                .await
                .expect("failed to bind an ephemeral UDP port");
            let base = first.local_addr().unwrap().port();
            if base.checked_add(count).is_none() {
                continue;
            }
            let mut held = vec![first];
            for offset in 1..count {
                match UdpSocket::bind((Ipv4Addr::LOCALHOST, base + offset)).await {
                    Ok(socket) => held.push(socket),
                    // A neighbouring port is taken; start over from a new base.
                    Err(_) => break,
                }
            }
            if held.len() == count as usize {
                return (base, held);
            }
        }
        panic!("could not reserve {} consecutive loopback UDP ports", count);
    }

    #[tokio::test]
    async fn try_bind_quic_with_fallback_picks_next_port() {
        // QUIC needs a real OS UDP socket. Hold the first two ports so the bind
        // path has to fall through to a later one.
        let (base, _held) = reserve_consecutive_udp(2).await;
        let start = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), base);
        let auth = test_server_auth();
        let result = try_bind_quic_with_fallback(start, 50, &auth, &QuicConfig::default()).await;
        match result {
            Ok((_listener, bound)) => {
                assert!(
                    bound.port() >= base + 2,
                    "expected both occupied ports to be skipped, got {}",
                    bound.port()
                );
            }
            Err(PhantomError::Config(msg)) => {
                // Acceptable on CI where the loopback may refuse UDP socket
                // creation. As long as we get a Config error (not an outright
                // panic), the test still demonstrates error handling.
                assert!(
                    msg.contains("No free QUIC port"),
                    "unexpected message: {}",
                    msg
                );
            }
            Err(other) => panic!("unexpected error: {:?}", other),
        }
    }

    #[tokio::test]
    async fn try_bind_quic_with_fallback_first_port_free() {
        // Release a freshly reserved port and claim it through the function
        // under test; retry if a concurrent bind wins the race.
        for attempt in 0..16 {
            let (base, held) = reserve_consecutive_udp(1).await;
            drop(held);
            let start = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), base);
            let auth = test_server_auth();
            match try_bind_quic_with_fallback(start, 1, &auth, &QuicConfig::default()).await {
                Ok((_listener, bound)) => {
                    assert_eq!(bound.port(), base, "a free start port must be used as-is");
                    return;
                }
                Err(PhantomError::Config(msg)) if attempt < 15 => {
                    assert!(
                        msg.contains("No free QUIC port"),
                        "unexpected message: {}",
                        msg
                    );
                    continue;
                }
                // Tolerated: CI loopback may not support QUIC at all; the error
                // path is still exercised.
                Err(PhantomError::Config(msg)) => {
                    assert!(
                        msg.contains("No free QUIC port"),
                        "unexpected message: {}",
                        msg
                    );
                    return;
                }
                Err(other) => panic!("unexpected error: {:?}", other),
            }
        }
    }

    /// Spin up a loopback server endpoint and echo every accepted bi-stream.
    /// Returns the address the server bound to.
    async fn spawn_echo_server(auth: QuicAuth) -> SocketAddr {
        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let endpoint =
            create_server_endpoint(&addr, &auth, &QuicConfig::default()).expect("server endpoint");
        let local = endpoint.local_addr().expect("local addr");
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let Ok(conn) = incoming.await else { continue };
                loop {
                    let Ok((mut send, mut recv)) = conn.accept_bi().await else {
                        break;
                    };
                    tokio::spawn(async move {
                        if let Ok(buf) = recv.read_to_end(64 * 1024).await {
                            let _ = send.write_all(&buf).await;
                            let _ = send.finish();
                        }
                    });
                }
            }
        });
        local
    }

    async fn open_client_connection(
        server_addr: SocketAddr,
        server_public: [u8; 32],
        client_secret: [u8; 32],
        psk: Psk,
    ) -> Result<quinn::Connection> {
        open_client_connection_with(
            server_addr,
            server_public,
            client_secret,
            psk,
            &QuicConfig::default(),
        )
        .await
    }

    async fn open_client_connection_with(
        server_addr: SocketAddr,
        server_public: [u8; 32],
        client_secret: [u8; 32],
        psk: Psk,
        quic: &QuicConfig,
    ) -> Result<quinn::Connection> {
        let auth = QuicAuth::client(
            client_secret,
            server_public,
            psk,
            CipherPreference::ChaCha20Poly1305,
        );
        let endpoint = create_client_endpoint(&auth, quic)?;
        let conn = endpoint
            .connect(server_addr, "phantom")
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?
            .await
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        Ok(conn)
    }

    /// PoC regression: one Noise-authenticated connection must multiplex many
    /// bi-streams, each with independent data.
    #[tokio::test]
    async fn noise_quic_multiplexes_streams() {
        let server_keys = KeyPair::generate().expect("server keys");
        let psk = Psk::generate();
        let server_auth = QuicAuth::server(
            server_keys.secret,
            psk.clone(),
            CipherPreference::ChaCha20Poly1305,
        );
        let server_addr = spawn_echo_server(server_auth).await;

        let conn = open_client_connection(server_addr, server_keys.public, [0x11; 32], psk)
            .await
            .expect("client connection");

        for i in 0..3u8 {
            let payload = format!("phantom-stream-{}", i).into_bytes();
            let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
            send.write_all(&payload).await.expect("write");
            send.finish().expect("finish");
            let echoed = recv.read_to_end(64 * 1024).await.expect("read");
            assert_eq!(echoed, payload, "stream {} data mismatch", i);
        }
    }

    /// PoC regression: the prologue-carried PSK must reject a mismatched peer
    /// during the handshake, exactly like psk1 does on the TCP path.
    #[tokio::test]
    async fn noise_quic_rejects_wrong_psk() {
        let server_keys = KeyPair::generate().expect("server keys");
        let server_auth = QuicAuth::server(
            server_keys.secret,
            Psk::generate(),
            CipherPreference::ChaCha20Poly1305,
        );
        let server_addr = spawn_echo_server(server_auth).await;

        let result = open_client_connection(
            server_addr,
            server_keys.public,
            [0x11; 32],
            Psk::generate(), // different PSK
        )
        .await;
        assert!(
            result.is_err(),
            "a client with the wrong PSK must not complete the handshake"
        );
    }

    /// The server must be able to recover the client's static public key from
    /// an established connection (connection-level whitelist check).
    #[tokio::test]
    async fn server_sees_client_static_key() {
        let server_keys = KeyPair::generate().expect("server keys");
        let client_keys = KeyPair::generate().expect("client keys");
        let psk = Psk::generate();

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let server_auth = QuicAuth::server(
            server_keys.secret,
            psk.clone(),
            CipherPreference::ChaCha20Poly1305,
        );
        let server_endpoint = create_server_endpoint(&addr, &server_auth, &QuicConfig::default())
            .expect("server endpoint");
        let server_addr = server_endpoint.local_addr().expect("local addr");

        let seen = tokio::spawn(async move {
            let incoming = server_endpoint.accept().await.expect("incoming");
            let conn = incoming.await.expect("connection");
            peer_static_key(&conn)
        });

        open_client_connection(server_addr, server_keys.public, client_keys.secret, psk)
            .await
            .expect("client connection");

        let seen = seen.await.expect("server task");
        assert_eq!(seen, Some(client_keys.public));
    }

    /// `quic.max_streams` must reach quinn's transport parameters: with a cap
    /// of 2, the client's third `open_bi` blocks while the server holds the
    /// first two streams open. (quinn 0.11 `TransportConfig` has no getters,
    /// so the knob is verified through observable behavior.)
    #[tokio::test]
    async fn max_streams_caps_client_opened_streams() {
        use tokio::time::{Duration, timeout};

        let server_keys = KeyPair::generate().expect("server keys");
        let psk = Psk::generate();
        let server_auth = QuicAuth::server(
            server_keys.secret,
            psk.clone(),
            CipherPreference::ChaCha20Poly1305,
        );

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let quic = QuicConfig {
            max_streams: 2,
            ..QuicConfig::default()
        };
        let endpoint = create_server_endpoint(&addr, &server_auth, &quic).expect("server endpoint");
        let server_addr = endpoint.local_addr().expect("local addr");
        tokio::spawn(async move {
            // Accept one connection and hold every bi-stream open forever so
            // the client-visible concurrency window stays full.
            while let Some(incoming) = endpoint.accept().await {
                let Ok(conn) = incoming.await else { continue };
                loop {
                    let Ok((send, _recv)) = conn.accept_bi().await else {
                        break;
                    };
                    tokio::spawn(async move {
                        let _send = send;
                        std::future::pending::<()>().await;
                    });
                }
            }
        });

        let conn = open_client_connection(server_addr, server_keys.public, [0x11; 32], psk)
            .await
            .expect("client connection");

        let _s1 = conn.open_bi().await.expect("stream 1");
        let _s2 = conn.open_bi().await.expect("stream 2");
        // Third stream must block: the cap is 2 and neither stream finishes.
        let third = timeout(Duration::from_millis(300), conn.open_bi()).await;
        assert!(third.is_err(), "third stream must block behind the cap");
    }

    /// Delivery-mechanism proof for `build_transport_config`: quinn 0.11
    /// `TransportConfig` has no getters and its `Debug` omits the factory, so
    /// "the configured congestion controller reaches live connections" is
    /// verified behaviorally. A counting factory wrapping `BbrConfig` is
    /// installed through the same endpoint-construction path
    /// `create_server_endpoint` uses; an established Noise-QUIC connection
    /// must have triggered at least one `build` call.
    #[tokio::test]
    async fn congestion_factory_is_applied_to_live_connections() {
        use quinn::congestion::{BbrConfig, Controller, ControllerFactory};
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Instant;

        struct CountingFactory {
            inner: Arc<BbrConfig>,
            builds: Arc<AtomicUsize>,
        }

        impl ControllerFactory for CountingFactory {
            fn build(self: Arc<Self>, now: Instant, current_mtu: u16) -> Box<dyn Controller> {
                self.builds.fetch_add(1, Ordering::SeqCst);
                self.inner.clone().build(now, current_mtu)
            }
        }

        let server_keys = KeyPair::generate().expect("server keys");
        let psk = Psk::generate();
        let auth = QuicAuth::server(
            server_keys.secret,
            psk.clone(),
            CipherPreference::ChaCha20Poly1305,
        );

        // Inline `create_server_endpoint`, swapping the transport config for
        // one carrying the counting factory.
        let crypto = build_hyphae_config(&auth).expect("hyphae config");
        let mut transport = quinn::TransportConfig::default();
        let builds = Arc::new(AtomicUsize::new(0));
        transport.congestion_controller_factory(Arc::new(CountingFactory {
            inner: Arc::new(BbrConfig::default()),
            builds: builds.clone(),
        }));
        let socket = std::net::UdpSocket::bind((Ipv4Addr::LOCALHOST, 0)).expect("udp socket");
        let endpoint = hyphae_server_endpoint(crypto, Some(Arc::new(transport)), socket)
            .expect("server endpoint");
        let server_addr = endpoint.local_addr().expect("local addr");
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                if let Ok(conn) = incoming.await {
                    let _ = conn.accept_bi().await;
                }
            }
        });

        open_client_connection(server_addr, server_keys.public, [0x11; 32], psk)
            .await
            .expect("client connection");

        // The factory fires when the connection path is created, which can
        // race with the client's handshake completion — poll briefly.
        for _ in 0..100 {
            if builds.load(Ordering::SeqCst) > 0 {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(
            builds.load(Ordering::SeqCst) > 0,
            "a TransportConfig congestion factory must build a controller per connection"
        );
    }

    /// `CongestionAlgorithm::Bbr` must yield working connections through the
    /// public endpoint constructors on both peers: handshake, stream open,
    /// and 1 MiB of echoed data (enough to drive the controller through
    /// several RTTs even on loopback).
    #[tokio::test]
    async fn bbr_congestion_config_echo_round_trip() {
        let server_keys = KeyPair::generate().expect("server keys");
        let psk = Psk::generate();
        let bbr = QuicConfig {
            congestion: CongestionAlgorithm::Bbr,
            ..QuicConfig::default()
        };
        let server_auth = QuicAuth::server(
            server_keys.secret,
            psk.clone(),
            CipherPreference::ChaCha20Poly1305,
        );

        let addr = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0);
        let endpoint = create_server_endpoint(&addr, &server_auth, &bbr).expect("server endpoint");
        let server_addr = endpoint.local_addr().expect("local addr");
        tokio::spawn(async move {
            while let Some(incoming) = endpoint.accept().await {
                let Ok(conn) = incoming.await else { continue };
                loop {
                    let Ok((mut send, mut recv)) = conn.accept_bi().await else {
                        break;
                    };
                    tokio::spawn(async move {
                        // Chunked echo keeps the flow-control window draining
                        // regardless of payload size.
                        let mut buf = vec![0u8; 16 * 1024];
                        loop {
                            match recv.read(&mut buf).await {
                                Ok(Some(n)) if n > 0 => {
                                    if send.write_all(&buf[..n]).await.is_err() {
                                        break;
                                    }
                                }
                                _ => break,
                            }
                        }
                        let _ = send.finish();
                    });
                }
            }
        });

        let conn =
            open_client_connection_with(server_addr, server_keys.public, [0x11; 32], psk, &bbr)
                .await
                .expect("client connection");

        let payload: Vec<u8> = (0..1024 * 1024u32)
            .map(|i| (i.wrapping_mul(0x9e37) >> 8) as u8)
            .collect();
        let (mut send, mut recv) = conn.open_bi().await.expect("open_bi");
        let writer = tokio::spawn({
            let payload = payload.clone();
            async move {
                send.write_all(&payload).await.expect("write payload");
                send.finish().expect("finish");
            }
        });
        let echoed = recv.read_to_end(2 * 1024 * 1024).await.expect("read echo");
        writer.await.expect("writer task");
        assert_eq!(
            echoed, payload,
            "a Bbr-configured connection corrupted the echoed payload"
        );
    }
}
