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
use jni::JavaVM;
#[cfg(target_os = "android")]
use jni::objects::{GlobalRef, JClass, JValue};

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

/// Optional on-disk mirror of [`LOG_BUFFER`].
///
/// The ring buffer dies with the process, which is exactly when a bug report
/// needs it most (app killed by the OS, VPN service crashed, tunnel wedged).
/// The platform shell points this at a sandbox file at service start; the file
/// is capped so a session that runs for days cannot fill the phone.
static LOG_FILE: Mutex<Option<std::fs::File>> = Mutex::new(None);
static LOG_FILE_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const LOG_FILE_MAX_BYTES: u64 = 1024 * 1024;

/// JVM handle and `RustBridge` class captured on the first JNI call.
///
/// `VpnService.protect()` is an instance method on the *service*, but the fd
/// it has to adopt is created later, deep inside the datapath. Remembering the
/// VM here is what lets `protect_fd` reach back into Kotlin from a tokio
/// worker thread.
#[cfg(target_os = "android")]
static PROTECT_BRIDGE: Mutex<Option<(JavaVM, GlobalRef)>> = Mutex::new(None);

/// Live counters of the running tunnel, shared with the UI.
///
/// The tunnel owns the only copy that matters (it is created on `start`), so
/// the UI polls a clone instead of keeping its own bookkeeping. `None` until
/// the first successful start.
static TRAFFIC_STATS: Mutex<Option<std::sync::Arc<crate::stats::TrafficStats>>> =
    Mutex::new(None);

/// Long-lived datapath objects the UI process may need to poke when the phone
/// switches networks. They are created inside the tunnel task, so the only way
/// to reach them later is to publish them here.
static SHARED_DNS: Mutex<Option<std::sync::Arc<crate::dns::DnsProxy>>> = Mutex::new(None);
static SHARED_QUIC: Mutex<Option<std::sync::Arc<crate::quic_pool::QuicPool>>> =
    Mutex::new(None);
static SHARED_TCP_POOL: Mutex<Option<std::sync::Arc<crate::tcp_pool::TcpSessionPool>>> =
    Mutex::new(None);
static SHARED_FAILOVER: Mutex<Option<std::sync::Arc<crate::failover::FailoverManager>>> =
    Mutex::new(None);

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
    {
        let mut buf = LOG_BUFFER.lock().unwrap();
        if buf.len() >= LOG_BUFFER_CAPACITY {
            buf.remove(0);
        }
        buf.push(line.to_string());
        let mut cursor = LOG_CURSOR.lock().unwrap();
        *cursor += 1;
    }
    append_log_file(line);
}

/// Point the on-disk log mirror at `path`, or disable it with `None`.
///
/// Returns 0 on success and -1 when the file could not be opened (the caller
/// keeps its in-memory log either way).
pub fn android_set_log_path(path: Option<&str>) -> i32 {
    let mut guard = LOG_FILE.lock().unwrap_or_else(|e| e.into_inner());
    *guard = None;
    LOG_FILE_BYTES.store(0, Ordering::Relaxed);
    let Some(path) = path else {
        return 0;
    };
    match std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        Ok(file) => {
            let len = file.metadata().map(|m| m.len()).unwrap_or(0);
            LOG_FILE_BYTES.store(len, Ordering::Relaxed);
            *guard = Some(file);
            tracing::info!("session log mirroring to {path}");
            0
        }
        Err(e) => {
            tracing::warn!("session log file {path} could not be opened: {e}");
            -1
        }
    }
}

/// Drop every buffered log line and truncate the on-disk mirror.
///
/// Both halves have to go together: clearing only the buffer would leave the
/// lines the operator just dismissed sitting in the file they open next.
pub fn android_clear_logs() -> i32 {
    LOG_BUFFER.lock().unwrap_or_else(|e| e.into_inner()).clear();
    *LOG_CURSOR.lock().unwrap_or_else(|e| e.into_inner()) = 0;
    if let Some(file) = LOG_FILE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        let _ = file.set_len(0);
    }
    LOG_FILE_BYTES.store(0, Ordering::Relaxed);
    0
}

/// Append one line to the mirrored file, rotating it when it hits the cap.
///
/// Lock order is always `LOG_BUFFER` → `LOG_CURSOR` → `LOG_FILE`; `push_log`
/// calls this only after releasing the first two, so the JNI clear path cannot
/// deadlock against the tracing writer.
fn append_log_file(line: &str) {
    use std::io::Write;

    let mut guard = LOG_FILE.lock().unwrap_or_else(|e| e.into_inner());
    let Some(file) = guard.as_mut() else {
        return;
    };
    let pending = line.len() as u64 + 1;
    if LOG_FILE_BYTES.load(Ordering::Relaxed) + pending > LOG_FILE_MAX_BYTES {
        // Rotate in place: keep the most recent lines (the ring buffer is a
        // superset of what the UI can show) instead of dropping the file.
        let tail = LOG_BUFFER
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if file.set_len(0).is_err() {
            return;
        }
        let mut written = 0u64;
        for old in tail.iter() {
            if file.write_all(old.as_bytes()).is_err() || file.write_all(b"\n").is_err() {
                return;
            }
            written += old.len() as u64 + 1;
        }
        LOG_FILE_BYTES.store(written, Ordering::Relaxed);
        return;
    }
    if file.write_all(line.as_bytes()).is_ok() && file.write_all(b"\n").is_ok() {
        LOG_FILE_BYTES.fetch_add(pending, Ordering::Relaxed);
    }
}

/// Exempt a socket from the VPN interface via `VpnService.protect()`.
///
/// Returns `false` when the bridge has not been captured yet or Kotlin refused
/// the fd; the caller then knows its packets will be captured by the TUN.
#[cfg(target_os = "android")]
pub fn remember_bridge(env: &mut JNIEnv, class: &JClass) {
    let vm = match env.get_java_vm() {
        Ok(vm) => vm,
        Err(e) => {
            tracing::debug!("remember_bridge: get_java_vm failed: {e}");
            return;
        }
    };
    let global = match env.new_global_ref(class) {
        Ok(global) => global,
        Err(e) => {
            tracing::debug!("remember_bridge: new_global_ref failed: {e}");
            return;
        }
    };
    *PROTECT_BRIDGE.lock().unwrap_or_else(|e| e.into_inner()) = Some((vm, global));
}

#[cfg(target_os = "android")]
pub fn protect_fd(fd: RawFd) -> bool {
    let guard = PROTECT_BRIDGE.lock().unwrap_or_else(|e| e.into_inner());
    let Some((vm, class)) = guard.as_ref() else {
        return false;
    };
    let mut env = match vm.attach_current_thread() {
        Ok(env) => env,
        Err(e) => {
            tracing::debug!("protect_fd: attach failed: {e}");
            return false;
        }
    };
    match env.call_static_method(class, "protectFd", "(I)Z", &[JValue::Int(fd)]) {
        Ok(value) => value.z().unwrap_or(false),
        Err(e) => {
            // A pending Java exception poisons every later JNI call on this
            // thread, and this runs on tokio workers that make plenty of them;
            // clear it so one rejected fd cannot take the datapath down.
            if env.exception_check().unwrap_or(false) {
                let _ = env.exception_clear();
            }
            tracing::warn!("protect_fd: protectFd({fd}) failed: {e}");
            false
        }
    }
}

/// Replace the user "分流白名单" rules the UI edited.
///
/// Same contract as the desktop `phantom_macos_set_proxy_domains`: call before
/// `android_start_with_uri`, because the routing state is built once per start.
/// Text rather than a structured list keeps the JNI surface a single string and
/// lets Android and HarmonyOS share one format (see [`crate::whitelist`]).
pub fn android_set_user_rules(text: &str) -> i32 {
    crate::whitelist::set_user_rules(text);
    0
}

/// Host builds (`cargo check` for the mobile bridge) have no VpnService.
#[cfg(not(target_os = "android"))]
pub fn protect_fd(_fd: RawFd) -> bool {
    true
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
            // Tunnel resolver for whitelisted domains; the URI form keeps the
            // direct resolver at the `ClientSettings` default (domestic).
            dns: "8.8.8.8:53".to_string(),
            dns_direct: phantom_core::ClientSettings::default().dns_direct,
            mode: proxy_mode,
            cipher: Default::default(),
            metrics_listen: "127.0.0.1:9150".to_string(),
            proxy_auth: None,
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

/// Snapshot of the live traffic counters as a JSON object.
///
/// Returns JSON (rather than a wide tuple) so the binding stays stable when a
/// counter is added, and so the UI can render it without positional parsing:
/// `{"up":<tcp bytes up>,"down":<tcp bytes down>,"udp_up":…,"udp_down":…,
///   "conns":…,"route_direct":…,"route_proxy":…}`.
///
/// All zeroes when no tunnel has been started yet.
pub fn android_get_stats_json() -> String {
    let stats = TRAFFIC_STATS.lock().unwrap_or_else(|e| e.into_inner()).clone();
    // The JSON shape lives in `TrafficStats` so every platform bridge (JNI,
    // NAPI, macOS C FFI) reports the same keys in the same order.
    match stats {
        Some(stats) => stats.snapshot_json(),
        None => crate::stats::TrafficStats::zero_snapshot_json(),
    }
}

/// Tell the datapath that the phone's underlying network changed.
///
/// Called by the HarmonyOS VPN extension when the OS reports a connectivity
/// change (Wi-Fi ⇄ cellular, or a new Wi-Fi network). Everything established
/// over the old link is unusable from that moment on:
///
/// * TCP flows born in the previous epoch are reset so the apps retry on the
///   new link instead of waiting for their own timeouts;
/// * the shared DNS-over-tunnel flow is dropped so the next query rebuilds it;
/// * cached QUIC connections are discarded (they are bound to the old source
///   address);
/// * failover health counters are cleared, because failures recorded while the
///   radio was switching say nothing about the servers.
///
/// Returns the new epoch so the caller can log it.
pub fn android_notify_network_change() -> u64 {
    let epoch = crate::tun::bump_network_epoch();
    if let Some(dns) = SHARED_DNS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .cloned()
    {
        dns.set_tunnel_sender(None);
    }
    if let Some(failover) = SHARED_FAILOVER
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .cloned()
    {
        failover.reset_health();
    }
    if let Some(quic) = SHARED_QUIC
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .cloned()
    {
        // The pool is only mutated behind an async lock; a plain blocking
        // acquire here would need a runtime, so spawn the cleanup instead.
        let rt = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rt) = rt.as_ref() {
            rt.spawn(async move { quic.clear().await });
        }
    }
    if let Some(tcp_pool) = SHARED_TCP_POOL
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_ref()
        .cloned()
    {
        // Same reasoning as the QUIC pool: a handshaked session is bound to the
        // source address of the network that no longer exists.
        let rt = RUNTIME.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(rt) = rt.as_ref() {
            rt.spawn(async move { tcp_pool.clear().await });
        }
    }
    tracing::info!("network changed (epoch {epoch}): tunnel flows invalidated");
    epoch
}

/// Enable (`Some(path)`) or disable (`None`) the opt-in TUN trace.
///
/// Returns 0 on success, -1 when the file could not be created.
pub fn android_set_trace_path(path: Option<&str>) -> i32 {
    match crate::tun_trace::set_path(path) {
        Ok(()) => {
            tracing::info!(
                "TUN trace {}",
                match path {
                    Some(p) => format!("enabled -> {p}"),
                    None => "disabled".to_string(),
                }
            );
            0
        }
        Err(e) => {
            tracing::warn!("TUN trace could not open {:?}: {}", path, e);
            -1
        }
    }
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
        // The UI renders raw log text, so ANSI colour escapes would show up as
        // literal "[2m[32m" garbage. Keep the output plain.
        .with_ansi(false)
        .with_target(false)
        // The phone shows this in a narrow, wrapping log pane; a full RFC3339
        // timestamp eats half the line. ArkTS prepends a short local HH:MM:SS.
        .without_time()
        .with_writer(LogBufferWriter::new)
        .try_init();

    set_status(1); // starting
    clear_error();
    // Rebuild the routing state so the whitelist the UI edited applies on every
    // start, not only the first one: without this the process-wide router built
    // by the previous start would be reused and the new rules ignored.
    crate::whitelist::rebuild(&config);
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
        //
        // fd < 0 selects the SOCKS5-only mode: no TUN device exists (e.g.
        // environments where the OS VPN dialog is unavailable, such as the
        // HarmonyOS emulator which lacks the com.huawei.hmos.vpndialog
        // system bundle). The local SOCKS5 listener still proxies traffic
        // through the tunnel, which keeps the datapath verifiable.
        let device = if fd >= 0 {
            match crate::tun::TunDevice::from_fd(fd) {
                Ok(d) => Some(d),
                Err(e) => {
                    let msg = format!("TUN fd wrap failed: {}", e);
                    tracing::error!("{}", msg);
                    set_error(msg);
                    return;
                }
            }
        } else {
            tracing::info!("No TUN fd (fd < 0): running in SOCKS5-only mode");
            None
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

        // Publish for `android_notify_network_change`.
        *SHARED_FAILOVER.lock().unwrap_or_else(|e| e.into_inner()) =
            Some(std::sync::Arc::clone(&failover));

        // Shared counters: SOCKS5 and TUN traffic land in the same instance.
        let stats = crate::stats::TrafficStats::new();
        // Publish the counters so the UI can show live throughput.
        *TRAFFIC_STATS.lock().unwrap_or_else(|e| e.into_inner()) = Some(std::sync::Arc::clone(&stats));

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
        let stats_socks5 = std::sync::Arc::clone(&stats);
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

            let quic_pool = std::sync::Arc::new(crate::quic_pool::QuicPool::new());
            *SHARED_QUIC.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(std::sync::Arc::clone(&quic_pool));
            // Ready-to-use TCP sessions: on the phone the panel is opened in
            // bursts (a page load, an app launch), and each new flow would
            // otherwise pay a connect plus a Noise handshake first.
            let tcp_pool = std::sync::Arc::new(crate::tcp_pool::TcpSessionPool::new());
            // Age the pool out in the background: without this the age rules
            // only ever run when a flow asks, and a phone left idle overnight
            // would keep holding handshaked sockets the whole time.
            tcp_pool.spawn_sweeper();
            *SHARED_TCP_POOL.lock().unwrap_or_else(|e| e.into_inner()) =
                Some(std::sync::Arc::clone(&tcp_pool));
            // Warm one session up front: the first page after "connect" is when
            // a saved round trip is most visible.
            if let Some(server) = config_clone
                .servers
                .first()
                .filter(|s| s.protocol == phantom_core::TransportProtocol::Tcp)
                .cloned()
            {
                let cipher = phantom_core::CipherPreference::effective_for(
                    server.cipher,
                    config_clone.client.cipher,
                );
                // The pooled session is a datapath object, not a per-flow
                // identity: flows keep generating their own key pair below.
                match phantom_core::crypto::KeyPair::generate() {
                    Ok(kp) => tcp_pool.spawn_refill(server, kp.secret, cipher),
                    Err(e) => tracing::warn!("TCP pool prewarm key generation failed: {e}"),
                }
            }
            let stats = stats_socks5;
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
                let qp = std::sync::Arc::clone(&quic_pool);
                let tp = std::sync::Arc::clone(&tcp_pool);
                let st = std::sync::Arc::clone(&stats);
                tokio::spawn(async move {
                    let local_secret = match phantom_core::crypto::KeyPair::generate() {
                        Ok(kp) => kp.secret,
                        Err(_) => return,
                    };
                    if let Err(e) = crate::http_proxy::handle_inbound(
                        stream,
                        &cfg,
                        &fo,
                        &qp,
                        &tp,
                        local_secret,
                        &st,
                    )
                    .await
                    {
                        tracing::debug!("SOCKS5 connection error from {}: {}", peer, e);
                    }
                });
            }
        });

        // 3. Start TUN transparent proxy (skipped in SOCKS5-only mode).
        let tun_task = device.map(|device| {
            let config_tun = config.clone();
            let stats_tun = stats.clone();
            tokio::spawn(async move {
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
                crate::tun::TunProxy::new(device, socks5_addr)
                    .with_mode(config_tun.client.mode)
                    .with_stats(stats_tun)
                    .with_whitelist(crate::whitelist::shared(&config_tun).whitelist());

            if let Some(server) = config_tun.servers.first() {
                proxy = proxy.with_server(server.clone(), tun_secret);
            }

            if let Ok(engine) = crate::rules::RuleEngine::from_config(&config_tun.rules) {
                proxy = proxy.with_rules(engine);
                tracing::info!(
                    "Smart routing enabled with {} rules",
                    config_tun.rules.rules.len()
                );
            }

            let tunnel_dns = crate::dns::parse_dns_addr(&config_tun.client.dns);
            let direct_dns = crate::dns::parse_dns_addr(&config_tun.client.dns_direct);
            if let (Some(tunnel_dns), Some(direct_dns)) = (tunnel_dns, direct_dns) {
                match crate::dns::DnsProxy::new(tunnel_dns, direct_dns).await {
                    Ok(dns) => {
                        let dns = std::sync::Arc::new(dns);
                        *SHARED_DNS.lock().unwrap_or_else(|e| e.into_inner()) = Some(dns.clone());
                        proxy = proxy.with_dns(dns);
                        tracing::info!(
                            "DNS hijack enabled, tunnel resolver = {}, direct resolver = {}",
                            tunnel_dns,
                            direct_dns
                        );
                    }
                    Err(e) => {
                        tracing::warn!("DNS proxy init failed: {}", e);
                    }
                }
            } else {
                tracing::warn!(
                    "Invalid DNS config (client.dns = '{}', client.dns_direct = '{}'), DNS hijack disabled",
                    config_tun.client.dns,
                    config_tun.client.dns_direct
                );
            }

            tracing::info!("Android TUN proxy started on fd {}", fd);
            if let Err(e) = proxy.run().await {
                let msg = format!("TUN proxy exited: {}", e);
                tracing::error!("{}", msg);
                set_error(msg);
            }
            })
        });

        // In SOCKS5-only mode there is no TUN task; just await the SOCKS5 side.
        match tun_task {
            Some(t) => {
                let _ = tokio::try_join!(socks5_task, t);
            }
            None => {
                let _ = socks5_task.await;
            }
        }
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
    // Drop the shared datapath handles with the runtime: they belong to the
    // tunnel that just stopped, and a stale DNS/QUIC handle would be applied to
    // the next session's network change.
    *SHARED_DNS.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *SHARED_QUIC.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *SHARED_TCP_POOL.lock().unwrap_or_else(|e| e.into_inner()) = None;
    *SHARED_FAILOVER.lock().unwrap_or_else(|e| e.into_inner()) = None;
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

// The JNI wrappers that Kotlin calls live in the `phantom-android` crate
// (`client/android/rust`), next to the embedded-server bridge, so this crate
// keeps a single platform-independent surface shared with the HarmonyOS NAPI
// bindings.

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
}
