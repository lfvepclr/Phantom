use std::net::SocketAddr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};
use phantom_core::protocol::TargetAddr;
use phantom_core::transport::Transport;

pub struct Socks5Client;

impl Socks5Client {
    pub async fn connect_ipv4(
        proxy_addr: SocketAddr, target_ip: [u8; 4], target_port: u16,
    ) -> std::io::Result<TcpStream> {
        let mut stream = timeout(Duration::from_secs(5), TcpStream::connect(proxy_addr))
            .await.map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "Connect timeout"))??;
        stream.write_all(&[0x05, 0x01, 0x00]).await?;
        stream.flush().await?;
        let mut resp = [0u8; 2];
        timeout(Duration::from_secs(5), stream.read_exact(&mut resp))
            .await.map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "Negotiation timeout"))??;
        if resp[0] != 0x05 || resp[1] != 0x00 {
            return Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "SOCKS5 method negotiation failed"));
        }
        let mut req = vec![0x05, 0x01, 0x00, 0x01];
        req.extend_from_slice(&target_ip);
        req.extend_from_slice(&target_port.to_be_bytes());
        stream.write_all(&req).await?;
        stream.flush().await?;
        let mut reply = [0u8; 10];
        timeout(Duration::from_secs(10), stream.read_exact(&mut reply))
            .await.map_err(|_| std::io::Error::new(std::io::ErrorKind::TimedOut, "Reply timeout"))??;
        if reply[1] != 0x00 {
            return Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, format!("SOCKS5 CONNECT failed with code {}", reply[1])));
        }
        Ok(stream)
    }
}

pub async fn connect_tunnel(
    server_addr: SocketAddr,
    server_public_key: &[u8; 32],
    client_secret: &[u8; 32],
    target: &TargetAddr,
    cipher_preference: phantom_core::CipherPreference,
) -> anyhow::Result<(
    phantom_core::protocol::FrameReader<tokio::io::ReadHalf<TcpStream>>,
    phantom_core::protocol::FrameWriter<tokio::io::WriteHalf<TcpStream>>,
    u32,
)> {
    use phantom_core::crypto::{NoiseInitiator, split_after_handshake};
    use phantom_core::crypto::session::CipherOffer;
    use phantom_core::crypto::cipher::CipherSuite;
    use phantom_core::protocol::codec::{FrameReader, FrameWriter};
    use phantom_core::protocol::frame::FrameFlags;
    use phantom_core::protocol::Frame;

    let transport = phantom_core::transport::tcp::TcpTransport::new(std::time::Duration::from_secs(10));
    let stream = transport.connect(&server_addr).await?;
    let offer = match cipher_preference {
        phantom_core::CipherPreference::Auto => CipherOffer::default_offer(),
        phantom_core::CipherPreference::Aes256Gcm => CipherOffer::new(vec![CipherSuite::Aes256Gcm]),
        phantom_core::CipherPreference::Aes128Gcm => CipherOffer::new(vec![CipherSuite::Aes128Gcm]),
        phantom_core::CipherPreference::Ascon128 => CipherOffer::new(vec![CipherSuite::Ascon128]),
        phantom_core::CipherPreference::ChaCha20Poly1305 => CipherOffer::new(vec![CipherSuite::ChaCha20Poly]),
    };
    let initiator = NoiseInitiator::new(client_secret, server_public_key);
    let result = initiator.handshake(stream, &offer).await?;
    let (session_reader, session_writer) = split_after_handshake(
        result.stream, result.split_keys, result.chosen_cipher, result.is_initiator,
    );
    let mut frame_reader = FrameReader::new(session_reader);
    let mut frame_writer = FrameWriter::new(session_writer);
    let stream_id: u32 = 1;
    frame_writer.write_frame(&Frame::syn(stream_id, target.encode())).await?;
    frame_writer.flush().await?;
    let response = frame_reader.read_frame().await?;
    if response.flags.contains(FrameFlags::RST) {
        return Err(anyhow::anyhow!("Server sent RST"));
    }
    if !response.flags.contains(FrameFlags::ACK) {
        return Err(anyhow::anyhow!("Expected ACK from server"));
    }
    Ok((frame_reader, frame_writer, stream_id))
}
