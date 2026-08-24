//! Hello/Hello-ACK verification handshake.
//!
//! Before the client starts accepting SOCKS5 traffic, it opens a single
//! encrypted control connection to the configured Phantom server, sends a
//! Hello frame, and waits for the server to prove that it can reach the
//! public internet.  This prevents the UI from showing "Connected" when
//! only the local SOCKS5 listener is up.

use base64::{Engine, engine::general_purpose::STANDARD};
use phantom_core::constants::{HELLO_ACK_MAGIC, HELLO_MAGIC};
use phantom_core::crypto::{NoiseInitiator, split_after_handshake};
use phantom_core::protocol::Frame;
use phantom_core::protocol::codec::{
    FrameReader, FrameWriter, MessageRead, MessageWrite, PlainMessageReader, PlainMessageWriter,
};
use phantom_core::transport::Transport;
use phantom_core::transport::tcp::TcpTransport;
use phantom_core::{ClientConfig, PhantomError, Result, ServerEntry};
use std::time::{Duration, Instant};

/// Outcome of a Hello verification attempt.
#[derive(Debug, Clone)]
pub struct HelloResult {
    pub success: bool,
    pub message: String,
    pub latency_ms: u64,
}

/// Verify client→server→internet connectivity.
///
/// Selects the first configured server, performs a full Noise handshake over
/// the configured transport, sends a Hello frame, and waits for the server's
/// Hello-ACK.  Returns an error only for local failures (bad config, handshake
/// timeout, etc.); a server-side internet failure is represented by
/// `HelloResult { success: false, ... }`.
pub async fn verify_server_connection(config: &ClientConfig) -> Result<HelloResult> {
    let server = config
        .servers
        .first()
        .ok_or(PhantomError::AllServersFailed)?;

    let local_secret = phantom_core::crypto::KeyPair::generate()
        .map_err(|e| PhantomError::Crypto(format!("Key generation failed: {}", e)))?
        .secret;

    let started = Instant::now();

    let result = match server.protocol {
        phantom_core::TransportProtocol::Tcp => {
            let transport = TcpTransport::new(Duration::from_secs(10));
            verify_over_transport(&transport, server, &local_secret, config).await?
        }
        phantom_core::TransportProtocol::Quic => {
            verify_over_quic(server, &local_secret, config).await?
        }
    };

    Ok(HelloResult {
        success: result.success,
        message: result.message,
        latency_ms: started.elapsed().as_millis() as u64,
    })
}

/// QUIC hello: the connection from `connect_once` is already
/// Noise-authenticated at connection level, so the hello exchange runs on a
/// bare plaintext-framed stream.
async fn verify_over_quic(
    server: &ServerEntry,
    local_secret: &[u8; 32],
    config: &ClientConfig,
) -> Result<HelloResult> {
    let conn = crate::quic_pool::connect_once(
        server,
        local_secret,
        phantom_core::CipherPreference::effective_for(server.cipher, config.client.cipher),
        Duration::from_secs(10),
    )
    .await?;
    let (send, recv) = conn
        .open_bi()
        .await
        .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
    let stream = phantom_core::transport::quic::QuicStream::new(send, recv);
    let (read_half, write_half) = tokio::io::split(stream);
    let frame_reader = FrameReader::new(PlainMessageReader::new(read_half));
    let frame_writer = FrameWriter::new(PlainMessageWriter::new(write_half));
    hello_exchange(frame_reader, frame_writer, config).await
}

async fn verify_over_transport<T: Transport>(
    transport: &T,
    server: &ServerEntry,
    local_secret: &[u8; 32],
    config: &ClientConfig,
) -> Result<HelloResult> {
    let addr: std::net::SocketAddr = server
        .address
        .parse()
        .map_err(|e| PhantomError::Config(format!("Invalid server address: {}", e)))?;

    let stream = transport.connect(&addr).await?;

    let remote_public = decode_public_key(&server.public_key)?;
    let initiator = NoiseInitiator::new(local_secret, &remote_public, server.decode_psk()?);
    let offer = crate::socks5::resolve_offer(phantom_core::CipherPreference::effective_for(
        server.cipher,
        config.client.cipher,
    ));
    let result = initiator.handshake(stream, &offer).await?;

    let (session_reader, session_writer) = split_after_handshake(
        result.stream,
        result.split_keys,
        result.chosen_cipher,
        result.is_initiator,
    );

    let frame_reader = FrameReader::new(session_reader);
    let frame_writer = FrameWriter::new(session_writer);

    hello_exchange(frame_reader, frame_writer, config).await
}

/// The HELLO/HELLO-ACK frame exchange, identical on both transports — only
/// the message framing underneath differs.
async fn hello_exchange<M: MessageRead, N: MessageWrite>(
    mut frame_reader: FrameReader<M>,
    mut frame_writer: FrameWriter<N>,
    config: &ClientConfig,
) -> Result<HelloResult> {
    let nonce = format!("{}", Instant::now().elapsed().as_nanos());
    let hello_payload = {
        let mut buf = HELLO_MAGIC.to_vec();
        // `hello.targets` ride along so the server probes what the operator
        // actually cares about; the server falls back to its own
        // verification_url / built-in list when this is absent.
        let mut body = serde_json::json!({ "nonce": nonce });
        if !config.hello.targets.is_empty() {
            body["targets"] = serde_json::json!(config.hello.targets);
        }
        buf.extend_from_slice(body.to_string().as_bytes());
        buf
    };

    frame_writer
        .write_frame(&Frame::hello(hello_payload))
        .await?;
    frame_writer.flush().await?;

    let ack = tokio::time::timeout(
        Duration::from_secs(config.hello.timeout),
        frame_reader.read_frame(),
    )
    .await
    .map_err(|_| PhantomError::Timeout)?
    .map_err(|e| PhantomError::Protocol(format!("Failed to read Hello-ACK: {}", e)))?;

    if ack.stream_id != 0 || !ack.payload.starts_with(HELLO_ACK_MAGIC) {
        return Err(PhantomError::Protocol(
            "Invalid Hello-ACK frame".to_string(),
        ));
    }

    let json_bytes = &ack.payload[HELLO_ACK_MAGIC.len()..];
    let result: serde_json::Value = serde_json::from_slice(json_bytes)
        .map_err(|e| PhantomError::Protocol(format!("Malformed Hello-ACK payload: {}", e)))?;

    let success = result.get("ok").and_then(|v| v.as_bool()).unwrap_or(false);
    let message = result
        .get("message")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    Ok(HelloResult {
        success,
        message,
        latency_ms: 0,
    })
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
