use phantom_core::CipherPreference;
use phantom_core::protocol::TargetAddr;
use phantom_e2e::echo::EchoMode;
use phantom_e2e::fixture::TestFixture;
use phantom_e2e::network::{NetworkCondition, ThrottledProxy};
use phantom_e2e::socks5::connect_tunnel;
use phantom_e2e::throughput::{echo_data, generate_random_data, measure_echo_throughput};

const WEAK_NET_TEST_SIZE: usize = 1024 * 1024;

async fn setup_tunnel_with_proxy(
    fixture: &TestFixture,
    proxy_addr: std::net::SocketAddr,
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
        proxy_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target,
        fixture.cipher_preference,
    )
    .await
}

#[tokio::test]
async fn weak_net_latency_50ms() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let proxy =
        ThrottledProxy::start(fixture.server_addr, NetworkCondition::high_latency(50)).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel_with_proxy(&fixture, proxy.listen_addr)
        .await
        .unwrap();
    let data = generate_random_data(WEAK_NET_TEST_SIZE);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Data mismatch at 50ms latency");
}

#[tokio::test]
async fn weak_net_latency_200ms() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let proxy =
        ThrottledProxy::start(fixture.server_addr, NetworkCondition::high_latency(200)).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel_with_proxy(&fixture, proxy.listen_addr)
        .await
        .unwrap();
    let data = generate_random_data(WEAK_NET_TEST_SIZE);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Data mismatch at 200ms latency");
}

#[tokio::test]
#[ignore]
async fn weak_net_latency_500ms() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let proxy =
        ThrottledProxy::start(fixture.server_addr, NetworkCondition::high_latency(500)).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel_with_proxy(&fixture, proxy.listen_addr)
        .await
        .unwrap();
    let data = generate_random_data(256 * 1024);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Data mismatch at 500ms latency");
}

#[tokio::test]
async fn weak_net_bandwidth_10mbps() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let proxy = ThrottledProxy::start(
        fixture.server_addr,
        NetworkCondition::low_bandwidth(10_000_000 / 8),
    )
    .await;
    let (mut reader, mut writer, stream_id) = setup_tunnel_with_proxy(&fixture, proxy.listen_addr)
        .await
        .unwrap();
    let data = generate_random_data(WEAK_NET_TEST_SIZE);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Data mismatch at 10Mbps bandwidth");
}

#[tokio::test]
#[ignore]
async fn weak_net_bandwidth_1mbps() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let proxy = ThrottledProxy::start(
        fixture.server_addr,
        NetworkCondition::low_bandwidth(1_000_000 / 8),
    )
    .await;
    let (mut reader, mut writer, stream_id) = setup_tunnel_with_proxy(&fixture, proxy.listen_addr)
        .await
        .unwrap();
    let data = generate_random_data(256 * 1024);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Data mismatch at 1Mbps bandwidth");
}

#[tokio::test]
async fn weak_net_loss_1pct() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let proxy =
        ThrottledProxy::start(fixture.server_addr, NetworkCondition::packet_loss(1.0)).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel_with_proxy(&fixture, proxy.listen_addr)
        .await
        .unwrap();
    let data = generate_random_data(WEAK_NET_TEST_SIZE);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Data mismatch at 1% packet loss");
}

#[tokio::test]
#[ignore]
async fn weak_net_loss_5pct() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let proxy =
        ThrottledProxy::start(fixture.server_addr, NetworkCondition::packet_loss(5.0)).await;
    let (mut reader, mut writer, stream_id) = setup_tunnel_with_proxy(&fixture, proxy.listen_addr)
        .await
        .unwrap();
    let data = generate_random_data(256 * 1024);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Data mismatch at 5% packet loss");
}

#[tokio::test]
async fn weak_net_moderate() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let proxy = ThrottledProxy::start(
        fixture.server_addr,
        NetworkCondition::combined(100, 10_000_000 / 8, 1.0),
    )
    .await;
    let (mut reader, mut writer, stream_id) = setup_tunnel_with_proxy(&fixture, proxy.listen_addr)
        .await
        .unwrap();
    let data = generate_random_data(512 * 1024);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Data mismatch in moderate weak network");
}

#[tokio::test]
async fn weak_net_jitter() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let proxy = ThrottledProxy::start(
        fixture.server_addr,
        NetworkCondition::high_latency(100).with_jitter(50),
    )
    .await;
    let (mut reader, mut writer, stream_id) = setup_tunnel_with_proxy(&fixture, proxy.listen_addr)
        .await
        .unwrap();
    let data = generate_random_data(WEAK_NET_TEST_SIZE);
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(echoed, data, "Data mismatch in jitter network");
}

#[tokio::test]
#[ignore]
async fn weak_net_200ms_aes256gcm_vs_ascon128() {
    let condition = NetworkCondition::high_latency(200);
    let test_size = 256 * 1024;
    eprintln!(
        "\n===== Weak Network Cipher Comparison (200ms latency, {} KB) =====",
        test_size / 1024
    );
    for (cipher, name) in [
        (CipherPreference::Aes256Gcm, "AES-256-GCM"),
        (CipherPreference::Ascon128, "ASCON-128"),
    ] {
        let fixture = TestFixture::new_with_mode(cipher, EchoMode::Echo).await;
        let proxy = ThrottledProxy::start(fixture.server_addr, condition.clone()).await;
        let ip_bytes = match fixture.target_addr.ip() {
            std::net::IpAddr::V4(ip) => ip.octets(),
            std::net::IpAddr::V6(_) => [127, 0, 0, 1],
        };
        let target = TargetAddr::IPv4(ip_bytes, fixture.target_addr.port());
        let (mut reader, mut writer, stream_id) = connect_tunnel(
            proxy.listen_addr,
            &fixture.server_key.public,
            &fixture.client_key.secret,
            &target,
            fixture.cipher_preference,
        )
        .await
        .unwrap();
        let result = measure_echo_throughput(&mut reader, &mut writer, stream_id, test_size).await;
        eprintln!("  {:20} {}", name, result);
    }
    eprintln!("======================================================\n");
}
