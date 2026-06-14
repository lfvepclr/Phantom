//! Android VpnService bridge.
//!
//! On Android the Rust core is compiled as a `cdylib` loaded by the Kotlin
//! VpnService.  The Kotlin layer creates a TUN interface via
//! `VpnService.Builder.establish()`, obtains the raw fd via `detachFd()`,
//! and passes it to Rust via a **single JNI call**.
//!
//! After the one-shot hand-off, all packet I/O, encryption, and transport
//! run entirely inside Rust.  No per-packet JNI.
//!
//! This module also mirrors the macOS bridge: it exposes a small state
//! machine (idle/starting/running/error), the last error message, and a
//! ring-buffer of recent logs so the Android UI can poll them.

use phantom_core::{
    ClientConfig, ClientSettings, FailoverConfig, HelloConfig, ProxyMode, parse_phantom_uri,
};
use std::os::unix::io::RawFd;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};
use tokio::runtime::Runtime;

#[cfg(target_os = "android")]
use jni::JNIEnv;
#[cfg(target_os = "android")]
use jni::objects::{JClass, JString};
#[cfg(target_os = "android")]
use jni::sys::jint;

static RUNTIME: Mutex<Option<Runtime>> = Mutex::new(None);

/// Tunnel lifecycle state shared with the Android UI.
///
/// - 0: idle
/// - 1: starting (tasks spawned, not yet operational)
/// - 2: running  (SOCKS5 listening, TUN active)
/// - 3: error    (failed to start or crashed)
static TUNNEL_STATUS: AtomicI32 = AtomicI32::new(0);

/// Human-readable description of the last error when status == 3.
static LAST_ERROR: Mutex<String> = Mutex::new(String::new());

/// Ring buffer for recent log lines, consumed by Kotlin via JNI.
const LOG_BUFFER_CAPACITY: usize = 200;
static LOG_BUFFER: Mutex<Vec<String>> = Mutex::new(Vec::new());
static LOG_CURSOR: Mutex<u64> = Mutex::new(0);

fn set_status(code: i32) {
    TUNNEL_STATUS.store(code, Ordering::SeqCst);
}

fn set_error(msg: String) {
    set_status(3);
    if let Ok(mut guard) = LAST_ERROR.lock() {
        *guard = msg;
    }
}

fn clear_error() {
    if let Ok(mut guard) = LAST_ERROR.lock() {
        guard.clear();
    }
}

fn push_log(line: &str) {
    let mut buf = LOG_BUFFER.lock().unwrap();
    if buf.len() >= LOG_BUFFER_CAPACITY {
        buf.remove(0);
    }
    buf.push(line.to_string());
    let mut cursor = LOG_CURSOR.lock().unwrap();
    *cursor += 1;
}

fn build_config_from_uri(uri: &str, mode: &str) -> Result<ClientConfig, i32> {
    let server_entry = match parse_phantom_uri(uri) {
        Ok(e) => e,
        Err(e) => {
            tracing::error!("URI parse error: {}", e);
            return Err(-2);
        }
    };

    let proxy_mode = match mode {
        "proxy" => ProxyMode::Proxy,
        "direct" => ProxyMode::Direct,
        _ => ProxyMode::Smart,
    };

    Ok(ClientConfig {
        servers: vec![server_entry],
        client: ClientSettings {
            listen: "127.0.0.1:11080".to_string(),
            dns: "tls://8.8.8.8:853".to_string(),
            mode: proxy_mode,
            cipher: Default::default(),
        },
        failover: FailoverConfig::default(),
        rules: Default::default(),
        hello: HelloConfig::default(),
    })
}

fn build_config_from_toml(config_str: &str) -> Result<ClientConfig, i32> {
    match toml::from_str(config_str) {
        Ok(c) => Ok(c),
        Err(e) => {
            tracing::error!("Config parse error: {}", e);
            Err(-2)
        }
    }
}

// ---------------------------------------------------------------------------
// Safe Rust wrappers
//
// The functions below are ordinary safe Rust and can be called directly by
// HarmonyOS NAPI, internal tests, or any other safe code.  They hide the
// unsafe FFI boundary from callers.
// ---------------------------------------------------------------------------

/// Start the tunnel using a `phantom://` URI and a mode string.
///
/// `fd` must be a valid, open TUN file descriptor whose ownership is
/// transferred into Rust (e.g. from `ParcelFileDescriptor.detachFd()`).
pub fn android_start_with_uri(fd: RawFd, uri: &str, mode: &str) -> i32 {
    let config = match build_config_from_uri(uri, mode) {
        Ok(c) => c,
        Err(rc) => return rc,
    };
    start_with_config(fd, config)
}

/// Start the tunnel using a TOML configuration string.
///
/// `fd` must be a valid, open TUN file descriptor whose ownership is
/// transferred into Rust.
pub fn android_start(fd: RawFd, config: &str) -> i32 {
    let mut cfg = match build_config_from_toml(config) {
        Ok(c) => c,
        Err(rc) => return rc,
    };
    // Ensure the hello config is present even if the TOML omits it.
    if cfg.hello.timeout == 0 {
        cfg.hello = HelloConfig::default();
    }
    start_with_config(fd, cfg)
}

/// Stop the tunnel and release resources.
pub fn android_stop() -> i32 {
    phantom_android_stop()
}

/// Return the current tunnel lifecycle status.
pub fn android_get_status() -> i32 {
    phantom_android_get_status()
}

/// Return the last error message, or an empty string if none.
pub fn android_get_last_error() -> String {
    LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Return recent log lines added after `since_cursor` and the new cursor.
///
/// The returned tuple is `(lines, new_cursor)`.  `lines` contains at most
/// [`LOG_BUFFER_CAPACITY`] entries.
pub fn android_get_logs(since_cursor: u64) -> (Vec<String>, u64) {
    let buf = LOG_BUFFER.lock().unwrap();
    let cursor = LOG_CURSOR.lock().unwrap();

    let skip = since_cursor.saturating_sub(*cursor - buf.len() as u64);
    let lines: Vec<String> = buf.iter().skip(skip as usize).cloned().collect();

    (lines, *cursor)
}

/// Start the tunnel using a `phantom://` URI string and mode.
///
/// # Safety
/// `fd` must be a valid, open TUN file descriptor obtained from
/// `ParcelFileDescriptor.detachFd()`.
/// `uri` must point to a valid UTF-8 string of length `uri_len`.
/// `mode` must point to a valid UTF-8 string of length `mode_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phantom_android_start_with_uri(
    fd: RawFd,
    uri: *const u8,
    uri_len: usize,
    mode: *const u8,
    mode_len: usize,
) -> i32 {
    if uri.is_null() || mode.is_null() {
        return -1;
    }
    // SAFETY: `uri`/`mode` are checked non-null and the caller guarantees
    // they point to valid UTF-8 strings of the given lengths.
    let uri_bytes = unsafe { std::slice::from_raw_parts(uri, uri_len) };
    let uri_str = match std::str::from_utf8(uri_bytes) {
        Ok(s) => s,
        Err(_) => return -1,
    };

    let mode_bytes = unsafe { std::slice::from_raw_parts(mode, mode_len) };
    let mode_str = match std::str::from_utf8(mode_bytes) {
        Ok(s) => s,
        Err(_) => return -1,
    };

    android_start_with_uri(fd, uri_str, mode_str)
}

/// Legacy entry: Initialize the Phantom tunnel with a TUN fd and TOML config.
///
/// # Safety
/// `fd` must be a valid, open TUN file descriptor obtained from
/// `ParcelFileDescriptor.detachFd()`.
/// `config_json` must point to a valid UTF-8 TOML string of length `config_len`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phantom_android_start(
    fd: RawFd,
    config_json: *const u8,
    config_len: usize,
) -> i32 {
    if config_json.is_null() {
        return -1;
    }
    // SAFETY: `config_json` is checked non-null and the caller guarantees
    // it points to a valid UTF-8 string of length `config_len`.
    let config_bytes = unsafe { std::slice::from_raw_parts(config_json, config_len) };
    let config_str = match std::str::from_utf8(config_bytes) {
        Ok(s) => s,
        Err(_) => return -1,
    };

    android_start(fd, config_str)
}

/// Common tunnel start logic shared by both URI and TOML entry points.
fn start_with_config(fd: RawFd, config: ClientConfig) -> i32 {
    // Install a tracing subscriber that captures INFO+ logs into the
    // ring buffer so the Kotlin UI can display them.  Only install once.
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

    rt.spawn(async move {
        // Before accepting traffic, prove the full path:
        // client -> server -> internet.
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
                set_error(msg);
                return;
            }
            Err(e) => {
                let msg = format!("Hello verification error: {}", e);
                tracing::error!("{}", msg);
                set_error(msg);
                return;
            }
        }

        // 1. Wrap the VpnService fd into a TunDevice.
        let device = match crate::tun::TunDevice::from_fd(fd) {
            Ok(d) => d,
            Err(e) => {
                let msg = format!("TUN fd wrap failed: {}", e);
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
                return;
            }
        };

        // Start health check loop.
        let failover_health = std::sync::Arc::clone(&failover);
        tokio::spawn(async move {
            failover_health.run_health_check_loop().await;
        });

        // 2. Start local SOCKS5 proxy (listens on loopback).
        let socks5_addr = match config.client.listen.parse() {
            Ok(a) => a,
            Err(e) => {
                let msg = format!("Invalid SOCKS5 address: {}", e);
                tracing::error!("{}", msg);
                set_error(msg);
                return;
            }
        };

        let config_clone = config.clone();
        let failover_socks5 = std::sync::Arc::clone(&failover);
        let socks5_task = tokio::spawn(async move {
            let listener = match tokio::net::TcpListener::bind(&config_clone.client.listen).await {
                Ok(l) => l,
                Err(e) => {
                    let msg = format!("SOCKS5 bind failed: {}", e);
                    tracing::error!("{}", msg);
                    set_error(msg);
                    return;
                }
            };
            tracing::info!("SOCKS5 proxy listening on {}", config_clone.client.listen);
            // SOCKS5 is up and accepting connections -> tunnel is operational.
            set_status(2); // running

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
                tokio::spawn(async move {
                    let local_secret = match phantom_core::crypto::KeyPair::generate() {
                        Ok(kp) => kp.secret,
                        Err(_) => return,
                    };
                    if let Err(e) =
                        crate::socks5::handle_socks5_connection(stream, &cfg, &fo, local_secret)
                            .await
                    {
                        tracing::debug!("SOCKS5 connection error from {}: {}", peer, e);
                    }
                });
            }
        });

        // 3. Start TUN transparent proxy.
        let tun_task = tokio::spawn(async move {
            let tun_secret = match phantom_core::crypto::KeyPair::generate() {
                Ok(kp) => kp.secret,
                Err(e) => {
                    let msg = format!("TUN key generation failed: {}", e);
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

            tracing::info!("Android TUN proxy started on fd {}", fd);
            if let Err(e) = proxy.run().await {
                let msg = format!("TUN proxy exited: {}", e);
                tracing::error!("{}", msg);
                set_error(msg);
            }
        });

        let _ = tokio::try_join!(socks5_task, tun_task);
        // If either long-running task returns, the tunnel is no longer operational.
        if TUNNEL_STATUS.load(Ordering::SeqCst) == 2 {
            set_error("Tunnel task exited unexpectedly".to_string());
        }
    });

    0
}

/// Stop the tunnel and release resources.
#[unsafe(no_mangle)]
pub extern "C" fn phantom_android_stop() -> i32 {
    if let Some(rt) = RUNTIME.lock().unwrap().take() {
        rt.shutdown_background();
    }
    set_status(0); // idle
    clear_error();
    tracing::info!("Android tunnel stopped");
    0
}

/// Return the current tunnel lifecycle status.
#[unsafe(no_mangle)]
pub extern "C" fn phantom_android_get_status() -> i32 {
    TUNNEL_STATUS.load(Ordering::SeqCst)
}

/// Return the last error message when status == 3, or an empty string.
/// The caller must free the returned pointer with `phantom_android_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn phantom_android_get_last_error() -> *mut std::ffi::c_char {
    // SAFETY: `CString::into_raw` hands ownership of the heap allocation to the
    // caller.  The contract is that the caller later calls `phantom_android_free_string`
    // to reclaim it.
    std::ffi::CString::new(android_get_last_error())
        .unwrap_or_default()
        .into_raw()
}

/// Return recent log lines as a newline-separated C string.
///
/// The returned string format is `<cursor>\n<line1>\n<line2>...` so the caller
/// can update its cursor without an additional out-parameter.  The caller must
/// free the returned pointer with `phantom_android_free_string`.
#[unsafe(no_mangle)]
pub extern "C" fn phantom_android_get_logs(since_cursor: u64) -> *mut std::ffi::c_char {
    let (lines, cursor) = android_get_logs(since_cursor);
    let result = format!("{}\n{}", cursor, lines.join("\n"));

    // SAFETY: same contract as `phantom_android_get_last_error`.
    std::ffi::CString::new(result)
        .unwrap_or_default()
        .into_raw()
}

/// Free a string returned by `phantom_android_get_last_error` or
/// `phantom_android_get_logs`.
///
/// # Safety
/// `ptr` must be a pointer previously returned by one of the `get_*` functions,
/// and must not have been freed already.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn phantom_android_free_string(ptr: *mut std::ffi::c_char) {
    // SAFETY: caller guarantees `ptr` was returned by `CString::into_raw` in one
    // of the `get_*` functions above and has not been freed yet.
    if !ptr.is_null() {
        unsafe {
            let _ = std::ffi::CString::from_raw(ptr);
        }
    }
}

fn parse_dns_addr(dns: &str) -> Option<std::net::SocketAddr> {
    let stripped = dns.strip_prefix("tls://").unwrap_or(dns);
    let stripped = stripped.strip_prefix("https://").unwrap_or(stripped);
    stripped
        .parse()
        .ok()
        .or_else(|| format!("{}:53", stripped).parse().ok())
}

/// A `std::io::Write` implementation that appends each line to `LOG_BUFFER`.
/// Used as the `tracing_subscriber::fmt` writer so all INFO+ logs are visible
/// in the Kotlin UI.
struct LogBufferWriter;

impl LogBufferWriter {
    fn new() -> Self {
        Self
    }
}

impl std::io::Write for LogBufferWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let s = String::from_utf8_lossy(buf);
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

impl<'a> tracing_subscriber::fmt::writer::MakeWriter<'a> for LogBufferWriter {
    type Writer = LogBufferWriter;
    fn make_writer(&'a self) -> Self::Writer {
        LogBufferWriter
    }
}

// JNI wrappers used by Kotlin `co.phantom.android.RustBridge`.
// These mirror the C-ABI functions above but accept JString parameters and
// return JString values so Kotlin can call them with ordinary `external fun`
// declarations.

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_startTunnelWithURI<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    fd: jint,
    uri: JString<'local>,
    mode: JString<'local>,
) -> jint {
    let uri = match env.get_string(&uri) {
        Ok(s) => s.to_str().unwrap_or("").to_string(),
        Err(_) => return -1,
    };
    let mode = match env.get_string(&mode) {
        Ok(s) => s.to_str().unwrap_or("smart").to_string(),
        Err(_) => return -1,
    };
    android_start_with_uri(fd as RawFd, &uri, &mode)
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_startTunnel<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    fd: jint,
    config: JString<'local>,
) -> jint {
    let config_str = match env.get_string(&config) {
        Ok(s) => s.to_str().unwrap_or("").to_string(),
        Err(_) => return -1,
    };
    android_start(fd as RawFd, &config_str)
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_stopTunnel(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    android_stop()
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_getStatus(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    android_get_status()
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_getLastError<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> JString<'local> {
    env.new_string(&android_get_last_error())
        .expect("new_string failed")
}

#[cfg(target_os = "android")]
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_getLogs<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    since_cursor: jni::sys::jlong,
) -> JString<'local> {
    let (lines, cursor) = android_get_logs(since_cursor as u64);
    let result = format!("{}\n{}", cursor, lines.join("\n"));
    env.new_string(&result).expect("new_string failed")
}

#[cfg(all(test, target_os = "android"))]
mod tests {
    use super::*;

    #[test]
    fn status_round_trip() {
        set_status(2);
        assert_eq!(TUNNEL_STATUS.load(Ordering::SeqCst), 2);
        set_status(0);
    }

    #[test]
    fn last_error_is_set_and_cleared() {
        set_error("test error".to_string());
        assert_eq!(*LAST_ERROR.lock().unwrap(), "test error");
        clear_error();
        assert!(LAST_ERROR.lock().unwrap().is_empty());
    }

    #[test]
    fn log_buffer_keeps_recent_lines() {
        // Drain existing logs.
        LOG_BUFFER.lock().unwrap().clear();
        *LOG_CURSOR.lock().unwrap() = 0;

        for i in 0..LOG_BUFFER_CAPACITY + 10 {
            push_log(&format!("log {}", i));
        }

        let buf = LOG_BUFFER.lock().unwrap();
        assert_eq!(buf.len(), LOG_BUFFER_CAPACITY);
        assert!(buf.first().unwrap().contains("log 10"));
        assert!(
            buf.last()
                .unwrap()
                .contains(&format!("log {}", LOG_BUFFER_CAPACITY + 9))
        );
    }

    #[test]
    fn parse_dns_addr_accepts_tls_prefix() {
        assert_eq!(
            parse_dns_addr("tls://8.8.8.8:853"),
            Some("8.8.8.8:853".parse().unwrap())
        );
        assert_eq!(
            parse_dns_addr("1.1.1.1"),
            Some("1.1.1.1:53".parse().unwrap())
        );
    }
}
