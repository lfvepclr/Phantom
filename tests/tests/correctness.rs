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

#[tokio::test]
async fn tcp_aes256gcm_echo_small() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();
    let data = generate_random_data(1024);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Echoed data does not match original");
}

#[tokio::test]
async fn tcp_aes128gcm_echo_small() {
    let fixture = TestFixture::new(CipherPreference::Aes128Gcm).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();
    let data = generate_random_data(1024);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Echoed data does not match original");
}

#[tokio::test]
async fn tcp_ascon128_echo_small() {
    let fixture = TestFixture::new(CipherPreference::Ascon128).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();
    let data = generate_random_data(1024);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Echoed data does not match original");
}

#[tokio::test]
async fn tcp_chacha20poly_echo_small() {
    let fixture = TestFixture::new(CipherPreference::ChaCha20Poly1305).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();
    let data = generate_random_data(1024);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Echoed data does not match original");
}

#[tokio::test]
async fn tcp_aes256gcm_echo_large() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();
    let data = generate_random_data(10 * 1024 * 1024);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed.len(), data.len(), "Data length mismatch");
    assert_eq!(echoed, data, "Echoed data does not match original (10MB)");
}

#[tokio::test]
async fn tcp_ascon128_echo_large() {
    let fixture = TestFixture::new(CipherPreference::Ascon128).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();
    let data = generate_random_data(10 * 1024 * 1024);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed.len(), data.len(), "Data length mismatch");
    assert_eq!(echoed, data, "Echoed data does not match original (10MB)");
}

#[tokio::test]
async fn tcp_various_payload_sizes() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    for &size in &[1, 64, 1024, 4096, 8192, 16384] {
        let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();
        let data = generate_random_data(size);
        let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
        assert_eq!(
            echoed, data,
            "Data mismatch for payload size {} bytes",
            size
        );
    }
}

#[tokio::test]
async fn tcp_concurrent_connections() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let ip_bytes = match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => ip.octets(),
        std::net::IpAddr::V6(_) => [127, 0, 0, 1],
    };
    let target = TargetAddr::IPv4(ip_bytes, fixture.target_addr.port());
    let num_concurrent = 10;
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

#[tokio::test]
async fn tcp_target_unreachable() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let target = TargetAddr::IPv4([192, 0, 2, 1], 1);
    let result = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target,
        fixture.cipher_preference,
    )
    .await;
    assert!(
        result.is_err(),
        "Expected error when connecting to unreachable target"
    );
}

#[tokio::test]
async fn tcp_ipv4_target() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();
    let data = b"hello ipv4 target".to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "IPv4 target echo mismatch");
}

#[tokio::test]
async fn tcp_auto_cipher_negotiation() {
    let fixture = TestFixture::new(CipherPreference::Auto).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();
    let data = b"auto cipher test".to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Auto cipher negotiation echo mismatch");
}

#[tokio::test]
async fn tcp_empty_payload() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();
    let data: Vec<u8> = vec![];
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Empty payload echo mismatch");
}

#[tokio::test]
async fn tcp_single_byte() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel(&fixture).await.unwrap();
    let data = vec![0x42];
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Single byte echo mismatch");
}
