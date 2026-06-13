//! macOS native tunnel bridge.
//!
//! On macOS the Rust core is compiled as a `cdylib`.
//! SwiftUI calls into Rust via a thin C FFI layer to start/stop the tunnel.
//! Rust fully owns the TUN device lifecycle — no Network Extension needed.

use phantom_core::{ClientConfig, ClientSettings, FailoverConfig, ProxyMode, parse_phantom_uri};
use std::os::unix::io::RawFd;
use std::sync::Mutex;
use tokio::runtime::Runtime;

static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

/// Default SOCKS5 listen address used by the macOS native client.
/// Single source of truth — Swift queries the port via FFI.
const DEFAULT_SOCKS5_ADDR: &str = "127.0.0.1:11080";

/// Start the tunnel using a `phantom://` URI + mode string.
///
/// The `input` parameter is `"phantom://key@host:port|mode"` where `mode`
/// is one of: proxy, smart, direct.
///
/// # Safety
/// `input` must point to a valid UTF-8 string of length `input_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phantom_macos_start_with_uri(input: *const u8, input_len: usize) -> i32 {
    let input_bytes = unsafe { std::slice::from_raw_parts(input, input_len) };
    let input_str = match std::str::from_utf8(input_bytes) {
        Ok(s) => s,
        Err(_) => return -1,
    };

    // Split URI and mode: "phantom://key@host:port|mode"
    let (uri_part, mode_str) = input_str.split_once('|').unwrap_or((input_str, "smart"));

    let server_entry = match parse_phantom_uri(uri_part) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("URI parse error: {}", e);
            return -2;
        }
    };

    let mode = match mode_str {
        "proxy" => ProxyMode::Proxy,
        "direct" => ProxyMode::Direct,
        _ => ProxyMode::Smart,
    };

    let config = ClientConfig {
        servers: vec![server_entry],
        client: ClientSettings {
            listen: DEFAULT_SOCKS5_ADDR.to_string(),
            dns: "tls://8.8.8.8:853".to_string(),
            mode,
            cipher: Default::default(),
        },
        failover: FailoverConfig::default(),
        rules: Default::default(),
    };

    start_with_config(config)
}

/// Legacy entry: start the tunnel with a TOML config string.
///
/// # Safety
/// `config_toml` must point to a valid UTF-8 string of length `config_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phantom_macos_start(config_toml: *const u8, config_len: usize) -> i32 {
    let config_bytes = unsafe { std::slice::from_raw_parts(config_toml, config_len) };
    let config_str = match std::str::from_utf8(config_bytes) {
        Ok(s) => s,
        Err(_) => return -1,
    };

    let config: ClientConfig = match toml::from_str(config_str) {
        Ok(c) => c,
        Err(e) => {
            tracing::error!("Config parse error: {}", e);
            return -2;
        }
    };

    start_with_config(config)
}

/// Common tunnel start logic shared by both URI and TOML entry points.
fn start_with_config(config: ClientConfig) -> i32 {
    let rt = match Runtime::new() {
        Ok(r) => r,
        Err(_) => return -3,
    };

    {
        let mut guard = RUNTIME.lock().unwrap();
        *guard = Some(rt);
    }

    let rt = RUNTIME.lock().unwrap();
    let rt = rt.as_ref().unwrap();

    rt.spawn(async move {
        let local_secret = match phantom_core::crypto::KeyPair::generate() {
            Ok(kp) => kp.secret,
            Err(e) => {
                tracing::error!("Key generation failed: {}", e);
                return;
            }
        };

        let failover = match crate::failover::FailoverManager::new(&config) {
            Ok(f) => std::sync::Arc::new(f),
            Err(e) => {
                tracing::error!("Failover manager init failed: {}", e);
                return;
            }
        };

        // Start health check loop.
        let failover_health = std::sync::Arc::clone(&failover);
        tokio::spawn(async move {
            failover_health.run_health_check_loop().await;
        });

        let tun_secret = local_secret;

        // 1. Start local SOCKS5 proxy.
        let config_clone = config.clone();
        let failover_socks5 = std::sync::Arc::clone(&failover);
        let socks5_task = tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(&config_clone.client.listen).await {
                Ok(l) => l,
                Err(e) => {
                    tracing::error!("SOCKS5 bind failed: {}", e);
                    return;
                }
            };
            tracing::info!("SOCKS5 proxy listening on {}", config_clone.client.listen);

            loop {
                let (stream, peer) = match listener.accept().await {
                    Ok(x) => x,
                    Err(e) => {
                        tracing::debug!("SOCKS5 accept error: {}", e);
                        continue;
                    }
                };
                let cfg = config_clone.clone();
                let fo = std::sync::Arc::clone(&failover_socks5);
                let secret = local_secret;
                tokio::spawn(async move {
                    if let Err(e) = crate::socks5::handle_socks5_connection(stream, &cfg, &fo, secret).await {
                        tracing::debug!("SOCKS5 connection error from {}: {}", peer, e);
                    }
                });
            }
        });

        // 2. Start TUN transparent proxy.
        let tun_task = tokio::spawn(async move {
            let device = match crate::tun::TunDevice::create() {
                Ok(d) => d,
                Err(e) => {
                    tracing::error!("TUN creation failed: {}", e);
                    return;
                }
            };
            let socks5_addr = match config.client.listen.parse() {
                Ok(a) => a,
                Err(e) => {
                    tracing::error!("Invalid SOCKS5 address: {}", e);
                    return;
                }
            };
            let mut proxy = crate::tun::TunProxy::new(device, socks5_addr)
                .with_mode(config.client.mode);

            if let Some(server) = config.servers.first() {
                proxy = proxy.with_server(server.clone(), tun_secret);
            }

            if let Ok(engine) = crate::rules::RuleEngine::from_config(&config.rules) {
                proxy = proxy.with_rules(engine);
                tracing::info!("Smart routing enabled with {} rules", config.rules.rules.len());
            }

            if let Some(dns_addr) = parse_dns_addr(&config.client.dns) {
                match crate::dns::DnsProxy::new(dns_addr).await {
                    Ok(dns) => {
                        proxy = proxy.with_dns(dns);
                        tracing::info!("DNS hijack enabled, upstream = {}", dns_addr);
                    }
                    Err(e) => {
                        tracing::warn!("DNS proxy init failed: {}", e);
                    }
                }
            }

            tracing::info!("TUN proxy started");
            if let Err(e) = proxy.run().await {
                tracing::error!("TUN proxy exited: {}", e);
            }
        });

        let _ = tokio::try_join!(socks5_task, tun_task);
    });

    0
}

/// Stop the tunnel by shutting down the tokio runtime.
#[unsafe(no_mangle)]
pub extern "C" fn phantom_macos_stop() -> i32 {
    if let Some(rt) = RUNTIME.lock().unwrap().take() {
        rt.shutdown_background();
    }
    tracing::info!("macOS tunnel stopped");
    0
}

fn parse_dns_addr(dns: &str) -> Option<std::net::SocketAddr> {
    let stripped = dns.strip_prefix("tls://").unwrap_or(dns);
    let stripped = stripped.strip_prefix("https://").unwrap_or(stripped);
    stripped.parse().ok().or_else(|| {
        // Fallback: if no port, append :53.
        format!("{}:53", stripped).parse().ok()
    })
}

/// Return the utun fd so Swift can optionally inspect it.
/// Currently returns -1 (fd is owned entirely by Rust).
#[unsafe(no_mangle)]
pub extern "C" fn phantom_macos_get_tun_fd() -> RawFd {
    -1
}

/// Return the local SOCKS5 listen port.
///
/// Swift calls this before configuring the system proxy so the proxy
/// port always matches the port Rust is actually listening on.
#[unsafe(no_mangle)]
pub extern "C" fn phantom_macos_get_socks5_port() -> u16 {
    match DEFAULT_SOCKS5_ADDR.parse::<std::net::SocketAddr>() {
        Ok(addr) => addr.port(),
        Err(_) => 0,
    }
}
