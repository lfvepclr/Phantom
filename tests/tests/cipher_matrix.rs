use phantom_core::CipherPreference;
use phantom_e2e::fixture::TestFixture;
use phantom_e2e::socks5::connect_tunnel;
use phantom_e2e::throughput::echo_data;
use phantom_core::protocol::TargetAddr;

fn target_from_fixture(fixture: &TestFixture) -> TargetAddr {
    match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => TargetAddr::IPv4(ip.octets(), fixture.target_addr.port()),
        std::net::IpAddr::V6(_) => TargetAddr::IPv4([127, 0, 0, 1], fixture.target_addr.port()),
    }
}

#[tokio::test]
async fn cipher_aes256gcm() {
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
    let data = b"hello aes256".to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(&echoed, b"hello aes256");
}

#[tokio::test]
async fn cipher_aes128gcm() {
    let fixture = TestFixture::new(CipherPreference::Aes128Gcm).await;
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();
    let data = b"hello aes128".to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(&echoed, b"hello aes128");
}

#[tokio::test]
async fn cipher_chacha20() {
    let fixture = TestFixture::new(CipherPreference::ChaCha20Poly1305).await;
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();
    let data = b"hello chacha".to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(&echoed, b"hello chacha");
}

#[tokio::test]
async fn cipher_ascon128() {
    let fixture = TestFixture::new(CipherPreference::Ascon128).await;
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();
    let data = b"hello ascon".to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(&echoed, b"hello ascon");
}

#[tokio::test]
async fn cipher_auto() {
    let fixture = TestFixture::new(CipherPreference::Auto).await;
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target_from_fixture(&fixture),
        fixture.cipher_preference,
    )
    .await
    .unwrap();
    let data = b"hello auto".to_vec();
    let echoed = echo_data(&mut reader, &mut writer, stream_id, &data).await;
    assert_eq!(&echoed, b"hello auto");
}
