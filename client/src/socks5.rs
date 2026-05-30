use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::BytesMut;
use phantom_core::constants::MAX_FRAME_PAYLOAD;
use phantom_core::CipherPreference;
use phantom_core::{ClientConfig, PhantomError, Result, ServerEntry};
use phantom_crypto::cipher::CipherSuite;
use phantom_crypto::session::CipherOffer;
use phantom_crypto::{split_after_handshake, NoiseInitiator};
use phantom_protocol::codec::{FrameReader, FrameWriter};
use phantom_protocol::frame::FrameFlags;
use phantom_protocol::{Frame, TargetAddr};
use phantom_core::TransportProtocol;
use phantom_transport::tcp::TcpTransport;
use phantom_transport::Transport;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

use crate::failover::FailoverManager;

pub async fn handle_socks5_connection(
    mut socks5: TcpStream,
    config: &ClientConfig,
    failover: &FailoverManager,
    local_secret: [u8; 32],
) -> Result<()> {
    // 1. SOCKS5 method negotiation
    negotiate_method(&mut socks5).await?;

    // 2. SOCKS5 request
    let target = read_request(&mut socks5).await?;

    // 3. Select server via failover manager
    let server = failover.select_server()?;

    // 4. Establish encrypted tunnel via selected transport protocol
    match server.protocol {
        TransportProtocol::Tcp => {
            let transport = TcpTransport::new(std::time::Duration::from_secs(10));
            match establish_tunnel(&transport, server, &local_secret, &target, config.client.cipher).await {
                Ok((frame_reader, frame_writer, stream_id)) => {
                    send_reply(&mut socks5, 0x00).await?;
                    relay_socks5_tunnel(socks5, frame_reader, frame_writer, stream_id).await
                }
                Err(e) => {
                    let _ = send_reply(&mut socks5, 0x05).await;
                    Err(e)
                }
            }
        }
        TransportProtocol::Quic => {
            let server_name = server.address.split(':').next().unwrap_or("").to_string();
            let transport = phantom_transport::quic::QuicTransport::new(
                std::time::Duration::from_secs(10),
                &server_name,
            );
            match establish_tunnel(&transport, server, &local_secret, &target, config.client.cipher).await {
                Ok((frame_reader, frame_writer, stream_id)) => {
                    send_reply(&mut socks5, 0x00).await?;
                    relay_socks5_tunnel(socks5, frame_reader, frame_writer, stream_id).await
                }
                Err(e) => {
                    let _ = send_reply(&mut socks5, 0x05).await;
                    Err(e)
                }
            }
        }
    }
}

async fn negotiate_method(stream: &mut TcpStream) -> Result<()> {
    let mut buf = [0u8; 2];
    stream
        .read_exact(&mut buf)
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 negotiation read failed: {}", e)))?;

    if buf[0] != 0x05 {
        return Err(PhantomError::Protocol(format!(
            "Not SOCKS5: version {}",
            buf[0]
        )));
    }

    let nmethods = buf[1] as usize;
    let mut methods = vec![0u8; nmethods];
    stream
        .read_exact(&mut methods)
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 methods read failed: {}", e)))?;

    if !methods.contains(&0x00) {
        let _ = stream.write_all(&[0x05, 0xFF]).await;
        return Err(PhantomError::Protocol(
            "No acceptable SOCKS5 auth method".to_string(),
        ));
    }

    stream
        .write_all(&[0x05, 0x00])
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 negotiation reply failed: {}", e)))?;

    Ok(())
}

async fn read_request(stream: &mut TcpStream) -> Result<TargetAddr> {
    let mut header = [0u8; 4];
    stream
        .read_exact(&mut header)
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 request read failed: {}", e)))?;

    if header[0] != 0x05 {
        return Err(PhantomError::Protocol(format!(
            "Not SOCKS5: version {}",
            header[0]
        )));
    }

    if header[1] != 0x01 {
        let _ = send_reply(stream, 0x07).await;
        return Err(PhantomError::Protocol(format!(
            "Unsupported SOCKS5 command: {}",
            header[1]
        )));
    }

    let atyp = header[3];
    let target = match atyp {
        0x01 => {
            let mut addr = [0u8; 4];
            stream.read_exact(&mut addr).await.map_err(PhantomError::Io)?;
            let mut port_buf = [0u8; 2];
            stream.read_exact(&mut port_buf).await.map_err(PhantomError::Io)?;
            let port = u16::from_be_bytes(port_buf);
            TargetAddr::IPv4(addr, port)
        }
        0x03 => {
            let mut len_buf = [0u8; 1];
            stream.read_exact(&mut len_buf).await.map_err(PhantomError::Io)?;
            let domain_len = len_buf[0] as usize;
            let mut domain = vec![0u8; domain_len];
            stream.read_exact(&mut domain).await.map_err(PhantomError::Io)?;
            let domain_str = String::from_utf8(domain)
                .map_err(|e| PhantomError::Protocol(format!("Invalid domain: {}", e)))?;
            let mut port_buf = [0u8; 2];
            stream.read_exact(&mut port_buf).await.map_err(PhantomError::Io)?;
            let port = u16::from_be_bytes(port_buf);
            TargetAddr::Domain(domain_str, port)
        }
        0x04 => {
            let mut addr = [0u8; 16];
            stream.read_exact(&mut addr).await.map_err(PhantomError::Io)?;
            let mut port_buf = [0u8; 2];
            stream.read_exact(&mut port_buf).await.map_err(PhantomError::Io)?;
            let port = u16::from_be_bytes(port_buf);
            TargetAddr::IPv6(addr, port)
        }
        _ => {
            return Err(PhantomError::Protocol(format!(
                "Unsupported address type: {}",
                atyp
            )));
        }
    };

    Ok(target)
}

async fn send_reply(stream: &mut TcpStream, reply: u8) -> Result<()> {
    let reply_bytes: [u8; 10] = [0x05, reply, 0x00, 0x01, 0, 0, 0, 0, 0, 0];
    stream
        .write_all(&reply_bytes)
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 reply failed: {}", e)))?;
    stream
        .flush()
        .await
        .map_err(|e| PhantomError::Protocol(format!("SOCKS5 flush failed: {}", e)))?;
    Ok(())
}

fn resolve_offer(cipher_preference: CipherPreference) -> CipherOffer {
    match cipher_preference {
        CipherPreference::Auto => CipherOffer::default_offer(),
        CipherPreference::Aes256Gcm => CipherOffer::new(vec![CipherSuite::Aes256Gcm]),
        CipherPreference::Aes128Gcm => CipherOffer::new(vec![CipherSuite::Aes128Gcm]),
        CipherPreference::Ascon128 => CipherOffer::new(vec![CipherSuite::Ascon128]),
        CipherPreference::ChaCha20Poly1305 => CipherOffer::new(vec![CipherSuite::ChaCha20Poly]),
    }
}

async fn establish_tunnel<T: Transport>(
    transport: &T,
    server: &ServerEntry,
    local_secret: &[u8; 32],
    target: &TargetAddr,
    cipher_preference: CipherPreference,
) -> Result<(
    FrameReader<tokio::io::ReadHalf<T::Stream>>,
    FrameWriter<tokio::io::WriteHalf<T::Stream>>,
    u32,
)> {
    let addr: std::net::SocketAddr = server
        .address
        .parse()
        .map_err(|e| PhantomError::Config(format!("Invalid server address: {}", e)))?;

    let stream = transport.connect(&addr).await?;

    let remote_public = decode_public_key(&server.public_key)?;
    let initiator = NoiseInitiator::new(local_secret, &remote_public);
    let offer = resolve_offer(cipher_preference);
    let result = initiator.handshake(stream, &offer).await?;

    tracing::debug!("Cipher negotiated: {}", result.chosen_cipher);

    let (session_reader, session_writer) = split_after_handshake(
        result.stream,
        result.split_keys,
        result.chosen_cipher,
        result.is_initiator,
    );
    let mut frame_reader = FrameReader::new(session_reader);
    let mut frame_writer = FrameWriter::new(session_writer);

    let stream_id: u32 = 1;
    frame_writer
        .write_frame(&Frame::syn(stream_id, target.encode()))
        .await?;
    frame_writer.flush().await?;

    let response = frame_reader.read_frame().await?;
    if response.flags.contains(FrameFlags::RST) {
        return Err(PhantomError::ServerUnreachable {
            name: server.name.clone(),
        });
    }
    if !response.flags.contains(FrameFlags::ACK) {
        return Err(PhantomError::Protocol("Expected ACK".to_string()));
    }

    Ok((frame_reader, frame_writer, stream_id))
}

fn decode_public_key(b64: &str) -> Result<[u8; 32]> {
    let decoded = STANDARD
        .decode(b64.trim())
        .map_err(|e| PhantomError::Crypto(format!("Base64 decode failed: {}", e)))?;
    if decoded.len() != 32 {
        return Err(PhantomError::Crypto(format!(
            "Public key must be 32 bytes, got {}",
            decoded.len()
        )));
    }
    let mut key = [0u8; 32];
    key.copy_from_slice(&decoded);
    Ok(key)
}

pub async fn relay_socks5_tunnel<R, W>(
    socks5: TcpStream,
    mut frame_reader: FrameReader<R>,
    mut frame_writer: FrameWriter<W>,
    stream_id: u32,
) -> Result<()>
where
    R: tokio::io::AsyncRead + Unpin,
    W: tokio::io::AsyncWrite + Unpin,
{
    let (mut s5_read, mut s5_write) = tokio::io::split(socks5);

    let to_tunnel = async {
        let mut buf = BytesMut::with_capacity(MAX_FRAME_PAYLOAD);
        loop {
            buf.clear();
            let n = s5_read.read_buf(&mut buf).await.map_err(PhantomError::Io)?;
            if n == 0 {
                break;
            }
            let data = buf.split().freeze();
            frame_writer
                .write_frame(&Frame::data(stream_id, data))
                .await?;
        }
        let _ = frame_writer.write_frame(&Frame::fin(stream_id)).await;
        let _ = frame_writer.flush().await;
        Ok::<_, PhantomError>(())
    };

    let from_tunnel = async {
        loop {
            let frame = frame_reader.read_frame().await?;
            if frame.flags.contains(FrameFlags::DATA) {
                s5_write
                    .write_all(&frame.payload)
                    .await
                    .map_err(PhantomError::Io)?;
            } else if frame.flags.contains(FrameFlags::FIN)
                || frame.flags.contains(FrameFlags::RST)
            {
                break;
            }
        }
        let _ = s5_write.shutdown().await;
        Ok::<_, PhantomError>(())
    };

    tokio::try_join!(to_tunnel, from_tunnel)?;
    Ok(())
}
