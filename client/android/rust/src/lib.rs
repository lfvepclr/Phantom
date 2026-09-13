// The JNI surface only exists when compiled for Android; the host build is an
// empty cdylib so `cargo check --workspace` stays green on a developer Mac.
#![cfg(target_os = "android")]

//! Phantom Android JNI bindings.
//!
//! This crate is the Android twin of `client/harmony/rust`: it holds every
//! symbol the platform shell calls, while the tunnel itself lives in
//! `phantom-client`. Keeping the two apart is what lets `phantom-client` stay
//! free of a `phantom-server` dependency — the embedded server is an Android
//! integration concern, not a client-core one.
//!
//! All functions here are safe Rust: they delegate to the safe wrappers in
//! `phantom_client::platform::android`, so this layer contains no handwritten
//! `unsafe` outside the `#[no_mangle]` FFI boundary itself.

use std::sync::Mutex;
use std::sync::atomic::{AtomicI32, Ordering};

use jni::JNIEnv;
use jni::objects::{JClass, JString};
use jni::sys::{jint, jlong};
use phantom_client::platform::android as phantom_android;
use phantom_core::{CipherPreference, TransportProtocol};
use std::path::PathBuf;

/// Read a Java string, or `None` when the argument is null or not valid UTF-8.
fn opt_string(env: &mut JNIEnv, value: &JString) -> Option<String> {
    if value.is_null() {
        return None;
    }
    env.get_string(value)
        .ok()
        .map(|s| s.to_str().unwrap_or("").to_string())
}

/// Read a Java string, falling back to `default` when it is null.
fn string_or(env: &mut JNIEnv, value: &JString, default: &str) -> String {
    opt_string(env, value).unwrap_or_else(|| default.to_string())
}

// ---------------------------------------------------------------------------
// Tunnel
// ---------------------------------------------------------------------------

/// Start the tunnel on a TUN fd obtained from `VpnService.Builder.establish()`.
///
/// Also captures the JVM + class handle the datapath later needs to call
/// `VpnService.protect()` on the sockets it opens outside the tunnel.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_startTunnelWithURI<'local>(
    mut env: JNIEnv<'local>,
    class: JClass<'local>,
    fd: jint,
    uri: JString<'local>,
    mode: JString<'local>,
) -> jint {
    phantom_android::remember_bridge(&mut env, &class);
    let uri = string_or(&mut env, &uri, "");
    let mode = string_or(&mut env, &mode, "smart");
    phantom_android::android_start_with_uri(fd as std::os::unix::io::RawFd, &uri, &mode)
}

/// Start the tunnel from a TOML config string (legacy entry point).
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_startTunnel<'local>(
    mut env: JNIEnv<'local>,
    class: JClass<'local>,
    fd: jint,
    config: JString<'local>,
) -> jint {
    phantom_android::remember_bridge(&mut env, &class);
    let config = string_or(&mut env, &config, "");
    phantom_android::android_start(fd as std::os::unix::io::RawFd, &config)
}

/// Stop the tunnel.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_stopTunnel(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    phantom_android::android_stop()
}

/// Tunnel status: 0 idle, 1 starting, 2 running, 3 error.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_getStatus(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    phantom_android::android_get_status()
}

/// Last error message, or an empty string.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_getLastError<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> JString<'local> {
    env.new_string(phantom_android::android_get_last_error())
        .expect("new_string failed")
}

/// New log lines after `since_cursor`, formatted `<cursor>\n<line>...`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_getLogsNative<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
    since_cursor: jlong,
) -> JString<'local> {
    let (lines, cursor) = phantom_android::android_get_logs(since_cursor as u64);
    let result = format!("{}\n{}", cursor, lines.join("\n"));
    env.new_string(result).expect("new_string failed")
}

/// Drop every buffered log line, in memory and on disk.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_clearLogs(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    phantom_android::android_clear_logs()
}

/// Live traffic counters as JSON (`up`/`down`/`udp_*`/`conns`/`route_*`).
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_getStatsJson<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> JString<'local> {
    env.new_string(phantom_android::android_get_stats_json())
        .expect("new_string failed")
}

/// Tell the datapath the phone's underlying network changed; returns the epoch.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_notifyNetworkChange(
    _env: JNIEnv,
    _class: JClass,
) -> jlong {
    phantom_android::android_notify_network_change() as jlong
}

/// Point the on-disk log mirror at `path`; pass null to disable.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_setLogPath<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) -> jint {
    match opt_string(&mut env, &path) {
        Some(p) => phantom_android::android_set_log_path(Some(&p)),
        None => phantom_android::android_set_log_path(None),
    }
}

/// Enable (`path`) or disable (`null`) the opt-in TUN trace file.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_setTracePath<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    path: JString<'local>,
) -> jint {
    match opt_string(&mut env, &path) {
        Some(p) => phantom_android::android_set_trace_path(Some(&p)),
        None => phantom_android::android_set_trace_path(None),
    }
}

/// Replace the user "分流白名单" rules (one `kind:value` per line, see
/// `whitelist::USER_RULE_FORMAT_HELP`).
///
/// Must be called before `startTunnelWithURI`: routing state is built once per
/// start, so a rule set handed over afterwards would wait for the next one.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_setUserRules<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    text: JString<'local>,
) -> jint {
    match opt_string(&mut env, &text) {
        Some(t) => phantom_android::android_set_user_rules(&t),
        None => phantom_android::android_set_user_rules(""),
    }
}

// ---------------------------------------------------------------------------
// Embedded server (phone-as-server)
//
// Mirrors `phantom_harmony_server_*`: bootstrap keys/config into the app
// sandbox (`work_dir`), spawn the listener on a dedicated runtime, and hand
// back the `phantom://` URI so the UI can render it as text or QR.
// ---------------------------------------------------------------------------

/// Server lifecycle: 0 idle, 1 starting, 2 running, 3 error.
static SERVER_STATUS: AtomicI32 = AtomicI32::new(0);
static SERVER_LAST_ERROR: Mutex<String> = Mutex::new(String::new());
static SERVER_STATE: Mutex<Option<ServerHandle>> = Mutex::new(None);
/// The `phantom://` URI handed out by the last successful `serverStart`.
///
/// It lives here rather than only in the UI because the server outlives the
/// page that started it: popping and re-opening that page must still be able
/// to show the code that is currently being served.
static SERVER_URI: Mutex<String> = Mutex::new(String::new());

struct ServerHandle {
    runtime: tokio::runtime::Runtime,
    /// Shutdown channel for the running listener; `None` while stopped (the
    /// runtime itself is kept so a restart does not pay for a second one).
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

fn lock_state() -> std::sync::MutexGuard<'static, Option<ServerHandle>> {
    SERVER_STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_server_error(msg: String) {
    *SERVER_LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = msg;
    SERVER_STATUS.store(3, Ordering::SeqCst);
}

fn set_server_uri(uri: String) {
    *SERVER_URI.lock().unwrap_or_else(|e| e.into_inner()) = uri;
}

/// Start the embedded server; returns the `phantom://` URI, or an empty string
/// on failure (the reason is available from `serverLastError`).
///
/// * `work_dir` — app sandbox directory (e.g. `context.filesDir`).
/// * `port` — first port to try; `0` means the default (443).
/// * `cipher` — `auto` / `aes-256-gcm` / `aes-128-gcm` / `chacha20-poly1305`.
/// * `proto` — `tcp` or `quic`.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_serverStart<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    work_dir: JString<'local>,
    port: jint,
    cipher: JString<'local>,
    proto: JString<'local>,
) -> JString<'local> {
    let work_dir = string_or(&mut env, &work_dir, "");
    let cipher = string_or(&mut env, &cipher, "auto");
    let proto = string_or(&mut env, &proto, "tcp");

    let uri = match start_server_inner(&work_dir, port.max(0) as u32, &cipher, &proto) {
        Ok(uri) => {
            SERVER_STATUS.store(2, Ordering::SeqCst);
            set_server_uri(uri.clone());
            uri
        }
        Err(e) => {
            set_server_uri(String::new());
            set_server_error(e.clone());
            tracing::error!("embedded server failed to start: {e}");
            String::new()
        }
    };
    env.new_string(uri).expect("new_string failed")
}

fn start_server_inner(
    work_dir: &str,
    port: u32,
    cipher: &str,
    proto: &str,
) -> Result<String, String> {
    // Only idle (0) or error (3) may start a new server.
    let prev = SERVER_STATUS.swap(1, Ordering::SeqCst);
    if prev == 1 || prev == 2 {
        SERVER_STATUS.store(prev, Ordering::SeqCst);
        return Err("server is already running".to_string());
    }
    if work_dir.is_empty() {
        SERVER_STATUS.store(0, Ordering::SeqCst);
        return Err("work_dir must be the app sandbox directory".to_string());
    }
    let start_port = match port {
        0 => None,
        p => Some(u16::try_from(p).map_err(|_| format!("port {p} out of range"))?),
    };
    let cipher = match cipher {
        "" | "auto" => None,
        "aes-256-gcm" => Some(CipherPreference::Aes256Gcm),
        "aes-128-gcm" => Some(CipherPreference::Aes128Gcm),
        "ascon-128" => Some(CipherPreference::Ascon128),
        "chacha20-poly1305" => Some(CipherPreference::ChaCha20Poly1305),
        other => {
            SERVER_STATUS.store(0, Ordering::SeqCst);
            return Err(format!("unknown cipher: {other}"));
        }
    };
    let protocol = match proto {
        "" | "tcp" => None,
        "quic" => Some(TransportProtocol::Quic),
        other => {
            SERVER_STATUS.store(0, Ordering::SeqCst);
            return Err(format!("unknown protocol: {other}"));
        }
    };

    // A tunnel may never have been started, in which case no subscriber exists
    // yet and the server's own INFO logs would go nowhere.
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .with_ansi(false)
        .with_target(false)
        .try_init();

    let opts = phantom_server::bootstrap::AutoOptions {
        // Auto-detection yields the Wi-Fi address, which is what the page shows.
        public_host: None,
        start_port,
        cipher,
        protocol,
        max_port_tries: None,
        work_dir: Some(PathBuf::from(work_dir)),
    };

    let result = (|| -> Result<String, String> {
        let mut guard = lock_state();
        if guard.is_none() {
            let runtime = tokio::runtime::Runtime::new()
                .map_err(|e| format!("failed to create tokio runtime: {e}"))?;
            *guard = Some(ServerHandle {
                runtime,
                shutdown: None,
            });
        }
        let handle = guard.as_mut().expect("server handle just ensured");

        // Key load/generation, port probing and server.toml write are fast local
        // operations; run them synchronously so the URI is ready when the call
        // returns and the page can render it immediately.
        let prepared = handle
            .runtime
            .block_on(phantom_server::bootstrap::prepare_auto(&opts))
            .map_err(|e| format!("bootstrap failed: {e:#}"))?;
        let uri = prepared.uri.clone();

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        handle.runtime.spawn(async move {
            let result = phantom_server::run_with_shutdown(prepared.options, async {
                let _ = rx.await;
            })
            .await;
            // Still RUNNING here means the listener died on its own: a stop
            // request flips the status to idle before signalling.
            if SERVER_STATUS.swap(0, Ordering::SeqCst) == 2 {
                set_server_uri(String::new());
                if let Err(e) = result {
                    set_server_error(format!("server exited unexpectedly: {e:#}"));
                }
            }
        });
        handle.shutdown = Some(tx);
        Ok(uri)
    })();

    if result.is_err() {
        SERVER_STATUS.store(0, Ordering::SeqCst);
    }
    result
}

/// Stop the embedded server. Returns 0 on success, 1 if it was not running.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_serverStop(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    let mut guard = lock_state();
    match guard.as_mut().and_then(|h| h.shutdown.take()) {
        Some(tx) => {
            // Flip to idle *before* signalling so the exiting task treats this
            // as a requested shutdown rather than a crash.
            SERVER_STATUS.store(0, Ordering::SeqCst);
            set_server_uri(String::new());
            let _ = tx.send(());
            0
        }
        None => 1,
    }
}

/// Embedded server status: 0 idle, 1 starting, 2 running, 3 error.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_serverStatus(
    _env: JNIEnv,
    _class: JClass,
) -> jint {
    SERVER_STATUS.load(Ordering::SeqCst)
}

/// Last embedded-server error, or an empty string.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_serverLastError<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> JString<'local> {
    let msg = SERVER_LAST_ERROR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone();
    env.new_string(msg).expect("new_string failed")
}

/// The `phantom://` URI the embedded server is currently serving, or an empty
/// string when it is stopped.
///
/// This is the recovery path for a page that was popped and re-opened while
/// the server kept running: `serverStart` only ever returns the URI once, and
/// that return value dies with the composition that received it.
#[unsafe(no_mangle)]
pub extern "system" fn Java_co_phantom_android_RustBridge_serversUri<'local>(
    env: JNIEnv<'local>,
    _class: JClass<'local>,
) -> JString<'local> {
    let uri = SERVER_URI.lock().unwrap_or_else(|e| e.into_inner()).clone();
    env.new_string(uri).expect("new_string failed")
}
