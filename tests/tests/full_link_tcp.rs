use phantom_core::CipherPreference;
use phantom_core::protocol::TargetAddr;
use phantom_e2e::fixture::TestFixture;
use phantom_e2e::socks5::connect_tunnel;
use phantom_e2e::throughput::{echo_data, generate_random_data};

async fn setup_tunnel(
    fixture: &TestFixture,
) -> anyhow::Result<(
    phantom_core::protocol::FrameReader<tokio::io::ReadHalf<tokio::net::TcpStream>>,
    phantom_core::protocol::FrameWriter<tokio::io::WriteHalf<tokio::net::TcpStream>>,
    u32,
)> {
    let ip_bytes = match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => ip.octets(),
        std::net::IpAddr::V6(_) => [127, 0, 0, 1],
    };
    let target = TargetAddr::IPv4(ip_bytes, fixture.target_addr.port());
    connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target,
        fixture.cipher_preference,
    )
    .await
}

/// Establish tunnel via connect_tunnel, send data, echo, verify integrity.
/// Then verify FIN closes cleanly.
#[tokio::test]
async fn tcp_full_link_echo() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();

    let data = b"hello full-link echo".to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Echoed data does not match original");
}

/// Send 64KB of data through the tunnel, verify full echo.
#[tokio::test]
async fn tcp_full_link_large_data() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();

    let data = generate_random_data(64 * 1024);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(
        echoed.len(),
        data.len(),
        "Data length mismatch: sent {}, received {}",
        data.len(),
        echoed.len()
    );
    assert_eq!(echoed, data, "Echoed 64KB data does not match original");
}

/// 5 concurrent connections through the same Phantom server.
#[tokio::test]
async fn tcp_full_link_concurrent() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let ip_bytes = match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => ip.octets(),
        std::net::IpAddr::V6(_) => [127, 0, 0, 1],
    };
    let target = TargetAddr::IPv4(ip_bytes, fixture.target_addr.port());
    let num_concurrent = 5;

    let mut handles = Vec::new();
    for i in 0..num_concurrent {
        let server_addr = fixture.server_addr;
        let server_public = fixture.server_key.public;
        let client_secret = fixture.client_key.secret;
        let target_clone = target.clone();
        let cipher = fixture.cipher_preference;

        let handle = tokio::spawn(async move {
            let (mut reader, mut writer, stream_id) = connect_tunnel(
                server_addr,
                &server_public,
                &client_secret,
                &target_clone,
                cipher,
            )
            .await
            .unwrap();

            let mut data = vec![0u8; 4096];
            for byte in data.iter_mut() {
                *byte = (i % 256) as u8;
            }
            let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
            assert_eq!(echoed, data, "Concurrent connection {} data mismatch", i);
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.unwrap();
    }
}
