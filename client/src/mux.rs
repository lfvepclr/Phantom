//! QUIC connection multiplexing with Noise connection-level key reuse.
//!
//! `MuxSession` maintains a single QUIC connection to a Phantom server.
//! The first bi-stream performs the full Noise IK handshake; all subsequent
//! bi-streams derive their session keys via HKDF from the parent connection
//! keys using the implicit stream counter (1, 2, 3 …).

use async_trait::async_trait;
use phantom_core::crypto::cipher::CipherSuite;
use phantom_core::crypto::session::CipherOffer;
use phantom_core::crypto::{NoiseInitiator, split_after_handshake, split_for_stream};
use phantom_core::transport::Transport;
use phantom_core::transport::quic::{QuicStream, create_client_endpoint};
use phantom_core::{CipherPreference, PhantomError, Result};
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::Mutex;

/// State of a multiplexed QUIC session.
pub struct MuxSession {
    conn: Arc<Mutex<quinn::Connection>>,
    state: Arc<Mutex<MuxState>>,
}

enum MuxState {
    HandshakePending {
        local_secret: [u8; 32],
        remote_public: [u8; 32],
        cipher_preference: CipherPreference,
    },
    HandshakeDone {
        split_keys: ([u8; 32], [u8; 32]),
        cipher: CipherSuite,
        next_stream_id: u32,
    },
}

/// Aliases for the encrypted stream halves returned by `MuxSession::open_stream`.
pub type EncryptedReader = phantom_core::crypto::SessionReader<tokio::io::ReadHalf<QuicStream>>;
pub type EncryptedWriter = phantom_core::crypto::SessionWriter<tokio::io::WriteHalf<QuicStream>>;

impl MuxSession {
    /// Establish a new QUIC connection and prepare for Noise authentication.
    pub async fn connect_with_auth(
        addr: SocketAddr,
        server_name: &str,
        local_secret: [u8; 32],
        remote_public: [u8; 32],
        cipher_preference: CipherPreference,
    ) -> Result<Self> {
        let endpoint = create_client_endpoint(Default::default())?;
        let connecting = endpoint
            .connect(addr, server_name)
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        let conn = connecting.await.map_err(|e| {
            PhantomError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                e,
            ))
        })?;
        Ok(Self {
            conn: Arc::new(Mutex::new(conn)),
            state: Arc::new(Mutex::new(MuxState::HandshakePending {
                local_secret,
                remote_public,
                cipher_preference,
            })),
        })
    }

    /// Open a new encrypted bi-directional stream.
    ///
    /// The first call performs the Noise IK handshake.  Subsequent calls
    /// derive session keys via HKDF and are therefore much faster.
    pub async fn open_stream(&self) -> Result<(EncryptedReader, EncryptedWriter)> {
        let (send, recv) = self
            .conn
            .lock()
            .await
            .open_bi()
            .await
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        let quic_stream = QuicStream::new(send, recv);

        let mut state_guard = self.state.lock().await;
        match &mut *state_guard {
            MuxState::HandshakePending {
                local_secret,
                remote_public,
                cipher_preference,
            } => {
                let offer = resolve_offer(*cipher_preference);
                let initiator = NoiseInitiator::new(local_secret, remote_public);
                let result = initiator.handshake(quic_stream, &offer).await?;

                let (session_reader, session_writer) = split_after_handshake(
                    result.stream,
                    result.split_keys,
                    result.chosen_cipher,
                    result.is_initiator,
                );

                *state_guard = MuxState::HandshakeDone {
                    split_keys: result.split_keys,
                    cipher: result.chosen_cipher,
                    next_stream_id: 2,
                };

                Ok((session_reader, session_writer))
            }
            MuxState::HandshakeDone {
                split_keys,
                cipher,
                next_stream_id,
            } => {
                let stream_id = *next_stream_id;
                *next_stream_id += 1;
                let (session_reader, session_writer) =
                    split_for_stream(quic_stream, split_keys, *cipher, true, stream_id);
                Ok((session_reader, session_writer))
            }
        }
    }

    /// Close the QUIC connection gracefully.
    pub async fn close(&self) {
        let conn = self.conn.lock().await;
        conn.close(0u32.into(), b"client close");
    }
}

fn resolve_offer(cipher_preference: CipherPreference) -> CipherOffer {
    use phantom_core::crypto::cipher::CipherSuite;
    match cipher_preference {
        CipherPreference::Auto => CipherOffer::default_offer(),
        CipherPreference::Aes256Gcm => CipherOffer::new(vec![CipherSuite::Aes256Gcm]),
        CipherPreference::Aes128Gcm => CipherOffer::new(vec![CipherSuite::Aes128Gcm]),
        CipherPreference::Ascon128 => CipherOffer::new(vec![CipherSuite::Ascon128]),
        CipherPreference::ChaCha20Poly1305 => CipherOffer::new(vec![CipherSuite::ChaCha20Poly]),
    }
}

/// A `Transport` implementation backed by a multiplexed session.
///
/// **Note**: This transport returns raw `QuicStream`s without encryption.
/// For connection-level multiplexing with shared Noise state, use
/// `MuxSession::open_stream()` directly.
pub struct MuxTransport {
    session: Arc<MuxSession>,
}

impl MuxTransport {
    pub fn new(session: Arc<MuxSession>) -> Self {
        Self { session }
    }
}

#[async_trait]
impl Transport for MuxTransport {
    type Stream = QuicStream;

    async fn connect(&self, _addr: &SocketAddr) -> Result<Self::Stream> {
        let (send, recv) = self
            .session
            .conn
            .lock()
            .await
            .open_bi()
            .await
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;
        Ok(QuicStream::new(send, recv))
    }

    fn name(&self) -> &str {
        "quic-mux"
    }
}
