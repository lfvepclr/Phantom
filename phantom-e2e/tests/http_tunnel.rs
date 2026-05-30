use phantom_core::CipherPreference;
use phantom_e2e::echo::start_http_echo_server;
use phantom_e2e::socks5::connect_tunnel;
use phantom_e2e::throughput::echo_data;
use phantom_protocol::TargetAddr;
use phantom_transport::TransportListener;

async fn setup_http_tunnel(fixture: &HttpTestFixture) -> anyhow::Result<(
    phantom_protocol::FrameReader<tokio::io::ReadHalf<tokio::net::TcpStream>>,
    phantom_protocol::FrameWriter<tokio::io::WriteHalf<tokio::net::TcpStream>>,
    u32,
)> {
    let ip_bytes = match fixture.http_addr.ip() {
        std::net::IpAddr::V4(ip) => ip.octets(),
        std::net::IpAddr::V6(_) => [127, 0, 0, 1],
    };
    let target = TargetAddr::IPv4(ip_bytes, fixture.http_addr.port());
    connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target,
        fixture.cipher_preference,
    )
    .await
}

struct HttpTestFixture {
    pub http_addr: std::net::SocketAddr,
    pub server_addr: std::net::SocketAddr,
    pub client_key: phantom_crypto::KeyPair,
    pub server_key: phantom_crypto::KeyPair,
    pub cipher_preference: CipherPreference,
    _http_server: phantom_e2e::echo::HttpEchoServer,
    _server_shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl HttpTestFixture {
    pub async fn new(cipher: CipherPreference) -> Self {
        let server_key = phantom_crypto::KeyPair::generate().expect("Failed to generate server key");
        let client_key = phantom_crypto::KeyPair::generate().expect("Failed to generate client key");

        let http_server = start_http_echo_server().await;
        let http_addr = http_server.addr;

        let server_listener = phantom_transport::tcp::TcpListener::bind(&"127.0.0.1:0".parse().unwrap())
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
                                    phantom_server::handler::handle_connection(stream, sk, &[], cp).await;
                                });
                            }
                            Err(e) => { tracing::error!("Server accept error: {}", e); }
                        }
                    }
                    _ = &mut server_shutdown_rx => { break; }
                }
            }
        });

        HttpTestFixture {
            http_addr,
            server_addr,
            client_key,
            server_key,
            cipher_preference: cipher,
            _http_server: http_server,
            _server_shutdown: Some(server_shutdown_tx),
        }
    }
}

#[tokio::test]
async fn http_get_ip_through_tunnel() {
    let fixture = HttpTestFixture::new(CipherPreference::Aes256Gcm).await;
    let (mut reader, mut writer, stream_id) = setup_http_tunnel(&fixture).await.unwrap();

    let request = b"GET /ip HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    let data = request.to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;

    let response = String::from_utf8_lossy(&echoed);
    assert!(
        response.contains("127.0.0.1"),
        "Expected IP in HTTP response, got: {}",
        response
    );
}

#[tokio::test]
async fn http_post_echo_through_tunnel() {
    let fixture = HttpTestFixture::new(CipherPreference::Aes256Gcm).await;
    let (mut reader, mut writer, stream_id) = setup_http_tunnel(&fixture).await.unwrap();

    let body = "hello phantom";
    let request = format!(
        "POST /echo HTTP/1.1\r\nHost: localhost\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let data = request.into_bytes();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;

    let response = String::from_utf8_lossy(&echoed);
    assert!(
        response.contains(body),
        "Expected echoed body in HTTP response, got: {}",
        response
    );
}

#[tokio::test]
async fn http_delay_through_tunnel() {
    use bytes::Bytes;
    use phantom_protocol::Frame;
    use phantom_protocol::frame::FrameFlags;

    let fixture = HttpTestFixture::new(CipherPreference::Aes256Gcm).await;
    let (mut reader, mut writer, stream_id) = setup_http_tunnel(&fixture).await.unwrap();

    let request = b"GET /delay/50 HTTP/1.1\r\nHost: localhost\r\nConnection: close\r\n\r\n";
    let data = request.to_vec();
    let start = std::time::Instant::now();

    // Send request without FIN — let the HTTP server close the connection after responding.
    let mut offset = 0;
    while offset < data.len() {
        let end = std::cmp::min(offset + phantom_core::constants::MAX_FRAME_PAYLOAD, data.len());
        let chunk = Bytes::copy_from_slice(&data[offset..end]);
        writer.write_frame(&Frame::data(stream_id, chunk)).await.unwrap();
        offset = end;
    }
    writer.flush().await.unwrap();

    let mut received = Vec::new();
    loop {
        let frame = reader.read_frame().await.unwrap();
        if frame.flags.contains(FrameFlags::DATA) {
            received.extend_from_slice(&frame.payload);
        } else if frame.flags.contains(FrameFlags::FIN) || frame.flags.contains(FrameFlags::RST) {
            break;
        }
    }
    let elapsed = start.elapsed();

    let response = String::from_utf8_lossy(&received);
    assert!(
        response.contains("ok"),
        "Expected 'ok' in HTTP response, got: {}",
        response
    );
    assert!(
        elapsed >= std::time::Duration::from_millis(50),
        "Expected at least 50ms delay"
    );
}
