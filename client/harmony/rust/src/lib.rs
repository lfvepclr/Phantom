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
