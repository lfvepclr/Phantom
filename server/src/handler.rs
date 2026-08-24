use std::sync::Arc;

use bytes::{Bytes, BytesMut};
use phantom_core::CipherPreference;
use phantom_core::constants::{HANDSHAKE_TIMEOUT_SECS, MAX_FRAME_PAYLOAD};
use phantom_core::crypto::cipher::CipherSuite;
use phantom_core::crypto::{NoiseResponder, split_after_handshake};
use phantom_core::protocol::codec::{
    FrameReader, FrameWriter, MessageRead, MessageWrite, PlainMessageReader, PlainMessageWriter,
};
use phantom_core::protocol::frame::FrameFlags;
use phantom_core::protocol::{Frame, TargetAddr};
use phantom_core::{PhantomError, Result};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};

/// Handle a single TCP-like connection with full Noise handshake.
/// Backward compatible with v2 clients.
pub async fn handle_connection<S>(
    stream: S,
    secret_key: [u8; 32],
    psk: phantom_core::Psk,
    allowed_clients: &[[u8; 32]],
    cipher_preference: CipherPreference,
    verification_url: Option<&str>,
) where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let supported = resolve_supported_ciphers(cipher_preference);
    let result = match tokio::time::timeout(
        std::time::Duration::from_secs(HANDSHAKE_TIMEOUT_SECS),
        NoiseResponder::new(&secret_key, psk).handshake(stream, &supported),
    )
    .await
    {
        Ok(Ok(r)) => r,
        // Silently drop: a peer without the right PSK or key gets no signal.
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

    let _ = handle_frame_stream(frame_reader, frame_writer, verification_url).await;
}

/// Handle a multiplexed QUIC connection.
///
/// Authentication has already happened at connection level: the Noise
/// handshake inside QUIC bound the PSK and the server's static key before the
/// first stream could be opened. What remains is the whitelist check — the
/// client's static public key is recovered from the connection's peer
/// identity — and then every bi-stream runs the bare frame protocol. Each
/// stream gets its own task so a long-lived relay never head-of-line blocks
/// fresh streams on the same connection.
pub async fn handle_quic_connection(
    conn: quinn::Connection,
    allowed_clients: &[[u8; 32]],
    verification_url: Option<String>,
) {
    use phantom_core::transport::quic::{QuicStream, peer_static_key};

    if !allowed_clients.is_empty() {
        let allowed = peer_static_key(&conn)
            .map(|key| allowed_clients.contains(&key))
            .unwrap_or(false);
        if !allowed {
            tracing::info!("Rejected unauthorized QUIC client");
            conn.close(0u32.into(), b"unauthorized");
            return;
        }
    }

    tracing::info!("QUIC client connected");
    loop {
        let (send, recv) = match conn.accept_bi().await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("QUIC stream accept error: {}", e);
                break;
            }
        };
        let vurl = verification_url.clone();
        tokio::spawn(async move {
            let stream = QuicStream::new(send, recv);
            let (read_half, write_half) = tokio::io::split(stream);
            // QUIC streams carry plaintext frames: the Noise handshake inside
            // QUIC already encrypted the connection, so only length-prefix
            // framing remains.
            let frame_reader = FrameReader::new(PlainMessageReader::new(read_half));
            let frame_writer = FrameWriter::new(PlainMessageWriter::new(write_half));
            let _ = handle_frame_stream(frame_reader, frame_writer, vurl.as_deref()).await;
        });
    }
}

/// Common post-handshake logic: read SYN, connect target, relay.
/// Supports both TCP and UDP streams (UDP flagged via FrameFlags::UDP).
async fn handle_frame_stream<M, N>(
    mut frame_reader: FrameReader<M>,
    mut frame_writer: FrameWriter<N>,
    verification_url: Option<&str>,
) -> Result<()>
where
    M: MessageRead + Send + 'static,
    N: MessageWrite + Send + 'static,
{
    let first_frame = match frame_reader.read_frame().await {
        Ok(f) => f,
        _ => return Ok(()),
    };

    // Hello verification takes precedence: stream_id 0 + HELLO magic.
    if first_frame.stream_id == 0
        && first_frame
            .payload
            .starts_with(phantom_core::constants::HELLO_MAGIC)
    {
        return handle_hello(first_frame, frame_writer, verification_url).await;
    }

    // From here on we expect a normal SOCKS5 relay SYN.
    if !first_frame.flags.contains(FrameFlags::SYN) {
        return Ok(());
    }
    let syn_frame = first_frame;

    if syn_frame.flags.contains(FrameFlags::UDP) {
        return udp_relay(syn_frame, frame_reader, frame_writer).await;
    }

    // --- TCP path (unchanged) ---
    let target = match TargetAddr::decode(&syn_frame.payload) {
        Ok(t) => t,
        Err(_) => {
            let _ = frame_writer
                .write_frame(&Frame::rst(syn_frame.stream_id))
                .await;
            let _ = frame_writer.flush().await;
            return Ok(());
        }
    };

    tracing::info!("SYN → {} (stream={})", target, syn_frame.stream_id);

    let target_addr = match target.to_socket_addr().await {
        Ok(a) => a,
        Err(_) => {
            let _ = frame_writer
                .write_frame(&Frame::rst(syn_frame.stream_id))
                .await;
            let _ = frame_writer.flush().await;
            return Ok(());
        }
    };

    let target_stream = match TcpStream::connect(target_addr).await {
        Ok(s) => s,
        Err(e) => {
            tracing::info!("Connect failed → {}: {}", target, e);
            let _ = frame_writer
                .write_frame(&Frame::rst(syn_frame.stream_id))
                .await;
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

    relay(
        target_stream,
        frame_reader,
        frame_writer,
        syn_frame.stream_id,
        &target,
    )
    .await
}

/// UDP relay: SYN payload contains the first datagram target address + data.
/// Wire format for UDP SYN payload: [TargetAddr encoded][datagram bytes]
async fn udp_relay<M, N>(
    syn_frame: Frame,
    mut frame_reader: FrameReader<M>,
    mut frame_writer: FrameWriter<N>,
) -> Result<()>
where
    M: MessageRead + Send + 'static,
    N: MessageWrite + Send + 'static,
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
    if frame_writer
        .write_frame(&Frame::ack(stream_id))
        .await
        .is_err()
    {
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
        0x01 => (4 + 2, 1 + 4 + 2),   // IPv4: 4 bytes + 2 port
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

async fn relay<M, N>(
    target: TcpStream,
    mut frame_reader: FrameReader<M>,
    mut frame_writer: FrameWriter<N>,
    stream_id: u32,
    target_addr: &TargetAddr,
) -> Result<()>
where
    M: MessageRead,
    N: MessageWrite,
{
    let (mut target_read, mut target_write) = tokio::io::split(target);

    let target_clone = target_addr.clone();
    let to_tunnel = async {
        let mut buf = BytesMut::with_capacity(MAX_FRAME_PAYLOAD);
        let mut total_down: u64 = 0;
        loop {
            buf.clear();
            let n = target_read
                .read_buf(&mut buf)
                .await
                .map_err(PhantomError::Io)?;
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
                target_write
                    .write_all(&frame.payload)
                    .await
                    .map_err(PhantomError::Io)?;
            } else if frame.flags.contains(FrameFlags::FIN) || frame.flags.contains(FrameFlags::RST)
            {
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

/// Handle a Hello verification frame: probe the public internet and echo the
/// result back to the client inside an encrypted Hello-ACK frame.
async fn handle_hello<N>(
    frame: Frame,
    mut frame_writer: FrameWriter<N>,
    verification_url: Option<&str>,
) -> Result<()>
where
    N: MessageWrite + Send + 'static,
{
    use phantom_core::constants::{DEFAULT_HELLO_TARGETS, HELLO_ACK_MAGIC};

    // Probe-target precedence: client-supplied `hello.targets` (they ride in
    // the Hello payload) > server `verification_url` > built-in list. The
    // client is fully authenticated by this point and could open the same
    // connections through the relay anyway, so parsing its URLs adds no
    // attack surface; the count/length caps only keep the probe loop bounded.
    let client_targets = parse_hello_targets(&frame.payload);
    let targets: Vec<&str> = client_targets
        .iter()
        .map(String::as_str)
        .chain(verification_url.into_iter())
        .chain(DEFAULT_HELLO_TARGETS.iter().copied())
        .collect();

    let mut last_error = String::from("No verification target configured");
    let mut success = false;
    let mut message = String::new();

    for url in targets {
        match http_probe(url).await {
            Ok(body_snippet) => {
                success = true;
                message = format!("{} reachable ({})", url, body_snippet);
                break;
            }
            Err(e) => {
                last_error = format!("{} failed: {}", url, e);
            }
        }
    }

    if !success {
        message = last_error;
    }

    let ack = serde_json::json!({
        "ok": success,
        "message": message,
        "ts": std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0),
    });
    let mut payload = HELLO_ACK_MAGIC.to_vec();
    payload.extend_from_slice(ack.to_string().as_bytes());

    let _ = frame_writer.write_frame(&Frame::hello_ack(payload)).await;
    let _ = frame_writer.flush().await;

    tracing::info!(
        "Hello verification {}",
        if success { "passed" } else { "failed" }
    );
    Ok(())
}

/// Extract the optional `targets` array from a Hello frame payload.
/// Malformed JSON or a missing field simply yields an empty list, which
/// falls back to the server-side defaults.
fn parse_hello_targets(payload: &[u8]) -> Vec<String> {
    use phantom_core::constants::HELLO_MAGIC;

    const MAX_TARGETS: usize = 4;
    const MAX_URL_LEN: usize = 256;

    let Some(json) = payload.strip_prefix(HELLO_MAGIC) else {
        return Vec::new();
    };
    let Ok(body) = serde_json::from_slice::<serde_json::Value>(json) else {
        return Vec::new();
    };
    body.get("targets")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str())
                .filter(|s| s.starts_with("http://") && s.len() <= MAX_URL_LEN)
                .take(MAX_TARGETS)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

/// Tiny HTTP/1.0 GET probe. Returns a short body snippet on 200 OK.
async fn http_probe(url: &str) -> Result<String> {
    let url = url.strip_prefix("http://").unwrap_or(url);
    let (host_port, path) = url
        .split_once('/')
        .map(|(h, p)| (h, format!("/{}", p)))
        .unwrap_or((url, "/".to_string()));
    let (host, port) = host_port
        .split_once(':')
        .map(|(h, p)| (h, p.parse::<u16>().unwrap_or(80)))
        .unwrap_or((host_port, 80));

    let addr = match tokio::net::lookup_host(format!("{}:{}", host, port)).await {
        Ok(mut iter) => iter.next().ok_or_else(|| {
            PhantomError::Protocol("DNS lookup returned no addresses".to_string())
        })?,
        Err(e) => return Err(PhantomError::Io(e)),
    };

    let mut stream = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        tokio::net::TcpStream::connect(addr),
    )
    .await
    .map_err(|_| PhantomError::Timeout)??;

    let request = format!(
        "GET {} HTTP/1.0\r\nHost: {}\r\nUser-Agent: Phantom/1.0\r\nConnection: close\r\n\r\n",
        path, host
    );
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(PhantomError::Io)?;
    stream.flush().await.map_err(PhantomError::Io)?;

    let mut buf = vec![0u8; 4096];
    let n = tokio::time::timeout(std::time::Duration::from_secs(5), stream.read(&mut buf))
        .await
        .map_err(|_| PhantomError::Timeout)??;
    buf.truncate(n);

    let response = String::from_utf8_lossy(&buf);
    if !response.starts_with("HTTP/1.0 200") && !response.starts_with("HTTP/1.1 200") {
        return Err(PhantomError::Protocol(format!(
            "Unexpected response: {}",
            response.lines().next().unwrap_or("empty").to_string()
        )));
    }

    // Return the first line of the body as the snippet.
    let body = response.splitn(2, "\r\n\r\n").nth(1).unwrap_or("");
    let snippet = body.lines().next().unwrap_or("").to_string();
    if snippet.is_empty() {
        Ok("200 OK".to_string())
    } else {
        Ok(snippet)
    }
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

    fn hello_payload(body: &serde_json::Value) -> Vec<u8> {
        let mut buf = phantom_core::constants::HELLO_MAGIC.to_vec();
        buf.extend_from_slice(body.to_string().as_bytes());
        buf
    }

    #[test]
    fn hello_targets_parsed_from_payload() {
        let payload = hello_payload(&serde_json::json!({
            "nonce": "1",
            "targets": ["http://a.example/", "http://b.example/health"],
        }));
        assert_eq!(
            parse_hello_targets(&payload),
            vec![
                "http://a.example/".to_string(),
                "http://b.example/health".to_string()
            ]
        );
    }

    #[test]
    fn hello_targets_absent_or_malformed_yields_empty() {
        // No targets field.
        assert!(parse_hello_targets(&hello_payload(&serde_json::json!({"nonce": "1"}))).is_empty());
        // Garbage JSON after the magic.
        let mut buf = phantom_core::constants::HELLO_MAGIC.to_vec();
        buf.extend_from_slice(b"not json");
        assert!(parse_hello_targets(&buf).is_empty());
        // Wrong magic entirely.
        assert!(parse_hello_targets(b"whatever").is_empty());
    }

    #[test]
    fn hello_targets_filtered_and_capped() {
        let long_url = format!("http://{}/", "x".repeat(300));
        let payload = hello_payload(&serde_json::json!({
            "targets": [
                "https://skip.example/", // https is not probeable: filtered
                long_url,                 // over-long: filtered
                "http://1.example/",
                "http://2.example/",
                "http://3.example/",
                "http://4.example/",
                "http://5.example/", // beyond the cap: dropped
            ],
        }));
        let targets = parse_hello_targets(&payload);
        assert_eq!(targets.len(), 4);
        assert!(targets.iter().all(|t| t.starts_with("http://")));
        assert!(targets.iter().all(|t| !t.contains('5')));
    }
}
