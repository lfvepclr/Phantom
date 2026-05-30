pub mod handler;

#[cfg(target_os = "linux")]
pub mod linux_ext;

#[cfg(not(target_os = "linux"))]
use phantom_transport::TransportListener;

use anyhow::Result;
use phantom_core::ServerConfig;

pub async fn run(config_path: &str) -> Result<()> {
    let config = ServerConfig::load(config_path)?;
    init_tracing("info");

    tracing::info!("Loading key pair from {}", config.private_key);
    let (_public_key, secret_key) = config.load_key_pair()?;

    tracing::info!("Loading allowed clients from {}", config.clients);
    let allowed_clients = config.load_allowed_clients()?;
    if allowed_clients.is_empty() {
        tracing::warn!("No client public keys in whitelist — all Noise IK handshakes will fail");
    } else {
        tracing::info!("Loaded {} allowed client keys", allowed_clients.len());
    }

    let cipher_preference = config.cipher;
    let addr: std::net::SocketAddr = config.bind.parse()?;

    if config.quic.enable {
        run_quic(addr, secret_key, allowed_clients, cipher_preference, config.quic.congestion).await
    } else {
        run_tcp(addr, secret_key, allowed_clients, cipher_preference, config.performance.io_uring).await
    }
}

async fn run_tcp(
    addr: std::net::SocketAddr,
    secret_key: [u8; 32],
    allowed_clients: Vec<[u8; 32]>,
    cipher_preference: phantom_core::CipherPreference,
    _io_uring: bool,
) -> Result<()> {
    #[cfg(all(target_os = "linux", feature = "io-uring"))]
    if _io_uring {
        return linux_ext::run_uring_server(addr, secret_key, allowed_clients, cipher_preference).await;
    }

    #[cfg(target_os = "linux")]
    let listener = {
        let l = linux_ext::bind(&addr).await?;
        tracing::info!("Phantom TCP server listening on {}", l.local_addr()?);
        l
    };

    #[cfg(not(target_os = "linux"))]
    let listener = {
        use phantom_transport::tcp::TcpListener;
        use phantom_transport::TransportListener;
        let l = TcpListener::bind(&addr).await?;
        tracing::info!("Phantom TCP server listening on {}", l.local_addr()?);
        l
    };

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _peer)) => {
                        let sk = secret_key;
                        let allowed = allowed_clients.clone();
                        let cipher = cipher_preference;
                        tokio::spawn(async move {
                            handler::handle_connection(stream, sk, &allowed, cipher).await;
                        });
                    }
                    Err(e) => {
                        tracing::error!("Accept error: {}", e);
                    }
                }
            }
            _ = &mut shutdown => {
                tracing::info!("Shutting down");
                break;
            }
        }
    }

    Ok(())
}

async fn run_quic(
    addr: std::net::SocketAddr,
    secret_key: [u8; 32],
    allowed_clients: Vec<[u8; 32]>,
    cipher_preference: phantom_core::CipherPreference,
    congestion: phantom_core::CongestionAlgorithm,
) -> Result<()> {
    let server_config = phantom_transport::quic::create_server_config(congestion)?;
    let endpoint = quinn::Endpoint::server(server_config, addr)?;
    tracing::info!("Phantom QUIC server listening on {}", endpoint.local_addr()?);

    let shutdown = tokio::signal::ctrl_c();
    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            incoming = endpoint.accept() => {
                match incoming {
                    Some(incoming) => {
                        let conn = match incoming.await {
                            Ok(c) => c,
                            Err(e) => {
                                tracing::debug!("QUIC connection failed: {}", e);
                                continue;
                            }
                        };
                        let sk = secret_key;
                        let allowed = allowed_clients.clone();
                        let cipher = cipher_preference;
                        tokio::spawn(async move {
                            handler::handle_quic_connection(conn, sk, &allowed, cipher).await;
                        });
                    }
                    None => {
                        tracing::info!("QUIC endpoint closed");
                        break;
                    }
                }
            }
            _ = &mut shutdown => {
                tracing::info!("Shutting down");
                break;
            }
        }
    }

    Ok(())
}

fn init_tracing(level: &str) {
    tracing_subscriber::fmt()
        .with_env_filter(level)
        .init();
}
