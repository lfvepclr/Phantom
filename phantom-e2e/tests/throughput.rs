use phantom_core::CipherPreference;
use phantom_e2e::echo::EchoMode;
use phantom_e2e::fixture::TestFixture;
use phantom_e2e::socks5::connect_tunnel;
use phantom_e2e::throughput::{measure_echo_throughput, measure_send_throughput};
use phantom_protocol::TargetAddr;

const THROUGHPUT_TEST_SIZE: usize = 50 * 1024 * 1024;

async fn measure_cipher_throughput(cipher: CipherPreference, size: usize) -> phantom_e2e::throughput::ThroughputResult {
    let fixture = TestFixture::new_with_mode(cipher, EchoMode::Echo).await;
    let ip_bytes = match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => ip.octets(),
        std::net::IpAddr::V6(_) => [127, 0, 0, 1],
    };
    let target = TargetAddr::IPv4(ip_bytes, fixture.target_addr.port());
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr, &fixture.server_key.public, &fixture.client_key.secret, &target, fixture.cipher_preference,
    ).await.unwrap();
    measure_echo_throughput(&mut reader, &mut writer, stream_id, size).await
}

async fn measure_cipher_send_throughput(cipher: CipherPreference, size: usize) -> phantom_e2e::throughput::ThroughputResult {
    let fixture = TestFixture::new_with_mode(cipher, EchoMode::Sink).await;
    let ip_bytes = match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => ip.octets(),
        std::net::IpAddr::V6(_) => [127, 0, 0, 1],
    };
    let target = TargetAddr::IPv4(ip_bytes, fixture.target_addr.port());
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr, &fixture.server_key.public, &fixture.client_key.secret, &target, fixture.cipher_preference,
    ).await.unwrap();
    measure_send_throughput(&mut reader, &mut writer, stream_id, size).await
}

#[tokio::test]
#[ignore]
async fn throughput_tcp_aes256gcm() {
    let result = measure_cipher_throughput(CipherPreference::Aes256Gcm, THROUGHPUT_TEST_SIZE).await;
    eprintln!("[AES-256-GCM] {}", result);
    assert!(result.throughput_mbps > 0.0);
}

#[tokio::test]
#[ignore]
async fn throughput_tcp_aes128gcm() {
    let result = measure_cipher_throughput(CipherPreference::Aes128Gcm, THROUGHPUT_TEST_SIZE).await;
    eprintln!("[AES-128-GCM] {}", result);
    assert!(result.throughput_mbps > 0.0);
}

#[tokio::test]
#[ignore]
async fn throughput_tcp_ascon128() {
    let result = measure_cipher_throughput(CipherPreference::Ascon128, THROUGHPUT_TEST_SIZE).await;
    eprintln!("[ASCON-128] {}", result);
    assert!(result.throughput_mbps > 0.0);
}

#[tokio::test]
#[ignore]
async fn throughput_tcp_chacha20poly() {
    let result = measure_cipher_throughput(CipherPreference::ChaCha20Poly1305, THROUGHPUT_TEST_SIZE).await;
    eprintln!("[ChaCha20-Poly1305] {}", result);
    assert!(result.throughput_mbps > 0.0);
}

#[tokio::test]
#[ignore]
async fn throughput_tcp_aes256gcm_send_only() {
    let result = measure_cipher_send_throughput(CipherPreference::Aes256Gcm, THROUGHPUT_TEST_SIZE).await;
    eprintln!("[AES-256-GCM send-only] {}", result);
    assert!(result.throughput_mbps > 0.0);
}

#[tokio::test]
#[ignore]
async fn throughput_tcp_ascon128_send_only() {
    let result = measure_cipher_send_throughput(CipherPreference::Ascon128, THROUGHPUT_TEST_SIZE).await;
    eprintln!("[ASCON-128 send-only] {}", result);
    assert!(result.throughput_mbps > 0.0);
}

#[tokio::test]
#[ignore]
async fn throughput_tcp_concurrent_10() {
    let fixture = TestFixture::new_with_mode(CipherPreference::Aes256Gcm, EchoMode::Echo).await;
    let ip_bytes = match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => ip.octets(),
        std::net::IpAddr::V6(_) => [127, 0, 0, 1],
    };
    let target = TargetAddr::IPv4(ip_bytes, fixture.target_addr.port());
    let per_conn_bytes = 5 * 1024 * 1024;
    let start = std::time::Instant::now();
    let mut handles = Vec::new();
    for _ in 0..10 {
        let server_addr = fixture.server_addr;
        let server_public = fixture.server_key.public;
        let client_secret = fixture.client_key.secret;
        let target_clone = target.clone();
        let cipher = fixture.cipher_preference;
        let handle = tokio::spawn(async move {
            let (mut reader, mut writer, stream_id) = connect_tunnel(
                server_addr, &server_public, &client_secret, &target_clone, cipher,
            ).await.unwrap();
            measure_echo_throughput(&mut reader, &mut writer, stream_id, per_conn_bytes).await
        });
        handles.push(handle);
    }
    let mut total_bytes = 0usize;
    for handle in handles {
        let result = handle.await.unwrap();
        total_bytes += result.bytes_sent + result.bytes_received;
    }
    let elapsed = start.elapsed();
    let throughput_mbps = (total_bytes as f64 * 8.0) / elapsed.as_secs_f64() / 1_000_000.0;
    eprintln!("[Concurrent 10] total {} bytes, elapsed {:.2}s, aggregate {:.2} Mbps", total_bytes, elapsed.as_secs_f64(), throughput_mbps);
    assert!(throughput_mbps > 0.0);
}

#[tokio::test]
#[ignore]
async fn throughput_cipher_comparison() {
    let ciphers = [
        (CipherPreference::Aes256Gcm, "AES-256-GCM"),
        (CipherPreference::Aes128Gcm, "AES-128-GCM"),
        (CipherPreference::Ascon128, "ASCON-128"),
        (CipherPreference::ChaCha20Poly1305, "ChaCha20-Poly1305"),
    ];
    let test_size = 20 * 1024 * 1024;
    eprintln!("\n===== Cipher Throughput Comparison ({} MB echo) =====", test_size / 1024 / 1024);
    for (cipher, name) in &ciphers {
        let result = measure_cipher_throughput(*cipher, test_size).await;
        eprintln!("  {:20} {}", name, result);
    }
    eprintln!("=============================================\n");
}
