//! Client-side QUIC connection pool.
//!
//! A Noise-authenticated QUIC connection is expensive relative to the streams
//! it carries, so the client keeps one connection per server address and
//! multiplexes every tunnel over it. `OnceCell` deduplicates concurrent first
//! connects; a connection whose `close_reason` is set (or whose connect
//! attempt failed) is evicted and rebuilt on the next request.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use phantom_core::transport::quic::{QuicAuth, create_client_endpoint};
use phantom_core::{CipherPreference, PhantomError, QuicConfig, Result, ServerEntry};
use tokio::sync::{Mutex, OnceCell};

/// Per-server cache of established QUIC connections.
#[derive(Default)]
pub struct QuicPool {
    entries: Mutex<HashMap<String, Arc<OnceCell<quinn::Connection>>>>,
}

impl QuicPool {
    pub fn new() -> Self {
        Self::default()
    }

    /// Drop every cached connection.
    ///
    /// Called when the OS reports the underlying network changed (Wi-Fi ⇄
    /// cellular): a QUIC connection is bound to the old source address, so
    /// reusing it would keep timing out instead of reconnecting on the new
    /// link. Tearing the pool down makes the next stream build a fresh one.
    pub async fn clear(&self) {
        let mut guard = self.entries.lock().await;
        if !guard.is_empty() {
            tracing::info!("QUIC pool cleared ({} connection(s))", guard.len());
            guard.clear();
        }
    }

    /// Open a fresh bi-directional stream on the pooled connection for
    /// `server`, connecting (or reconnecting after a close) as needed.
    pub async fn open_bi(
        &self,
        server: &ServerEntry,
        local_secret: &[u8; 32],
        cipher: CipherPreference,
        connect_timeout: Duration,
    ) -> Result<(quinn::SendStream, quinn::RecvStream)> {
        let conn = self
            .connection(server, local_secret, cipher, connect_timeout)
            .await?;
        conn.open_bi()
            .await
            .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))
    }

    async fn connection(
        &self,
        server: &ServerEntry,
        local_secret: &[u8; 32],
        cipher: CipherPreference,
        connect_timeout: Duration,
    ) -> Result<quinn::Connection> {
        let key = server.address.clone();
        loop {
            let cell = {
                let mut guard = self.entries.lock().await;
                guard
                    .entry(key.clone())
                    .or_insert_with(|| Arc::new(OnceCell::new()))
                    .clone()
            };

            match cell
                .get_or_try_init(|| async {
                    connect_once(server, local_secret, cipher, connect_timeout).await
                })
                .await
            {
                Ok(conn) if conn.close_reason().is_none() => return Ok(conn.clone()),
                Ok(_) => {
                    // Established earlier but already closed (idle timeout,
                    // network flap, server restart): evict and rebuild.
                    self.evict_if_same(&key, &cell).await;
                }
                Err(e) => {
                    // Never cache a failed connect — otherwise one transient
                    // failure would poison the pool permanently.
                    self.evict_if_same(&key, &cell).await;
                    return Err(e);
                }
            }
        }
    }

    async fn evict_if_same(&self, key: &str, cell: &Arc<OnceCell<quinn::Connection>>) {
        let mut guard = self.entries.lock().await;
        if guard.get(key).is_some_and(|c| Arc::ptr_eq(c, cell)) {
            guard.remove(key);
        }
    }
}

/// Establish one Noise-authenticated QUIC connection to `server`.
///
/// Shared by the pool (cached path) and the startup hello check (one-shot
/// path).
pub async fn connect_once(
    server: &ServerEntry,
    local_secret: &[u8; 32],
    cipher: CipherPreference,
    connect_timeout: Duration,
) -> Result<quinn::Connection> {
    let addr: std::net::SocketAddr = server
        .address
        .parse()
        .map_err(|e| PhantomError::Config(format!("Invalid server address: {}", e)))?;
    let remote_public = decode_public_key(&server.public_key)?;
    let auth = QuicAuth::client(*local_secret, remote_public, server.decode_psk()?, cipher);
    let endpoint = create_client_endpoint(&auth, &QuicConfig::default())?;

    // The server name is unused by the Noise handshake (the remote static key
    // is pinned in `auth`), but quinn's API still requires one.
    let connecting = endpoint
        .connect(addr, "phantom")
        .map_err(|e| PhantomError::Io(std::io::Error::new(std::io::ErrorKind::Other, e)))?;

    tokio::time::timeout(connect_timeout, connecting)
        .await
        .map_err(|_| PhantomError::Timeout)?
        .map_err(|e| {
            PhantomError::Io(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                e,
            ))
        })
}

/// Decode a base64 server public key. Shared by every QUIC entry point now
/// that the key feeds the Noise handshake directly.
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
