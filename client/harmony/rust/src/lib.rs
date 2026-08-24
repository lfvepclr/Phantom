//! Phantom HarmonyOS NAPI bindings.
//!
//! This crate exposes the same Rust tunnel core to ArkTS via the NAPI ABI.
//! It intentionally mirrors the Android FFI surface so that the two clients
//! stay in sync.
//!
//! All functions here are safe Rust: they delegate to the safe wrappers in
//! `phantom_client::platform::android`, so the NAPI layer itself contains no
//! handwritten `unsafe` blocks.

use napi_derive_ohos::napi;
use phantom_client::platform::android as phantom_android;
use phantom_core::{CipherPreference, TransportProtocol};
use std::path::PathBuf;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard};

/// Start the tunnel with a TUN fd obtained from VpnExtensionAbility.
///
/// `uri` must be a valid `phantom://` connection string.
/// `mode` must be one of `proxy`, `smart`, or `direct`.
#[napi]
pub fn phantom_harmony_start(fd: i32, uri: String, mode: String) -> i32 {
    phantom_android::android_start_with_uri(fd as std::os::unix::io::RawFd, &uri, &mode)
}

/// Start the tunnel with a TOML config string.
#[napi]
pub fn phantom_harmony_start_config(fd: i32, config: String) -> i32 {
    phantom_android::android_start(fd as std::os::unix::io::RawFd, &config)
}

/// Stop the tunnel.
#[napi]
pub fn phantom_harmony_stop() -> i32 {
    phantom_android::android_stop()
}

/// Return the current tunnel status: 0 idle, 1 starting, 2 running, 3 error.
#[napi]
pub fn phantom_harmony_get_status() -> i32 {
    phantom_android::android_get_status()
}

/// Return the last error message, or an empty string if none.
#[napi]
pub fn phantom_harmony_get_last_error() -> String {
    phantom_android::android_get_last_error()
}

/// Fetch log lines appended after `since_cursor`.
/// Returns a tuple `(lines, new_cursor)`.
#[napi]
pub fn phantom_harmony_get_logs(since_cursor: i64) -> (Vec<String>, i64) {
    let (lines, cursor) = phantom_android::android_get_logs(since_cursor as u64);
    (lines, cursor as i64)
}

// ---------------------------------------------------------------------------
// Embedded server (phone-as-server)
//
// The HarmonyOS app can also run the Phantom *server* on the phone itself:
// `phantom_harmony_server_start` bootstraps keys/config into the app sandbox
// (`work_dir`), spawns the listener on a dedicated Tokio runtime, and returns
// the `phantom://` quick-link URI (with PSK) so the ArkTS page can render it
// as text / QR for other devices to import.
// ---------------------------------------------------------------------------

/// Server lifecycle: 0 idle, 1 starting, 2 running, 3 error.
/// Mirrors the client-side status contract so ArkTS can share UI logic.
static SERVER_STATUS: AtomicI32 = AtomicI32::new(0);
static SERVER_LAST_ERROR: Mutex<String> = Mutex::new(String::new());
static SERVER_STATE: Mutex<Option<ServerHandle>> = Mutex::new(None);

struct ServerHandle {
    runtime: tokio::runtime::Runtime,
    /// Shutdown channel for the currently running listener; `None` while
    /// the server is stopped (the runtime itself is kept for reuse).
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

fn napi_err(msg: impl Into<String>) -> napi_ohos::Error {
    napi_ohos::Error::new(napi_ohos::Status::GenericFailure, msg.into())
}

fn lock_state() -> MutexGuard<'static, Option<ServerHandle>> {
    SERVER_STATE.lock().unwrap_or_else(|e| e.into_inner())
}

fn set_server_error(msg: String) {
    *SERVER_LAST_ERROR.lock().unwrap_or_else(|e| e.into_inner()) = msg;
    SERVER_STATUS.store(3, Ordering::SeqCst);
}

/// Start the embedded server.
///
/// * `work_dir` — app sandbox directory (e.g. `context.filesDir`); the
///   generated `server.key` / `server.toml` live there. Must be non-empty.
/// * `port` — first port to try; `0` means the default (443).
/// * `cipher` — `"auto"` / `"aes-256-gcm"` / `"aes-128-gcm"` /
///   `"chacha20-poly1305"` (empty = auto).
/// * `proto` — `"tcp"` or `"quic"` (empty = tcp).
///
/// Returns the `phantom://` URI once the listener is up. Throws on error.
#[napi]
pub fn phantom_harmony_server_start(
    work_dir: String,
    port: u32,
    cipher: String,
    proto: String,
) -> napi_ohos::Result<String> {
    // Only idle (0) or error (3) may start a new server.
    let prev = SERVER_STATUS.swap(1, Ordering::SeqCst);
    if prev == 1 || prev == 2 {
        SERVER_STATUS.store(prev, Ordering::SeqCst);
        return Err(napi_err("server is already running"));
    }

    match start_server_inner(&work_dir, port, &cipher, &proto) {
        Ok(uri) => {
            SERVER_STATUS.store(2, Ordering::SeqCst);
            Ok(uri)
        }
        Err(e) => {
            set_server_error(e.to_string());
            Err(e)
        }
    }
}

fn start_server_inner(
    work_dir: &str,
    port: u32,
    cipher: &str,
    proto: &str,
) -> napi_ohos::Result<String> {
    if work_dir.is_empty() {
        return Err(napi_err(
            "work_dir must be the app sandbox directory (e.g. context.filesDir)",
        ));
    }
    let start_port = match port {
        0 => None,
        p => Some(
            u16::try_from(p).map_err(|_| napi_err(format!("port {p} out of range")))?,
        ),
    };
    let cipher = match cipher {
        "" | "auto" => None,
        "aes-256-gcm" => Some(CipherPreference::Aes256Gcm),
        "aes-128-gcm" => Some(CipherPreference::Aes128Gcm),
        "ascon-128" => Some(CipherPreference::Ascon128),
        "chacha20-poly1305" => Some(CipherPreference::ChaCha20Poly1305),
        other => return Err(napi_err(format!("unknown cipher: {other}"))),
    };
    let protocol = match proto {
        "" | "tcp" => None,
        "quic" => Some(TransportProtocol::Quic),
        other => return Err(napi_err(format!("unknown protocol: {other}"))),
    };

    // Make sure a tracing subscriber exists even if the VPN tunnel was
    // never started (it installs its own log-capturing subscriber).
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let opts = phantom_server::bootstrap::AutoOptions {
        // Public host auto-detection yields the LAN/WiFi address here,
        // which is exactly what the server page displays.
        public_host: None,
        start_port,
        cipher,
        protocol,
        max_port_tries: None,
        work_dir: Some(PathBuf::from(work_dir)),
    };

    let mut guard = lock_state();
    if guard.is_none() {
        let runtime = tokio::runtime::Runtime::new()
            .map_err(|e| napi_err(format!("failed to create tokio runtime: {e}")))?;
        *guard = Some(ServerHandle {
            runtime,
            shutdown: None,
        });
    }
    let handle = guard.as_mut().expect("server handle just ensured");

    // Key load/generation, port probing and server.toml write are fast
    // local operations; run them synchronously so the URI is available
    // to the UI as soon as this call returns.
    let prepared = handle
        .runtime
        .block_on(phantom_server::bootstrap::prepare_auto(&opts))
        .map_err(|e| napi_err(format!("bootstrap failed: {e:#}")))?;
    let uri = prepared.uri.clone();

    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    handle.runtime.spawn(async move {
        let result = phantom_server::run_with_shutdown(prepared.options, async {
            let _ = rx.await;
        })
        .await;
        // If the status is still RUNNING at exit, the listener died on its
        // own (a stop request flips it to IDLE before signalling).
        if SERVER_STATUS.swap(0, Ordering::SeqCst) == 2 {
            if let Err(e) = result {
                set_server_error(format!("server exited unexpectedly: {e:#}"));
            }
        }
    });
    handle.shutdown = Some(tx);

    Ok(uri)
}

/// Stop the embedded server. Returns 0 on success, 1 if it was not running.
#[napi]
pub fn phantom_harmony_server_stop() -> i32 {
    let mut guard = lock_state();
    match guard.as_mut().and_then(|h| h.shutdown.take()) {
        Some(tx) => {
            // Flip to idle *before* signalling so the exiting task treats
            // this as a requested shutdown rather than a crash.
            SERVER_STATUS.store(0, Ordering::SeqCst);
            let _ = tx.send(());
            0
        }
        None => 1,
    }
}

/// Return the embedded server status: 0 idle, 1 starting, 2 running, 3 error.
#[napi]
pub fn phantom_harmony_server_status() -> i32 {
    SERVER_STATUS.load(Ordering::SeqCst)
}

/// Return the last server-side error message, or an empty string if none.
#[napi]
pub fn phantom_harmony_server_last_error() -> String {
    SERVER_LAST_ERROR
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}
