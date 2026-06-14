//! Performance benchmarks for the Phantom tunnel.
//!
//! Uses `std::time::Instant` for timing. Tests are marked `#[ignore]`
//! by default so they do not run in CI; run with `cargo test -- --ignored`.

use phantom_core::CipherPreference;
use phantom_core::protocol::TargetAddr;
use phantom_e2e::echo::EchoMode;
use phantom_e2e::fixture::TestFixture;
use phantom_e2e::socks5::connect_tunnel;
use phantom_e2e::throughput::{echo_data, generate_random_data, measure_echo_throughput};
use std::time::Instant;

fn target_from_fixture(fixture: &TestFixture) -> TargetAddr {
    match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => TargetAddr::IPv4(ip.octets(), fixture.target_addr.port()),
        std::net::IpAddr::V6(_) => TargetAddr::IPv4([127, 0, 0, 1], fixture.target_addr.port()),
    }
}

/// Measure TCP echo throughput through the tunnel for a given cipher.
/// Sends 10 MB and measures round-trip throughput.
#[tokio::test]
#[ignore]
async fn perf_tcp_throughput_aes256gcm() {
    let fixture = TestFixture::new_with_mode(CipherPreference::Aes256Gcm, EchoMode::Echo).await;
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();

    let result =
        measure_echo_throughput(&mut reader, &mut writer, stream_id, 10 * 1024 * 1024).await;
    eprintln!("[AES-256-GCM throughput] {}", result);
    assert!(
        result.throughput_mbps > 0.0,
        "Throughput should be positive"
    );
}

#[tokio::test]
#[ignore]
async fn perf_tcp_throughput_ascon128() {
    let fixture = TestFixture::new_with_mode(CipherPreference::Ascon128, EchoMode::Echo).await;
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();

    let result =
        measure_echo_throughput(&mut reader, &mut writer, stream_id, 10 * 1024 * 1024).await;
    eprintln!("[ASCON-128 throughput] {}", result);
    assert!(
        result.throughput_mbps > 0.0,
        "Throughput should be positive"
    );
}

#[tokio::test]
#[ignore]
async fn perf_tcp_throughput_chacha20poly1305() {
    let fixture =
        TestFixture::new_with_mode(CipherPreference::ChaCha20Poly1305, EchoMode::Echo).await;
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();

    let result =
        measure_echo_throughput(&mut reader, &mut writer, stream_id, 10 * 1024 * 1024).await;
    eprintln!("[ChaCha20-Poly1305 throughput] {}", result);
    assert!(
        result.throughput_mbps > 0.0,
        "Throughput should be positive"
    );
}

/// Measure handshake latency for each cipher suite.
/// Time from TCP connect attempt to receiving the ACK frame.
#[tokio::test]
#[ignore]
async fn perf_handshake_latency_aes256gcm() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let start = Instant::now();
    let (_reader, _writer, _stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();
    eprintln!(
        "[AES-256-GCM handshake] {:.2} ms",
        elapsed.as_secs_f64() * 1000.0
    );
    assert!(
        elapsed.as_secs() < 5,
        "Handshake should complete within 5 seconds"
    );
}

#[tokio::test]
#[ignore]
async fn perf_handshake_latency_ascon128() {
    let fixture = TestFixture::new(CipherPreference::Ascon128).await;
    let start = Instant::now();
    let (_reader, _writer, _stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();
    eprintln!(
        "[ASCON-128 handshake] {:.2} ms",
        elapsed.as_secs_f64() * 1000.0
    );
    assert!(
        elapsed.as_secs() < 5,
        "Handshake should complete within 5 seconds"
    );
}

#[tokio::test]
#[ignore]
async fn perf_handshake_latency_chacha20poly1305() {
    let fixture = TestFixture::new(CipherPreference::ChaCha20Poly1305).await;
    let start = Instant::now();
    let (_reader, _writer, _stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();
    eprintln!(
        "[ChaCha20-Poly1305 handshake] {:.2} ms",
        elapsed.as_secs_f64() * 1000.0
    );
    assert!(
        elapsed.as_secs() < 5,
        "Handshake should complete within 5 seconds"
    );
}

#[tokio::test]
#[ignore]
async fn perf_handshake_latency_aes128gcm() {
    let fixture = TestFixture::new(CipherPreference::Aes128Gcm).await;
    let start = Instant::now();
    let (_reader, _writer, _stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();
    let elapsed = start.elapsed();
    eprintln!(
        "[AES-128-GCM handshake] {:.2} ms",
        elapsed.as_secs_f64() * 1000.0
    );
    assert!(
        elapsed.as_secs() < 5,
        "Handshake should complete within 5 seconds"
    );
}

/// Measure concurrent connection throughput.
/// Spawns N connections, each sending a small payload, and measures
/// aggregate completion time.
#[tokio::test]
#[ignore]
async fn perf_concurrent_connections() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let num_concurrent = 20;
    let payload_size = 65536;

    let start = Instant::now();
    let mut handles = Vec::new();
    for i in 0..num_concurrent {
        let server_addr = fixture.server_addr;
        let server_public = fixture.server_key.public;
        let client_secret = fixture.client_key.secret;
        let target = target_from_fixture(&fixture);
        let cipher = fixture.cipher_preference;
        let handle = tokio::spawn(async move {
            let (mut reader, mut writer, stream_id) =
                connect_tunnel(server_addr, &server_public, &client_secret, &target, cipher)
                    .await
                    .unwrap();
            let mut data = vec![0u8; payload_size];
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
    let elapsed = start.elapsed();
    let total_bytes = num_concurrent as u64 * payload_size as u64 * 2; // up + down
    let throughput_mbps = (total_bytes as f64 * 8.0) / elapsed.as_secs_f64() / 1_000_000.0;
    eprintln!(
        "[Concurrent {} x {}KB] elapsed {:.2}s, aggregate {:.2} Mbps",
        num_concurrent,
        payload_size / 1024,
        elapsed.as_secs_f64(),
        throughput_mbps
    );
    assert!(
        throughput_mbps > 0.0,
        "Aggregate throughput should be positive"
    );
}

/// Cipher comparison: run the same workload with each cipher and print
/// a side-by-side comparison table.
#[tokio::test]
#[ignore]
async fn perf_cipher_comparison() {
    let ciphers = [
        (CipherPreference::Aes256Gcm, "AES-256-GCM"),
        (CipherPreference::Aes128Gcm, "AES-128-GCM"),
        (CipherPreference::Ascon128, "ASCON-128"),
        (CipherPreference::ChaCha20Poly1305, "ChaCha20-Poly1305"),
    ];
    let test_size = 20 * 1024 * 1024;

    eprintln!(
        "\n===== Cipher Performance Comparison ({} MB echo) =====",
        test_size / 1024 / 1024
    );
    for (cipher, name) in &ciphers {
        let fixture = TestFixture::new_with_mode(*cipher, EchoMode::Echo).await;
        let (mut reader, mut writer, stream_id) = connect_tunnel(
            fixture.server_addr,
            &fixture.server_key.public,
            &fixture.client_key.secret,
            &target_from_fixture(&fixture),
            fixture.cipher_preference,
        )
        .await
        .unwrap();

        let result = measure_echo_throughput(&mut reader, &mut writer, stream_id, test_size).await;
        eprintln!("  {:20} {}", name, result);
    }
    eprintln!("=============================================\n");
}
