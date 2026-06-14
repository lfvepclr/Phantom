//! macOS native tunnel bridge.
//!
//! On macOS the Rust core is compiled as a `cdylib`.
//! SwiftUI calls into Rust via a thin C FFI layer to start/stop the tunnel.
//! Rust fully owns the TUN device lifecycle — no Network Extension needed.

use phantom_core::{
    ClientConfig, ClientSettings, FailoverConfig, HelloConfig, ProxyMode, parse_phantom_uri,
};
use std::os::unix::io::RawFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};
use tokio::runtime::Runtime;

static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

/// Tunnel lifecycle state shared with the Swift UI.
///
/// - 0: idle
/// - 1: starting (tasks spawned, not yet operational)
/// - 2: running  (SOCKS5 listening successfully)
/// - 3: error    (failed to start or crashed)
static TUNNEL_STATUS: AtomicI32 = AtomicI32::new(0);

/// Human-readable description of the last error when status == 3.
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

/// Ring buffer for recent log lines, consumed by Swift via FFI.
const LOG_BUFFER_CAPACITY: usize = 200;
static LOG_BUFFER: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LOG_CURSOR: Mutex<u64> = Mutex::new(0);

/// Append a log line to the ring buffer.
fn push_log(line: &str) {
    let mut buf = LOG_BUFFER.lock().unwrap();
    if buf.len() >= LOG_BUFFER_CAPACITY {
        buf.remove(0);
    }
    buf.push(line.to_string());
    let mut cursor = LOG_CURSOR.lock().unwrap();
    *cursor += 1;
}

fn set_status(code: i32) {
    TUNNEL_STATUS.store(code, Ordering::SeqCst);
}

fn set_error(msg: String) {
    set_status(3);
    if let Ok(mut guard) = LAST_ERROR.lock() {
        *guard = msg;
    }
}

fn get_status() -> i32 {
    TUNNEL_STATUS.load(Ordering::SeqCst)
}

fn clear_error() {
    if let Ok(mut guard) = LAST_ERROR.lock() {
        guard.clear();
    }
}

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
    // SAFETY: caller guarantees `input` points to a valid UTF-8 string
    // of length `input_len`.
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
        hello: HelloConfig::default(),
    };

    start_with_config(config)
}

/// Legacy entry: start the tunnel with a TOML config string.
///
/// # Safety
/// `config_toml` must point to a valid UTF-8 string of length `config_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phantom_macos_start(config_toml: *const u8, config_len: usize) -> i32 {
    // SAFETY: caller guarantees `config_toml` points to a valid UTF-8 string
    // of length `config_len`.
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
    // Install a tracing subscriber that captures INFO+ logs into the
    // ring buffer so the Swift UI can display them.  Only install once;
    // subsequent calls are no-ops (the global subscriber is already set).
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_writer(LogBufferWriter::new)
        .try_init();

    set_status(1); // starting
    clear_error();
    push_log(&format!(
        "[INFO] Starting tunnel (mode={:?}) ...",
        config.client.mode
    ));

    let rt = match Runtime::new() {
        Ok(r) => r,
        Err(_) => {
            set_error("Failed to create tokio runtime".to_string());
            return -3;
        }
    };

    {
        let mut guard = RUNTIME.lock().unwrap();
        *guard = Some(rt);
    }

    let rt = RUNTIME.lock().unwrap();
    let rt = rt.as_ref().unwrap();

    // Channel used to report whether SOCKS5 proxy started successfully.
    let (ready_tx, ready_rx) = std::sync::mpsc::channel::<Result<(), String>>();

    rt.spawn(async move {
        let local_secret = match phantom_core::crypto::KeyPair::generate() {
            Ok(kp) => kp.secret,
            Err(e) => {
                let msg = format!("Key generation failed: {}", e);
                tracing::error!("{}", msg);
                set_error(msg);
                return;
            }
        };

        let failover = match crate::failover::FailoverManager::new(&config) {
            Ok(f) => std::sync::Arc::new(f),
            Err(e) => {
                let msg = format!("Failover manager init failed: {}", e);
                tracing::error!("{}", msg);
                set_error(msg);
                let _ = ready_tx.send(Err("Failover manager init failed".to_string()));
                return;
            }
        };

        // Start health check loop.
        let failover_health = std::sync::Arc::clone(&failover);
        tokio::spawn(async move {
            failover_health.run_health_check_loop().await;
        });

        // Before starting local proxies, verify the full path:
        // client → server → internet.  This prevents the UI from showing
        // "Connected" when only the local SOCKS5 listener has come up.
        match crate::hello::verify_server_connection(&config).await {
            Ok(result) if result.success => {
                tracing::info!(
                    "Hello verification passed: {} ({} ms)",
                    result.message,
                    result.latency_ms
                );
            }
            Ok(result) => {
                let msg = format!("Hello verification failed: {}", result.message);
                tracing::error!("{}", msg);
                set_error(msg.clone());
                let _ = ready_tx.send(Err(msg));
                return;
            }
            Err(e) => {
                let msg = format!("Hello verification error: {}", e);
                tracing::error!("{}", msg);
                set_error(msg.clone());
                let _ = ready_tx.send(Err(msg));
                return;
            }
        }

        let tun_secret = local_secret;

        // 1. Start local SOCKS5 proxy.
        let config_clone = config.clone();
        let failover_socks5 = std::sync::Arc::clone(&failover);
        let ready_tx2 = ready_tx.clone();
        let socks5_task = tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(&config_clone.client.listen).await {
                Ok(l) => l,
                Err(e) => {
                    let msg = format!("SOCKS5 bind failed: {}", e);
                    tracing::error!("{}", msg);
                    let _ = ready_tx2.send(Err(msg.clone()));
                    set_error(msg);
                    return;
                }
            };
            tracing::info!("SOCKS5 proxy listening on {}", config_clone.client.listen);
            // SOCKS5 is up and accepting connections → tunnel is operational.
            set_status(2); // running
            let _ = ready_tx2.send(Ok(()));

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
                    if let Err(e) =
                        crate::socks5::handle_socks5_connection(stream, &cfg, &fo, secret).await
                    {
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
                    let msg = format!("TUN creation failed: {}", e);
                    tracing::error!("{}", msg);
                    set_error(msg);
                    let _ = ready_tx.send(Err("TUN creation failed".to_string()));
                    return;
                }
            };
            let socks5_addr = match config.client.listen.parse() {
                Ok(a) => a,
                Err(e) => {
                    let msg = format!("Invalid SOCKS5 address: {}", e);
                    tracing::error!("{}", msg);
                    set_error(msg);
                    return;
                }
            };
            let mut proxy =
                crate::tun::TunProxy::new(device, socks5_addr).with_mode(config.client.mode);

            if let Some(server) = config.servers.first() {
                proxy = proxy.with_server(server.clone(), tun_secret);
            }

            if let Ok(engine) = crate::rules::RuleEngine::from_config(&config.rules) {
                proxy = proxy.with_rules(engine);
                tracing::info!(
                    "Smart routing enabled with {} rules",
                    config.rules.rules.len()
                );
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
                let msg = format!("TUN proxy exited: {}", e);
                tracing::error!("{}", msg);
                set_error(msg);
            }
        });

        let _ = tokio::try_join!(socks5_task, tun_task);
        // If either long-running task returns, the tunnel is no longer operational.
        if get_status() == 2 {
            set_error("Tunnel task exited unexpectedly".to_string());
        }
    });

    // Wait a short time for the SOCKS5 listener to come up.  This keeps the
    // Swift start() call synchronous while still reporting real success/failure.
    match ready_rx.recv_timeout(std::time::Duration::from_millis(1500)) {
        Ok(Ok(())) => 0,
        Ok(Err(msg)) => {
            push_log(&format!("[ERROR] {}", msg));
            -4
        }
        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
            // SOCKS5 did not report within the timeout.  The async task is still
            // running; Swift should poll get_status() for updates.
            0
        }
        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
            set_error("Start task aborted".to_string());
            -5
        }
    }
}

/// Stop the tunnel by shutting down the tokio runtime.
#[unsafe(no_mangle)]
pub extern "C" fn phantom_macos_stop() -> i32 {
    if let Some(rt) = RUNTIME.lock().unwrap().take() {
        rt.shutdown_background();
    }
    set_status(0); // idle
    clear_error();
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

/// A `std::io::Write` implementation that appends each line to `LOG_BUFFER`.
/// Used as the `tracing_subscriber::fmt` writer so all INFO+ logs are
/// visible in the Swift UI.
struct LogBufferWriter;

impl LogBufferWriter {
    fn new() -> Self {
        Self
    }
}

impl std::io::Write for LogBufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let s = String::from_utf8_lossy(buf);
        // tracing_subscriber::fmt writes one line at a time, but we
        // split on newlines just in case.
        for line in s.lines() {
            if !line.is_empty() {
                push_log(line);
            }
        }
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

// MakeWriter impl so tracing_subscriber::fmt().with_writer(LogBufferWriter::new)
// can use it.
impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for LogBufferWriter {
    type Writer = LogBufferWriter;
    fn make_writer(&'a self) -> Self::Writer {
        LogBufferWriter
    }
}

/// Return recent log lines as a newline-separated C string.
///
/// The returned string format is `<cursor>\n<line1>\n<line2>...` so the caller
/// can update its cursor without an additional out-parameter.  The caller must
/// free the returned pointer with `phantom_macos_free_logs`.
#[unsafe(no_mangle)]
pub extern "C" fn phantom_macos_get_logs(since_cursor: u64) -> *mut std::ffi::c_char {
    let buf = LOG_BUFFER.lock().unwrap();
    let cursor = LOG_CURSOR.lock().unwrap();

    // Only return lines added after the caller's last cursor.
    let skip = since_cursor.saturating_sub(*cursor - buf.len() as u64);
    let lines: Vec<&str> = buf.iter().skip(skip as usize).map(|s| s.as_str()).collect();
    let result = format!("{}\n{}", *cursor, lines.join("\n"));

    // SAFETY: `CString::into_raw` hands ownership of the heap allocation to the
    // caller.  The contract is that the caller later calls `phantom_macos_free_logs`
    // to reclaim it.
    std::ffi::CString::new(result)
        .unwrap_or_default()
        .into_raw()
}

/// Free the string returned by `phantom_macos_get_logs`.
///
/// # Safety
/// `ptr` must be a pointer previously returned by `phantom_macos_get_logs`,
/// and must not have been freed already.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phantom_macos_free_logs(ptr: *mut std::ffi::c_char) {
    // SAFETY: caller guarantees `ptr` was returned by `CString::into_raw` in
    // `phantom_macos_get_logs` and has not been freed yet.
    if !ptr.is_null() {
        unsafe {
            let _ = std::ffi::CString::from_raw(ptr);
        }
    }
}

/// Return the current tunnel lifecycle status.
///
/// - 0: idle
/// - 1: starting
/// - 2: running  (SOCKS5 listening, ready for traffic)
/// - 3: error    (see `phantom_macos_get_last_error` for details)
#[unsafe(no_mangle)]
pub extern "C" fn phantom_macos_get_status() -> i32 {
    get_status()
}

/// Return the last error message when status == 3, or an empty string.
/// The caller must free the returned pointer with `phantom_macos_free_logs`.
#[unsafe(no_mangle)]
pub extern "C" fn phantom_macos_get_last_error() -> *mut std::ffi::c_char {
    let msg = LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // SAFETY: same contract as `phantom_macos_get_logs`.
    std::ffi::CString::new(msg)
        .unwrap_or_default()
        .into_raw()
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
