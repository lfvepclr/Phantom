//! Shared UDP-over-tunnel plumbing.
//!
//! Used by both the TUN UDP proxy and the SOCKS5 UDP ASSOCIATE handler.
//!
//! Wire format (server side: `server/src/handler.rs::udp_relay`):
//! - `SYN|UDP|DATA` frame: payload = `TargetAddr::encode()` ++ first datagram
//! - `UDP|DATA` frames carry raw datagram bytes in both directions
//! - `FIN`/`RST` tears the flow down

use base64::{Engine, engine::general_purpose::STANDARD};
use bytes::Bytes;
use phantom_core::constants::PROTOCOL_VERSION;
use phantom_core::crypto::{NoiseInitiator, split_after_handshake};
use phantom_core::protocol::codec::{
    FrameReader, FrameWriter, MessageRead, MessageWrite, PlainMessageReader, PlainMessageWriter,
};
use phantom_core::protocol::frame::FrameFlags;
use phantom_core::protocol::{Frame, TargetAddr};
use phantom_core::transport::Transport;
use phantom_core::transport::quic::QuicStream;
use phantom_core::transport::tcp::TcpTransport;
use phantom_core::{CipherPreference, PhantomError, Result, ServerEntry};
use std::time::Duration;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::quic_pool::QuicPool;

/// Caller-facing channels of one tunnelled UDP flow.
pub struct UdpFlowChannels {
    /// Datagrams to deliver to the flow's target.
    pub outbound: UnboundedSender<Vec<u8>>,
    /// Datagrams received from the flow's target.
    pub inbound: UnboundedReceiver<Vec<u8>>,
}

/// Establish a UDP flow over the TCP transport (full Noise session) and spawn
/// the frame pump. `first_datagram` rides inside the SYN payload so the first
/// packet costs no extra round trip.
pub async fn establish_udp_flow_tcp(
    server: &ServerEntry,
    local_secret: &[u8; 32],
    target: TargetAddr,
    first_datagram: Vec<u8>,
) -> Result<UdpFlowChannels> {
    let addr: std::net::SocketAddr = server
        .address
        .parse()
        .map_err(|e| PhantomError::Config(format!("Invalid server address: {}", e)))?;

    let transport = TcpTransport::new(Duration::from_secs(10));
    let stream = transport.connect(&addr).await?;

    let remote_public = decode_public_key(&server.public_key)?;
    let initiator = NoiseInitiator::new(local_secret, &remote_public, server.decode_psk()?);
    let offer = crate::socks5::resolve_offer(server.cipher);
    let result = initiator.handshake(stream, &offer).await?;

    let (session_reader, session_writer) = split_after_handshake(
        result.stream,
        result.split_keys,
        result.chosen_cipher,
        result.is_initiator,
    );
    let mut frame_reader = FrameReader::new(session_reader);
    let mut frame_writer = FrameWriter::new(session_writer);

    let stream_id =
        udp_syn_handshake(&mut frame_reader, &mut frame_writer, target, first_datagram).await?;
    Ok(spawn_udp_frame_pump(frame_reader, frame_writer, stream_id))
}

/// QUIC variant: the pooled connection is already Noise-authenticated, so the
/// flow runs the bare frame protocol on a fresh bi-stream.
pub async fn establish_udp_flow_quic(
    pool: &QuicPool,
    server: &ServerEntry,
    local_secret: &[u8; 32],
    cipher: CipherPreference,
    target: TargetAddr,
    first_datagram: Vec<u8>,
) -> Result<UdpFlowChannels> {
    let (send, recv) = pool
        .open_bi(server, local_secret, cipher, Duration::from_secs(10))
        .await?;
    let stream = QuicStream::new(send, recv);
    let (read_half, write_half) = tokio::io::split(stream);
    let mut frame_reader = FrameReader::new(PlainMessageReader::new(read_half));
    let mut frame_writer = FrameWriter::new(PlainMessageWriter::new(write_half));

    let stream_id =
        udp_syn_handshake(&mut frame_reader, &mut frame_writer, target, first_datagram).await?;
    Ok(spawn_udp_frame_pump(frame_reader, frame_writer, stream_id))
}

/// Send the UDP SYN (target + first datagram) and wait for the server's ACK.
async fn udp_syn_handshake<M: MessageRead, N: MessageWrite>(
    frame_reader: &mut FrameReader<M>,
    frame_writer: &mut FrameWriter<N>,
    target: TargetAddr,
    first_datagram: Vec<u8>,
) -> Result<u32> {
    let stream_id: u32 = 1;
    let mut syn_payload = target.encode().to_vec();
    syn_payload.extend_from_slice(&first_datagram);
    let syn_frame = Frame {
        version: PROTOCOL_VERSION,
        stream_id,
        flags: FrameFlags::SYN | FrameFlags::UDP | FrameFlags::DATA,
        payload: Bytes::from(syn_payload),
    };
    frame_writer.write_frame(&syn_frame).await?;
    frame_writer.flush().await?;

    let ack = frame_reader.read_frame().await?;
    if ack.flags.contains(FrameFlags::RST) {
        return Err(PhantomError::Protocol("UDP SYN rejected".to_string()));
    }
    if !ack.flags.contains(FrameFlags::ACK) {
        return Err(PhantomError::Protocol(
            "Expected ACK for UDP SYN".to_string(),
        ));
    }
    Ok(stream_id)
}

/// Spawn the bidirectional pump and return the caller-facing channels.
///
/// The pump ends as soon as either direction does: the caller closing
/// `outbound` (or a write failure) stops the reader, and a server `FIN`/`RST`
/// (or a read failure) stops the writer. UDP is lossy, so late datagrams are
/// not drained on teardown.
pub fn spawn_udp_frame_pump<M, N>(
    mut frame_reader: FrameReader<M>,
    mut frame_writer: FrameWriter<N>,
    stream_id: u32,
) -> UdpFlowChannels
where
    M: MessageRead + Send + 'static,
    N: MessageWrite + Send + 'static,
{
    let (outbound_tx, mut outbound_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();
    let (inbound_tx, inbound_rx) = tokio::sync::mpsc::unbounded_channel::<Vec<u8>>();

    tokio::spawn(async move {
        let writer = async {
            while let Some(data) = outbound_rx.recv().await {
                let frame = Frame {
                    version: PROTOCOL_VERSION,
                    stream_id,
                    flags: FrameFlags::UDP | FrameFlags::DATA,
                    payload: Bytes::from(data),
                };
                if frame_writer.write_frame(&frame).await.is_err() {
                    break;
                }
                if frame_writer.flush().await.is_err() {
                    break;
                }
            }
            let _ = frame_writer.write_frame(&Frame::fin(stream_id)).await;
            let _ = frame_writer.flush().await;
        };

        let reader = async {
            loop {
                let frame = match frame_reader.read_frame().await {
                    Ok(f) => f,
                    Err(_) => break,
                };
                if frame.flags.contains(FrameFlags::DATA) && frame.flags.contains(FrameFlags::UDP) {
                    if inbound_tx.send(frame.payload.to_vec()).is_err() {
                        break;
                    }
                } else if frame.flags.contains(FrameFlags::FIN)
                    || frame.flags.contains(FrameFlags::RST)
                {
                    break;
                }
            }
            // Drop `inbound_tx` so the caller's recv() observes the end.
        };

        tokio::select! {
            _ = writer => {}
            _ = reader => {}
        }
    });

    UdpFlowChannels {
        outbound: outbound_tx,
        inbound: inbound_rx,
    }
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
