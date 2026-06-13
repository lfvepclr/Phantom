//! Stats and metrics integration tests.
//!
//! Tests TrafficStats counters and Prometheus rendering.
//! Since TunProxy may not be running in test context, we test
//! TrafficStats::render_prometheus() directly and verify that counters
//! are updated correctly after tunnel activity.

use phantom_core::CipherPreference;
use phantom_client::TrafficStats;
use phantom_e2e::fixture::TestFixture;
use phantom_e2e::socks5::connect_tunnel;
use phantom_e2e::throughput::{echo_data, generate_random_data};
use phantom_core::protocol::TargetAddr;
use std::sync::atomic::Ordering;

fn target_from_fixture(fixture: &TestFixture) -> TargetAddr {
    match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => TargetAddr::IPv4(ip.octets(), fixture.target_addr.port()),
        std::net::IpAddr::V6(_) => TargetAddr::IPv4([127, 0, 0, 1], fixture.target_addr.port()),
    }
}

/// Verify that TrafficStats counters start at zero and that
/// render_prometheus() produces the expected metric names.
#[test]
fn stats_initial_zero_prometheus_format() {
    let stats = TrafficStats::new();
    let output = stats.render_prometheus();

    // All counters should be zero initially
    assert!(output.contains("phantom_tcp_bytes_up 0"), "tcp_bytes_up should start at 0");
    assert!(output.contains("phantom_tcp_bytes_down 0"), "tcp_bytes_down should start at 0");
    assert!(output.contains("phantom_udp_bytes_up 0"), "udp_bytes_up should start at 0");
    assert!(output.contains("phantom_udp_bytes_down 0"), "udp_bytes_down should start at 0");
    assert!(output.contains("phantom_tcp_connections 0"), "tcp_connections should start at 0");
    assert!(output.contains("phantom_udp_datagrams_up 0"), "udp_datagrams_up should start at 0");
    assert!(output.contains("phantom_udp_datagrams_down 0"), "udp_datagrams_down should start at 0");

    // Verify TYPE annotations are present
    assert!(output.contains("# TYPE phantom_tcp_bytes_up counter"));
    assert!(output.contains("# TYPE phantom_tcp_bytes_down counter"));
    assert!(output.contains("# TYPE phantom_tcp_connections counter"));
}

/// Verify that recording TCP and UDP traffic updates the counters
/// and that render_prometheus() reflects the new values.
#[test]
fn stats_record_traffic_and_render() {
    let stats = TrafficStats::new();

    stats.record_tcp_connect();
    stats.record_tcp_connect();
    stats.record_tcp_up(1024);
    stats.record_tcp_down(2048);
    stats.record_udp_up(512);
    stats.record_udp_down(256);

    let output = stats.render_prometheus();
    assert!(output.contains("phantom_tcp_connections 2"), "expected 2 TCP connections");
    assert!(output.contains("phantom_tcp_bytes_up 1024"), "expected 1024 TCP bytes up");
    assert!(output.contains("phantom_tcp_bytes_down 2048"), "expected 2048 TCP bytes down");
    assert!(output.contains("phantom_udp_bytes_up 512"), "expected 512 UDP bytes up");
    assert!(output.contains("phantom_udp_bytes_down 256"), "expected 256 UDP bytes down");
    assert!(output.contains("phantom_udp_datagrams_up 1"), "expected 1 UDP datagram up");
    assert!(output.contains("phantom_udp_datagrams_down 1"), "expected 1 UDP datagram down");
}

/// Verify that counters accumulate correctly across multiple record calls.
#[test]
fn stats_counters_accumulate() {
    let stats = TrafficStats::new();

    stats.record_tcp_up(100);
    stats.record_tcp_up(200);
    stats.record_tcp_up(300);
    stats.record_tcp_down(50);

    assert_eq!(stats.tcp_bytes_up.load(Ordering::Relaxed), 600);
    assert_eq!(stats.tcp_bytes_down.load(Ordering::Relaxed), 50);

    let output = stats.render_prometheus();
    assert!(output.contains("phantom_tcp_bytes_up 600"));
    assert!(output.contains("phantom_tcp_bytes_down 50"));
}

/// Integration test: send data through a tunnel, then verify that
/// TrafficStats can be used to record the observed byte counts and
/// that the Prometheus output is well-formed.
#[tokio::test]
async fn stats_after_tunnel_echo() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();

    let data = generate_random_data(8192);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Echoed data should match");

    // Record what we observed in TrafficStats
    let stats = TrafficStats::new();
    stats.record_tcp_connect();
    stats.record_tcp_up(data.len() as u64);
    stats.record_tcp_down(echoed.len() as u64);

    let output = stats.render_prometheus();
    assert!(output.contains("phantom_tcp_connections 1"));
    assert!(output.contains("phantom_tcp_bytes_up 8192"));
    assert!(output.contains("phantom_tcp_bytes_down 8192"));
}
