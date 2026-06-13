use bytes::Bytes;
use phantom_core::CipherPreference;
use phantom_e2e::udp_echo::UdpEchoServer;
use phantom_core::protocol::TargetAddr;
use phantom_core::protocol::Frame;
use phantom_core::protocol::frame::FrameFlags;
use phantom_core::crypto::KeyPair;
use phantom_server::handler::handle_connection;
use phantom_core::transport::tcp::TcpListener;
use phantom_core::transport::TransportListener;
use phantom_core::transport::Transport;

struct UdpTestFixture {
    pub udp_echo_addr: std::net::SocketAddr,
    pub server_addr: std::net::SocketAddr,
    pub client_key: KeyPair,
    pub server_key: KeyPair,
    pub cipher_preference: CipherPreference,
    _udp_echo: UdpEchoServer,
    _server_shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl UdpTestFixture {
    pub async fn new(cipher: CipherPreference) -> Self {
        let server_key = KeyPair::generate().expect("Failed to generate server key");
        let client_key = KeyPair::generate().expect("Failed to generate client key");

        let udp_echo = UdpEchoServer::start().await;
        let udp_echo_addr = udp_echo.addr;

        let server_listener = TcpListener::bind(&"127.0.0.1:0".parse().unwrap())
            .await
            .expect("Failed to bind phantom server");
        let server_addr = server_listener.local_addr().unwrap();
        let (server_shutdown_tx, server_shutdown_rx) = tokio::sync::oneshot::channel::<()>();
        let server_secret = server_key.secret;

        tokio::spawn(async move {
            tokio::pin!(server_shutdown_rx);
            loop {
                tokio::select! {
                    accept_result = server_listener.accept() => {
                        match accept_result {
                            Ok((stream, _peer)) => {
                                let sk = server_secret;
                                let cp = cipher;
                                tokio::spawn(async move {
                                    handle_connection(stream, sk, &[], cp).await;
                                });
                            }
                            Err(e) => { tracing::error!("Server accept error: {}", e); }
                        }
                    }
                    _ = &mut server_shutdown_rx => { break; }
                }
            }
        });

        UdpTestFixture {
            udp_echo_addr,
            server_addr,
            client_key,
            server_key,
            cipher_preference: cipher,
            _udp_echo: udp_echo,
            _server_shutdown: Some(server_shutdown_tx),
        }
    }
}

/// Perform the noise handshake and return a frame reader/writer pair,
/// but do NOT send a SYN yet — the caller controls what SYN to send.
async fn handshake_only(
    server_addr: std::net::SocketAddr,
    server_public_key: &[u8; 32],
    client_secret: &[u8; 32],
    cipher_preference: CipherPreference,
) -> anyhow::Result<(
    phantom_core::protocol::FrameReader<tokio::io::ReadHalf<tokio::net::TcpStream>>,
    phantom_core::protocol::FrameWriter<tokio::io::WriteHalf<tokio::net::TcpStream>>,
)> {
    use phantom_core::crypto::{NoiseInitiator, split_after_handshake};
    use phantom_core::crypto::session::CipherOffer;
    use phantom_core::crypto::cipher::CipherSuite;
    use phantom_core::protocol::codec::{FrameReader, FrameWriter};

    let transport = phantom_core::transport::tcp::TcpTransport::new(std::time::Duration::from_secs(10));
    let stream = transport.connect(&server_addr).await?;
    let offer = match cipher_preference {
        CipherPreference::Auto => CipherOffer::default_offer(),
        CipherPreference::Aes256Gcm => CipherOffer::new(vec![CipherSuite::Aes256Gcm]),
        CipherPreference::Aes128Gcm => CipherOffer::new(vec![CipherSuite::Aes128Gcm]),
        CipherPreference::Ascon128 => CipherOffer::new(vec![CipherSuite::Ascon128]),
        CipherPreference::ChaCha20Poly1305 => CipherOffer::new(vec![CipherSuite::ChaCha20Poly]),
    };
    let initiator = NoiseInitiator::new(client_secret, server_public_key);
    let result = initiator.handshake(stream, &offer).await?;
    let (session_reader, session_writer) = split_after_handshake(
        result.stream, result.split_keys, result.chosen_cipher, result.is_initiator,
    );
    Ok((FrameReader::new(session_reader), FrameWriter::new(session_writer)))
}

/// Use handshake to establish a session, then send a UDP SYN frame to the
/// UDP echo server's address, verify response comes back as UDP|DATA frame.
#[tokio::test]
async fn udp_relay_echo() {
    let fixture = UdpTestFixture::new(CipherPreference::Aes256Gcm).await;

    let (mut reader, mut writer) = handshake_only(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        fixture.cipher_preference,
    )
    .await
    .unwrap();

    // Build the UDP SYN payload: [TargetAddr encoded][datagram bytes]
    let ip_bytes = match fixture.udp_echo_addr.ip() {
        std::net::IpAddr::V4(ip) => ip.octets(),
        std::net::IpAddr::V6(_) => [127, 0, 0, 1],
    };
    let target = TargetAddr::IPv4(ip_bytes, fixture.udp_echo_addr.port());
    let datagram = b"hello udp relay";
    let mut syn_payload = target.encode().to_vec();
    syn_payload.extend_from_slice(datagram);

    let stream_id: u32 = 1;
    let udp_syn = Frame {
        version: phantom_core::constants::PROTOCOL_VERSION,
        stream_id,
        flags: FrameFlags::SYN | FrameFlags::UDP | FrameFlags::DATA,
        payload: Bytes::from(syn_payload),
    };
    writer.write_frame(&udp_syn).await.unwrap();
    writer.flush().await.unwrap();

    // Read the ACK first
    let ack = reader.read_frame().await.unwrap();
    assert!(
        ack.flags.contains(FrameFlags::ACK),
        "Expected ACK for UDP SYN, got flags: {:?}",
        ack.flags,
    );

    // Now read the echoed UDP response — should come back as UDP|DATA
    let response = reader.read_frame().await.unwrap();
    assert!(
        response.flags.contains(FrameFlags::UDP),
        "Expected UDP flag in response, got flags: {:?}",
        response.flags,
    );
    assert!(
        response.flags.contains(FrameFlags::DATA),
        "Expected DATA flag in response, got flags: {:?}",
        response.flags,
    );
    assert_eq!(
        response.payload.as_ref(),
        datagram,
        "UDP echo payload mismatch",
    );
}
