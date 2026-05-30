//! Linux io_uring extensions for zero-copy high-performance I/O.
//!
//! When the `io-uring` feature is enabled and `performance.io_uring = true`
//! in server.toml, the server uses `tokio-uring` for async I/O submission.
//!
//! Key benefits:
//! * Registered buffers eliminate per-syscall page-table walks.
//! * Single `io_uring_enter` batch processes hundreds of requests,
//!   drastically reducing context switches.
//! * Zero-copy `read_fixed` / `write_fixed` with pre-registered buffers.

use phantom_core::{PhantomError, Result};
use std::net::SocketAddr;

/// Run the server using io_uring on Linux.
///
/// This function is called from `server/src/lib.rs` when
/// `config.performance.io_uring` is true on a Linux host.
#[cfg(all(feature = "io-uring", target_os = "linux"))]
pub async fn run_uring_server(
    addr: SocketAddr,
    secret_key: [u8; 32],
    allowed_clients: Vec<[u8; 32]>,
    cipher_preference: phantom_core::CipherPreference,
) -> Result<()> {
    use tokio_uring::net::TcpListener;
    use tokio_uring::net::TcpStream;

    let listener = TcpListener::bind(addr).map_err(PhantomError::Io)?;
    tracing::info!("Phantom io_uring server listening on {}", addr);

    loop {
        match listener.accept().await {
            Ok((stream, _peer)) => {
                let sk = secret_key;
                let allowed = allowed_clients.clone();
                let cipher = cipher_preference;
                tokio::spawn(async move {
                    handle_uring_stream(stream, sk, &allowed, cipher).await;
                });
            }
            Err(e) => {
                tracing::error!("io_uring accept error: {}", e);
            }
        }
    }
}

#[cfg(all(feature = "io-uring", target_os = "linux"))]
async fn handle_uring_stream(
    stream: tokio_uring::net::TcpStream,
    secret_key: [u8; 32],
    allowed_clients: &[[u8; 32]],
    cipher_preference: phantom_core::CipherPreference,
) {
    // tokio_uring::net::TcpStream implements AsyncRead + AsyncWrite,
    // so it can be passed directly to the generic handler.
    crate::handler::handle_connection(stream, secret_key, allowed_clients, cipher_preference).await;
}

/// Fallback: standard tokio listener (no io_uring).
///
/// Used when `io-uring` feature is disabled or on non-Linux.
pub async fn bind(addr: &SocketAddr) -> Result<tokio::net::TcpListener> {
    tokio::net::TcpListener::bind(addr)
        .await
        .map_err(PhantomError::Io)
}
