use phantom_core::CipherPreference;
use phantom_core::crypto::KeyPair;
use phantom_core::protocol::TargetAddr;
use phantom_e2e::echo::EchoMode;
use phantom_e2e::fixture::{TestFixture, TestFixtureBuilder};
use phantom_e2e::socks5::connect_tunnel;
use phantom_e2e::throughput::echo_data;

fn target_from_fixture(fixture: &TestFixture) -> TargetAddr {
    match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => TargetAddr::IPv4(ip.octets(), fixture.target_addr.port()),
        std::net::IpAddr::V6(_) => TargetAddr::IPv4([127, 0, 0, 1], fixture.target_addr.port()),
    }
}

/// Empty allowed_clients list means any client key is accepted.
#[tokio::test]
async fn empty_allowed_clients_accepts_any() {
    let fixture = TestFixtureBuilder::new().build().await;
    let result = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await;
    assert!(
        result.is_ok(),
        "Empty allowed_clients should accept any client key"
    );
}

/// When allowed_clients contains only a key that does NOT match the client,
/// the server silently drops the connection after handshake.
#[tokio::test]
async fn allowed_clients_rejects_unknown_key() {
    let stranger = KeyPair::generate().expect("Failed to generate stranger key");
    let fixture = TestFixtureBuilder::new()
        .allowed_client(stranger.public)
        .build()
        .await;
    let result = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await;
    assert!(
        result.is_err(),
        "Connection with unknown client key should be rejected"
    );
}

/// When allowed_clients contains the client's actual public key,
/// the connection is accepted.
#[tokio::test]
async fn allowed_clients_accepts_known_key() {
    let client_key = KeyPair::generate().expect("Failed to generate client key");
    let fixture = TestFixtureBuilder::new()
        .allowed_client(client_key.public)
        .client_key(client_key)
        .build()
        .await;
    let result = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await;
    assert!(
        result.is_ok(),
        "Connection with known client key should be accepted"
    );
}

/// EchoMode::Sink causes the echo server to swallow data instead of echoing
/// it back, so the client receives no payload before the FIN.
#[tokio::test]
async fn echo_mode_sink_swallows_data() {
    let fixture = TestFixture::new_with_mode(CipherPreference::Aes256Gcm, EchoMode::Sink).await;
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();
    let data = b"sink test".to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert!(
        echoed.is_empty(),
        "Sink mode should not echo data back, got {} bytes",
        echoed.len()
    );
}
