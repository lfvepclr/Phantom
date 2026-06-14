use bytes::Bytes;
use phantom_core::CipherPreference;
use phantom_core::protocol::Frame;
use phantom_core::protocol::TargetAddr;
use phantom_core::protocol::frame::FrameFlags;
use phantom_e2e::fixture::TestFixture;
use phantom_e2e::socks5::connect_tunnel;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

#[tokio::test]
async fn debug_raw_echo_server() {
    // Test echo server directly
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;

    // Connect directly to echo target
    let mut stream = tokio::net::TcpStream::connect(fixture.target_addr)
        .await
        .unwrap();
    stream.write_all(b"hello").await.unwrap();
    stream.shutdown().await.unwrap(); // half-close

    let mut buf = vec![0u8; 1024];
    let n = tokio::time::timeout(std::time::Duration::from_secs(3), stream.read(&mut buf))
        .await
        .unwrap()
        .unwrap();

    eprintln!(
        "Echo server returned: {} bytes: {:?}",
        n,
        String::from_utf8_lossy(&buf[..n])
    );
    assert_eq!(&buf[..n], b"hello");
}

#[tokio::test]
async fn debug_echo_via_tunnel_no_fin() {
    let fixture = TestFixture::new(CipherPreference::Aes256Gcm).await;
    let ip_bytes = match fixture.target_addr.ip() {
        std::net::IpAddr::V4(ip) => ip.octets(),
        std::net::IpAddr::V6(_) => [127, 0, 0, 1],
    };
    let target = TargetAddr::IPv4(ip_bytes, fixture.target_addr.port());
    let (mut reader, mut writer, stream_id) = connect_tunnel(
        fixture.server_addr,
        &fixture.server_key.public,
        &fixture.client_key.secret,
        &target,
        fixture.cipher_preference,
    )
    .await
    .unwrap();

    // Send DATA only (no FIN) and read response
    writer
        .write_frame(&Frame::data(stream_id, Bytes::from(&b"hello"[..])))
        .await
        .unwrap();
    writer.flush().await.unwrap();
    eprintln!("DATA sent");

    // Read response
    let frame = tokio::time::timeout(std::time::Duration::from_secs(3), reader.read_frame())
        .await
        .unwrap()
        .unwrap();

    eprintln!(
        "Got frame: flags={:?}, data={:?}",
        frame.flags,
        String::from_utf8_lossy(&frame.payload)
    );
    assert!(frame.flags.contains(FrameFlags::DATA));
    assert_eq!(&frame.payload[..], b"hello");

    // Now send FIN
    writer.write_frame(&Frame::fin(stream_id)).await.unwrap();
    writer.flush().await.unwrap();
    eprintln!("FIN sent");

    // Try to read FIN from server (5s timeout)
    let fin_result =
        tokio::time::timeout(std::time::Duration::from_secs(5), reader.read_frame()).await;

    match fin_result {
        Ok(Ok(f)) => eprintln!("Got: flags={:?}", f.flags),
        Ok(Err(e)) => eprintln!("Error: {:?}", e),
        Err(_) => eprintln!("FIN read timed out!"),
    }
}
