use phantom_core::CipherPreference;
use phantom_core::protocol::TargetAddr;
use phantom_e2e::mock_web::MockWebServer;
use phantom_e2e::socks5::connect_tunnel;
use phantom_e2e::throughput::echo_data;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Helper: establish a Phantom tunnel to a specific IPv4 target address.
async fn tunnel_to_ipv4(
    fixture: &phantom_e2e::fixture::TestFixture,
    target_addr: std::net::SocketAddr,
) -> anyhow::Result<(
    phantom_core::protocol::FrameReader<tokio::io::ReadHalf<tokio::net::TcpStream>>,
    phantom_core::protocol::FrameWriter<tokio::io::WriteHalf<tokio::net::TcpStream>>,
    u32,
)> {
    let ip_bytes = match target_addr.ip() {
        std::net::IpAddr::V4(ip) => ip.octets(),
        std::net::IpAddr::V6(_) => [127, 0, 0, 1],
    };
    let target = TargetAddr::IPv4(ip_bytes, target_addr.port());
    connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target,
        fixture.cipher_preference,
    )
    .await
}

#[tokio::test]
async fn mock_baidu_through_proxy() {
    // Start a mock baidu web server
    let mock_server = MockWebServer::start().await;

    // Build a Phantom tunnel fixture (the echo server it creates is unused;
    // we will tell the tunnel to connect to the MockWebServer instead)
    let fixture = phantom_e2e::fixture::TestFixture::new(CipherPreference::Aes256Gcm).await;

    // Connect through the Phantom tunnel to the mock baidu server
    let (mut reader, mut writer, stream_id) = tunnel_to_ipv4(&fixture, mock_server.addr)
        .await
        .expect("Failed to establish tunnel to mock baidu");

    // Send an HTTP GET request for the index page
    let request = format!(
        "GET / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        mock_server.addr
    );
    let data = request.into_bytes();
    let response_bytes = echo_data(&mut reader, &mut writer, stream_id, &data).await;

    let response = String::from_utf8_lossy(&response_bytes);
    assert!(
        response.contains("百度一下"),
        "Expected '百度一下' in mock baidu response, got: {}",
        response
    );
}

#[tokio::test]
async fn mock_baidu_health_check() {
    // Start a mock baidu web server
    let mock_server = MockWebServer::start().await;

    // Build a Phantom tunnel fixture
    let fixture = phantom_e2e::fixture::TestFixture::new(CipherPreference::Aes256Gcm).await;

    // Connect through the Phantom tunnel to the mock baidu server
    let (mut reader, mut writer, stream_id) = tunnel_to_ipv4(&fixture, mock_server.addr)
        .await
        .expect("Failed to establish tunnel to mock baidu");

    // Send an HTTP GET request for the /health endpoint
    let request = format!(
        "GET /health HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        mock_server.addr
    );
    let data = request.into_bytes();
    let response_bytes = echo_data(&mut reader, &mut writer, stream_id, &data).await;

    let response = String::from_utf8_lossy(&response_bytes);
    assert!(
        response.contains("ok"),
        "Expected 'ok' in health response, got: {}",
        response
    );
}

#[tokio::test]
#[ignore] // Requires real network access to www.baidu.com
async fn real_baidu_global_mode() {
    // Build a Phantom tunnel fixture — all traffic is routed through
    // the tunnel (equivalent to Global/Proxy mode)
    let fixture = phantom_e2e::fixture::TestFixture::new(CipherPreference::Aes256Gcm).await;

    // Connect to real www.baidu.com through the Phantom tunnel
    let target = TargetAddr::Domain("www.baidu.com".to_string(), 80);
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target,
        fixture.cipher_preference,
    )
    .await
    .expect("Failed to establish tunnel to www.baidu.com");

    // Send an HTTP GET request
    let request = "GET / HTTP/1.1\r\nHost: www.baidu.com\r\nConnection: close\r\n\r\n";
    let data = request.as_bytes().to_vec();
    let response_bytes = echo_data(&mut reader, &mut writer, stream_id, &data).await;

    let response = String::from_utf8_lossy(&response_bytes);
    let status_line = response.lines().next().unwrap_or("");
    assert!(
        status_line.contains("200 OK") || status_line.contains("302"),
        "Expected 200 OK or 302 redirect, got: {}",
        status_line
    );
    assert!(
        response.contains("百度"),
        "Expected '百度' in real baidu response"
    );
}

#[tokio::test]
#[ignore] // Requires real network access to www.baidu.com
async fn real_baidu_direct_mode() {
    use tokio::net::TcpStream;

    // Direct connection to www.baidu.com — no Phantom tunnel
    let mut stream = TcpStream::connect("www.baidu.com:80")
        .await
        .expect("Failed to connect directly to www.baidu.com");

    // Send an HTTP GET request
    let request = "GET / HTTP/1.1\r\nHost: www.baidu.com\r\nConnection: close\r\n\r\n";
    stream.write_all(request.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();

    // Read the full HTTP response
    let mut buf = Vec::new();
    stream.read_to_end(&mut buf).await.unwrap();

    let response = String::from_utf8_lossy(&buf);
    let status_line = response.lines().next().unwrap_or("");
    assert!(
        status_line.contains("200 OK") || status_line.contains("302"),
        "Expected 200 OK or 302 redirect, got: {}",
        status_line
    );
    assert!(
        response.contains("百度"),
        "Expected '百度' in real baidu response"
    );
}
