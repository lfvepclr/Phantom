use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use phantom_core::constants::{HANDSHAKE_TIMEOUT_SECS, MAX_FRAME_PAYLOAD};
use phantom_core::CipherPreference;
use phantom_core::{PhantomError, Result};
use phantom_core::crypto::cipher::CipherSuite;
use phantom_core::crypto::{split_after_handshake, split_for_stream, NoiseResponder};
use phantom_core::protocol::codec::{FrameReader, FrameWriter};
use phantom_core::protocol::frame::FrameFlags;
use phantom_core::protocol::{Frame, TargetAddr};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

/// Handle a single TCP-like connection with full Noise handshake.
/// Backward compatible with v2 clients.
pub async fn handle_connection<S>(
    stream: S,
    secret_key: [u8; 32],
    allowed_clients: &[[u8; 32]],
    cipher_preference: CipherPreference,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let supported = resolve_supported_ciphers(cipher_preference);
    let result = match tokio::time::timeout(
        std::time::Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
        NoiseResponder::new(&secret_key).handshake(stream, &supported),
    )
    .await
    {
        Ok(Ok(r)) => r,
        _ => return,
    };

    if !allowed_clients.is_empty() && !allowed_clients.contains(&result.remote_static_key) {
        tracing::info!("Rejected unauthorized client");
        return;
    }

    tracing::info!("Client connected (cipher={:?})", result.chosen_cipher);

    let (session_reader, session_writer) = split_after_handshake(
        result.stream,
        result.split_keys,
        result.chosen_cipher,
        result.is_initiator,
    );
    let frame_reader = FrameReader::new(session_reader);
    let frame_writer = FrameWriter::new(session_writer);

    let _ = handle_frame_stream(frame_reader, frame_writer).await;
}

/// Handle a multiplexed QUIC connection.
///
/// The first bi-stream performs the Noise IK handshake.  All subsequent
/// bi-streams derive their session keys from the parent connection keys
/// using HKDF with the implicit stream counter (1, 2, 3 …).
pub async fn handle_quic_connection(
    conn: quinn::Connection,
    secret_key: [u8; 32],
    allowed_clients: &[[u8; 32]],
    cipher_preference: CipherPreference,
) {
    use phantom_core::transport::quic::QuicStream;

    let supported = resolve_supported_ciphers(cipher_preference);
    let mut handshake_done = false;
    let mut conn_keys: Option<([u8; 32], [u8; 32])> = None;
    let mut conn_cipher: Option<CipherSuite> = None;
    let mut stream_counter: u32 = 0;

    loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("QUIC stream accept error: {}", e);
                break;
            }
        };
        stream_counter += 1;
        let stream = QuicStream::new(send, recv);

        if !handshake_done {
            let result = match tokio::time::timeout(
                std::time::Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
                NoiseResponder::new(&secret_key).handshake(stream, &supported),
            )
            .await
            {
                Ok(Ok(r)) => r,
                _ => continue,
            };

            if !allowed_clients.is_empty() && !allowed_clients.contains(&result.remote_static_key) {
                continue;
            }

            conn_keys = Some(result.split_keys);
            conn_cipher = Some(result.chosen_cipher);
            handshake_done = true;

            let (session_reader, session_writer) = split_after_handshake(
                result.stream,
                result.split_keys,
                result.chosen_cipher,
                result.is_initiator,
            );
            let frame_reader = FrameReader::new(session_reader);
            let frame_writer = FrameWriter::new(session_writer);

            let _ = handle_frame_stream(frame_reader, frame_writer).await;
        } else {
            let keys = match conn_keys {
                Some(k) => k,
                None => continue,
            };
            let cipher = match conn_cipher {
                Some(c) => c,
                None => continue,
            };

            let (session_reader, session_writer) =
                split_for_stream(stream, &keys, cipher, false, stream_counter);
            let frame_reader = FrameReader::new(session_reader);
            let frame_writer = FrameWriter::new(session_writer);

            let _ = handle_frame_stream(frame_reader, frame_writer).await;
        }
    }
}

/// Common post-handshake logic: read SYN, connect target, relay.
/// Supports both TCP and UDP streams (UDP flagged via FrameFlags::UDP).
async fn handle_frame_stream<R, W>(
    mut frame_reader: FrameReader<R>,
    mut frame_writer: FrameWriter<W>,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let syn_frame = match frame_reader.read_frame().await {
        Ok(f) if f.flags.contains(FrameFlags::SYN) => f,
        _ => return Ok(()),
    };

    if syn_frame.flags.contains(FrameFlags::UDP) {
        return udp_relay(syn_frame, frame_reader, frame_writer).await;
    }

    // --- TCP path (unchanged) ---
    let target = match TargetAddr::decode(&syn_frame.payload) {
        Ok(t) => t,
        Err(_) => {
            let _ = frame_writer.write_frame(&Frame::rst(syn_frame.stream_id)).await;
            let _ = frame_writer.flush().await;
            return Ok(());
        }
    };

    tracing::info!("SYN → {} (stream={})", target, syn_frame.stream_id);

    let target_addr = match target.to_socket_addr().await {
        Ok(a) => a,
        Err(_) => {
            let _ = frame_writer.write_frame(&Frame::rst(syn_frame.stream_id)).await;
            let _ = frame_writer.flush().await;
            return Ok(());
        }
    };

    let target_stream = match TcpStream::connect(target_addr).await {
        Ok(s) => s,
        Err(e) => {
            tracing::info!("Connect failed → {}: {}", target, e);
            let _ = frame_writer.write_frame(&Frame::rst(syn_frame.stream_id)).await;
            let _ = frame_writer.flush().await;
            return Ok(());
        }
    };

    if frame_writer
        .write_frame(&Frame::ack(syn_frame.stream_id))
        .await
        .is_err()
    {
        return Ok(());
    }
    let _ = frame_writer.flush().await;

    relay(target_stream, frame_reader, frame_writer, syn_frame.stream_id, &target).await
}

/// UDP relay: SYN payload contains the first datagram target address + data.
/// Wire format for UDP SYN payload: [TargetAddr encoded][datagram bytes]
async fn udp_relay<R, W>(
    syn_frame: Frame,
    mut frame_reader: FrameReader<R>,
    mut frame_writer: FrameWriter<W>,
) -> Result<()>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin + Send + 'static,
{
    let stream_id = syn_frame.stream_id;

    // Parse target address from the beginning of the SYN payload.
    let (target, datagram) = match decode_udp_syn(&syn_frame.payload) {
        Some(t) => t,
        None => {
            let _ = frame_writer.write_frame(&Frame::rst(stream_id)).await;
            let _ = frame_writer.flush().await;
            return Ok(());
        }
    };

    let target_addr = match target.to_socket_addr().await {
        Ok(a) => a,
        Err(_) => {
            let _ = frame_writer.write_frame(&Frame::rst(stream_id)).await;
            let _ = frame_writer.flush().await;
            return Ok(());
        }
    };

    let socket = match UdpSocket::bind("0.0.0.0:0").await {
        Ok(s) => s,
        Err(_) => {
            let _ = frame_writer.write_frame(&Frame::rst(stream_id)).await;
            let _ = frame_writer.flush().await;
            return Ok(());
        }
    };

    // Send initial datagram.
    let _ = socket.send_to(datagram, target_addr).await;

    // ACK the SYN.
    if frame_writer.write_frame(&Frame::ack(stream_id)).await.is_err() {
        return Ok(());
    }
    let _ = frame_writer.flush().await;

    // Spawn a task to read UDP responses and send them back as UDP|DATA frames.
    let resp_socket = Arc::new(socket);
    let send_socket = Arc::clone(&resp_socket);
    let mut fw = frame_writer;
    let recv_handle = tokio::spawn(async move {
        let mut buf = vec![0u8; MAX_FRAME_PAYLOAD];
        loop {
            match resp_socket.recv_from(&mut buf).await {
                Ok((n, _peer)) => {
                    let data = Bytes::copy_from_slice(&buf[..n]);
                    let frame = Frame {
                        version: phantom_core::constants::PROTOCOL_VERSION,
                        stream_id,
                        flags: FrameFlags::UDP | FrameFlags::DATA,
                        payload: data,
                    };
                    if fw.write_frame(&frame).await.is_err() || fw.flush().await.is_err() {
                        break;
                    }
                }
                Err(_) => break,
            }
        }
    });

    // Read frames from client: UDP|DATA → send via socket, FIN → stop.
    loop {
        let frame = match frame_reader.read_frame().await {
            Ok(f) => f,
            Err(_) => break,
        };
        if frame.flags.contains(FrameFlags::DATA) && frame.flags.contains(FrameFlags::UDP) {
            let _ = send_socket.send_to(&frame.payload, target_addr).await;
        } else if frame.flags.contains(FrameFlags::FIN) || frame.flags.contains(FrameFlags::RST) {
            break;
        }
    }

    let _ = recv_handle.await;
    Ok(())
}

/// Decode a UDP SYN payload into (TargetAddr, remaining datagram bytes).
/// The TargetAddr is encoded first (variable length), followed by the raw datagram.
fn decode_udp_syn(payload: &[u8]) -> Option<(TargetAddr, &[u8])> {
    if payload.is_empty() {
        return None;
    }
    let atyp = payload[0];
    let (_addr_len, target_end) = match atyp {
        0x01 => (4 + 2, 1 + 4 + 2),  // IPv4: 4 bytes + 2 port
        0x04 => (16 + 2, 1 + 16 + 2), // IPv6: 16 bytes + 2 port
        0x03 => {
            if payload.len() < 2 {
                return None;
            }
            let domain_len = payload[1] as usize;
            (domain_len + 2, 1 + 1 + domain_len + 2)
        }
        _ => return None,
    };
    if payload.len() < target_end {
        return None;
    }
    let target = TargetAddr::decode(payload).ok()?;
    Some((target, &payload[target_end..]))
}

fn resolve_supported_ciphers(pref: CipherPreference) -> Vec<CipherSuite> {
    match pref {
        CipherPreference::Auto => CipherSuite::all_ordered().to_vec(),
        CipherPreference::Aes256Gcm => vec![CipherSuite::Aes256Gcm],
        CipherPreference::Aes128Gcm => vec![CipherSuite::Aes128Gcm],
        CipherPreference::Ascon128 => vec![CipherSuite::Ascon128],
        CipherPreference::ChaCha20Poly1305 => vec![CipherSuite::ChaCha20Poly],
    }
}

async fn relay<R, W>(
    target: TcpStream,
    mut frame_reader: FrameReader<R>,
    mut frame_writer: FrameWriter<W>,
    stream_id: u32,
    target_addr: &TargetAddr,
) -> Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let (mut target_read, mut target_write) = tokio::io::split(target);

    let target_clone = target_addr.clone();
    let to_tunnel = async {
        let mut buf = BytesMut::with_capacity(MAX_FRAME_PAYLOAD);
        let mut total_down: u64 = 0;
        loop {
            buf.clear();
            let n = target_read.read_buf(&mut buf).await.map_err(PhantomError::Io)?;
            if n == 0 {
                break;
            }
            total_down += n as u64;
            let data = buf.split().freeze();
            frame_writer
                .write_frame(&Frame::data(stream_id, data))
                .await?;
        }
        let _ = frame_writer.write_frame(&Frame::fin(stream_id)).await;
        let _ = frame_writer.flush().await;
        tracing::info!("Relay done ↓ {} ({} bytes down)", target_clone, total_down);
        Ok::<_, PhantomError>(())
    };

    let target_clone = target_addr.clone();
    let from_tunnel = async {
        let mut total_up: u64 = 0;
        loop {
            let frame = frame_reader.read_frame().await?;
            if frame.flags.contains(FrameFlags::DATA) {
                total_up += frame.payload.len() as u64;
                target_write.write_all(&frame.payload).await.map_err(PhantomError::Io)?;
            } else if frame.flags.contains(FrameFlags::FIN) || frame.flags.contains(FrameFlags::RST) {
                break;
            }
        }
        // Shutdown the write half to send TCP FIN to the target,
        // so the target knows no more data is coming and can close.
        let _ = target_write.shutdown().await;
        tracing::info!("Relay done ↑ {} ({} bytes up)", target_clone, total_up);
        Ok::<_, PhantomError>(())
    };

    tokio::try_join!(to_tunnel, from_tunnel)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decode_udp_syn_empty() {
        assert!(decode_udp_syn(&[]).is_none());
    }

    #[test]
    fn decode_udp_syn_ipv4_with_datagram() {
        // atyp=1 (IPv4), 4 bytes IP, 2 bytes port, then datagram
        let mut payload = vec![0x01]; // IPv4
        payload.extend_from_slice(&[192, 168, 1, 1]); // IP
        payload.extend_from_slice(&[0x00, 0x35]); // port 53
        payload.extend_from_slice(b"hello");
        let (target, datagram) = decode_udp_syn(&payload).unwrap();
        assert!(matches!(target, TargetAddr::IPv4(_, 53)));
        assert_eq!(datagram, b"hello");
    }

    #[test]
    fn decode_udp_syn_ipv4_empty_datagram() {
        let mut payload = vec![0x01]; // IPv4
        payload.extend_from_slice(&[8, 8, 8, 8]); // IP
        payload.extend_from_slice(&[0x00, 0x35]); // port 53
        let (target, datagram) = decode_udp_syn(&payload).unwrap();
        assert!(matches!(target, TargetAddr::IPv4(_, 53)));
        assert_eq!(datagram, b"");
    }

    #[test]
    fn decode_udp_syn_truncated_ipv4() {
        let payload = vec![0x01, 192, 168]; // only 2 bytes of IP
        assert!(decode_udp_syn(&payload).is_none());
    }

    #[test]
    fn decode_udp_syn_unsupported_atyp() {
        let payload = vec![0x05]; // unsupported
        assert!(decode_udp_syn(&payload).is_none());
    }

    #[test]
    fn decode_udp_syn_domain_with_data() {
        let mut payload = vec![0x03, 0x0B]; // domain, length 11
        payload.extend_from_slice(b"example.com");
        payload.extend_from_slice(&[0x01, 0xBB]); // port 443
        payload.extend_from_slice(b"data");
        let (target, datagram) = decode_udp_syn(&payload).unwrap();
        assert!(matches!(target, TargetAddr::Domain(_, 443)));
        assert_eq!(datagram, b"data");
    }
}
