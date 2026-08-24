pub mod handler;

#[cfg(target_os = "linux")]
pub mod linux_ext;

// Enabling `io-uring` on a musl target is a no-op (see server/Cargo.toml);
// surface that at compile time instead of silently running epoll.
#[cfg(all(target_os = "linux", not(target_env = "gnu"), feature = "io-uring"))]
const _: () = {
    #[deprecated(
        note = "the io-uring feature has no effect on musl targets; the epoll runtime is used instead"
    )]
    const IO_URING_ON_MUSL: () = ();
    let _ = IO_URING_ON_MUSL;
};

pub mod bootstrap;

#[cfg(not(target_os = "linux"))]
use phantom_core::transport::TransportListener;

use anyhow::Result;
use base64::Engine;
use phantom_core::{CipherPreference, ServerConfig, ServerEntry, TransportProtocol};
use std::net::SocketAddr;

/// All-in-one runtime configuration for the server, regardless of how the
/// caller gathered the parameters (TOML, URI bootstrap, or programmatic).
///
/// `bind` MUST be the actual bound address — callers (e.g. `bootstrap`) are
/// responsible for port probing before constructing this struct, so that the
/// `phantom://` URI quick-link written into `server.toml` by bootstrap always
/// references a port that the server is actually listening on.
#[derive(Debug, Clone)]
pub struct BootstrapOptions {
    pub bind: SocketAddr,
    pub secret_key: [u8; 32],
    /// Pre-shared key required by the Noise handshake. Peers without it are
    /// rejected on the first handshake message.
    pub psk: phantom_core::Psk,
    pub allowed_clients: Vec<[u8; 32]>,
    pub cipher: CipherPreference,
    pub protocol: TransportProtocol,
    /// Full `[quic]` section: congestion, stream cap, keep-alive.
    pub quic_config: phantom_core::QuicConfig,
    pub io_uring: bool,
    /// Optional URL used for the Hello verification probe. When `None`, the
    /// built-in captive-portal targets are used.
    pub verification_url: Option<String>,
}

/// Load a TOML config and dispatch to the unified runtime.
///
/// This is the entry point used by the `phantom-server` binary and by
/// `phantom server -c <file>`. Behavior is unchanged from before the
/// bootstrap refactor: it loads keys + clients from disk, then runs the
/// Noise listener.
pub async fn run(config_path: &str) -> Result<()> {
    let config = ServerConfig::load(config_path)?;

    tracing::info!("Loading key pair from {}", config.private_key);
    // Loads the PSK from line 3 of the key file, minting one if the file
    // predates PSK support.
    let identity = phantom_core::KeyPair::load_server_identity(&config.private_key)?;
    let public_key = identity.keys.public;
    let secret_key = identity.keys.secret;
    if identity.psk_generated {
        tracing::warn!(
            "{} had no PSK; one was generated and appended. Previously \
             distributed URIs no longer work — re-distribute the URI below.",
            config.private_key
        );
    }

    tracing::info!("Loading allowed clients from {:?}", config.clients);
    let allowed_clients = config.load_allowed_clients()?;
    if allowed_clients.is_empty() {
        tracing::warn!("No client public keys in whitelist — all Noise IK handshakes will fail");
    } else {
        tracing::info!("Loaded {} allowed client keys", allowed_clients.len());
    }

    let bind: SocketAddr = config.bind.parse()?;
    let protocol = if config.quic.enable {
        TransportProtocol::Quic
    } else {
        TransportProtocol::Tcp
    };
    let io_uring = config.performance.io_uring;

    if protocol == TransportProtocol::Quic && io_uring {
        tracing::warn!("io_uring is ignored when running over QUIC");
    }

    // Synthesize the same quick-link URI auto-bootstrap emits and print it
    // alongside its QR code, so operators running in load mode (systemd / CI)
    // can also scan-and-share from the terminal.
    // `load_key_pair` returns raw 32 bytes; `build_phantom_uri` needs a base64
    // string, so encode locally rather than re-loading the keypair struct.
    let public_key_b64 = phantom_core::B64.encode(&public_key);
    let uri = phantom_core::build_phantom_uri(
        &public_key_b64,
        &config.bind,
        &identity.psk.to_base64(),
        config.cipher,
        protocol,
        Some("default"),
    );
    println!();
    println!("=== Phantom server bootstrapped (load mode) ===");
    println!("  config file  : {}", config_path);
    println!("  bind address : {}", bind);
    println!("  URI link     : {}", uri);
    println!();
    println!("Scan the QR code below with a Phantom client to import the URI:");
    crate::bootstrap::print_qr_code(&uri);
    println!();
    println!("Or copy it manually:");
    println!("  phantom client --server \"{}\"", uri);
    println!();

    let opts = BootstrapOptions {
        bind,
        secret_key,
        psk: identity.psk.clone(),
        allowed_clients,
        cipher: config.cipher,
        protocol,
        quic_config: config.quic.clone(),
        io_uring,
        verification_url: config.verification_url.clone(),
    };

    run_with_options(opts).await
}

/// Run the Noise listener with an already-constructed [`BootstrapOptions`].
///
/// This is the unified runtime that all entry points funnel into. It owns
/// the TCP/QUIC dispatch and the accept loop. The server stops on Ctrl-C;
/// embedders that need programmatic shutdown should use
/// [`run_with_shutdown`] instead.
pub async fn run_with_options(opts: BootstrapOptions) -> Result<()> {
    run_with_shutdown(opts, async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await
}

/// Like [`run_with_options`], but the accept loop also terminates when the
/// supplied `shutdown` future resolves — whichever comes first. Used by the
/// HarmonyOS embedded server, where there is no Ctrl-C and the UI stops the
/// server via a oneshot channel.
///
/// Note: the io_uring runtime (`linux_ext::run_uring_server`) does not yet
/// honour external shutdown; embedded targets never enable that feature.
pub async fn run_with_shutdown<S>(opts: BootstrapOptions, shutdown: S) -> Result<()>
where
    S: std::future::Future<Output = ()> + Send,
{
    init_tracing("info");
    match opts.protocol {
        TransportProtocol::Tcp => {
            run_tcp(
                opts.bind,
                opts.secret_key,
                opts.psk,
                opts.allowed_clients,
                opts.cipher,
                opts.io_uring,
                opts.verification_url,
                shutdown,
            )
            .await
        }
        TransportProtocol::Quic => {
            run_quic(
                opts.bind,
                opts.secret_key,
                opts.psk,
                opts.allowed_clients,
                opts.cipher,
                opts.quic_config,
                opts.verification_url,
                shutdown,
            )
            .await
        }
    }
}

/// Boot a server directly from a `phantom://` URI plus a local key file.
///
/// Verifies that the public key embedded in the URI matches the local key
/// pair, parses the bind address from the URI, reads the `[[allowed_clients]]`
/// whitelist from `./server.toml` (CWD-relative; missing file = open mode),
/// and dispatches to [`run_with_options`].
pub async fn run_from_uri(uri: &str, key_path: &str) -> Result<()> {
    init_tracing("info");

    let entry: ServerEntry = phantom_core::parse_phantom_uri(uri)
        .map_err(|e| anyhow::anyhow!("Failed to parse server URI: {}", e))?;

    let identity = phantom_core::KeyPair::load_server_identity(key_path)
        .map_err(|e| anyhow::anyhow!("Failed to load key from {}: {}", key_path, e))?;
    let kp = &identity.keys;
    let local_pub = kp.public_key_base64();
    if local_pub != entry.public_key {
        anyhow::bail!(
            "Public key mismatch: URI says {}, local key {} says {}",
            entry.public_key,
            key_path,
            local_pub
        );
    }
    // The URI is the credential clients use, so its PSK must match what this
    // server will actually enforce; otherwise every client would be rejected.
    if !entry.psk.is_empty() && entry.psk != identity.psk.to_base64() {
        anyhow::bail!(
            "PSK mismatch: the URI's psk= does not match the PSK in {}. \
             Re-generate the URI from this server's key file.",
            key_path
        );
    }

    let bind: SocketAddr = entry
        .address
        .parse()
        .map_err(|e| anyhow::anyhow!("Invalid bind address in URI '{}': {}", entry.address, e))?;

    // Read the inline `[[allowed_clients]]` whitelist from ./server.toml
    // (CWD-relative). Missing file → open mode.
    let toml_path = std::env::current_dir()
        .map_err(|e| anyhow::anyhow!("failed to read CWD: {}", e))?
        .join("server.toml");
    let allowed_clients = if toml_path.exists() {
        let s = toml_path
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("non-UTF8 server.toml path: {}", toml_path.display()))?;
        let cfg = ServerConfig::load(s)
            .map_err(|e| anyhow::anyhow!("failed to load {}: {}", toml_path.display(), e))?;
        cfg.load_allowed_clients()
            .map_err(|e| anyhow::anyhow!("failed to load allowed_clients: {}", e))?
    } else {
        tracing::warn!(
            "{} does not exist — running in open mode (no whitelist)",
            toml_path.display()
        );
        Vec::new()
    };

    let protocol = entry.protocol;
    let opts = BootstrapOptions {
        bind,
        secret_key: kp.secret,
        psk: identity.psk.clone(),
        allowed_clients,
        cipher: entry.cipher,
        protocol,
        quic_config: phantom_core::QuicConfig::default(),
        io_uring: false,
        verification_url: None,
    };

    run_with_options(opts).await
}

async fn run_tcp<S>(
    addr: std::net::SocketAddr,
    secret_key: [u8; 32],
    psk: phantom_core::Psk,
    allowed_clients: Vec<[u8; 32]>,
    cipher_preference: phantom_core::CipherPreference,
    _io_uring: bool,
    verification_url: Option<String>,
    shutdown: S,
) -> Result<()>
where
    S: std::future::Future<Output = ()> + Send,
{
    // tokio-uring does not compile on musl (missing libc::statx), so the
    // io_uring runtime only exists on gnu targets; on musl the flag is
    // silently ignored here but flagged at compile time in lib.rs.
    #[cfg(all(target_os = "linux", target_env = "gnu", feature = "io-uring"))]
    if _io_uring {
        return linux_ext::run_uring_server(
            addr,
            secret_key,
            psk,
            allowed_clients,
            cipher_preference,
            verification_url,
        )
        .await;
    }

    #[cfg(target_os = "linux")]
    let listener = {
        let l = linux_ext::bind(&addr).await?;
        tracing::info!("Phantom TCP server listening on {}", l.local_addr()?);
        l
    };

    #[cfg(not(target_os = "linux"))]
    let listener = {
        use phantom_core::transport::TransportListener;
        use phantom_core::transport::tcp::TcpListener;
        let l = TcpListener::bind(&addr).await?;
        tracing::info!("Phantom TCP server listening on {}", l.local_addr()?);
        l
    };

    tokio::pin!(shutdown);

    loop {
        tokio::select! {
            accept_result = listener.accept() => {
                match accept_result {
                    Ok((stream, _peer)) => {
                        let sk = secret_key;
                        let peer_psk = psk.clone();
                        let allowed = allowed_clients.clone();
                        let cipher = cipher_preference;
                        let vurl = verification_url.clone();
                        tokio::spawn(async move {
                            handler::handle_connection(stream, sk, peer_psk, &allowed, cipher, vurl.as_deref()).await;
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

async fn run_quic<S>(
    addr: std::net::SocketAddr,
    secret_key: [u8; 32],
    psk: phantom_core::Psk,
    allowed_clients: Vec<[u8; 32]>,
    cipher_preference: phantom_core::CipherPreference,
    quic_config: phantom_core::QuicConfig,
    verification_url: Option<String>,
    shutdown: S,
) -> Result<()>
where
    S: std::future::Future<Output = ()> + Send,
{
    // Noise-authenticated endpoint: the PSK and the server static key are
    // bound during the QUIC handshake itself, so a peer without them never
    // completes a connection. ASCON + QUIC fails right here with a clear
    // message instead of at first connect.
    let auth = phantom_core::QuicAuth::server(secret_key, psk, cipher_preference);
    let endpoint = phantom_core::transport::create_server_endpoint(&addr, &auth, &quic_config)
        .map_err(|e| anyhow::anyhow!("QUIC endpoint failed to start on {}: {}", addr, e))?;
    tracing::info!(
        "Phantom QUIC server listening on {}",
        endpoint.local_addr()?
    );

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
                        let allowed = allowed_clients.clone();
                        let vurl = verification_url.clone();
                        tokio::spawn(async move {
                            handler::handle_quic_connection(conn, &allowed, vurl).await;
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
    let _ = tracing_subscriber::fmt().with_env_filter(level).try_init();
}
