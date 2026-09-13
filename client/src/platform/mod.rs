#[cfg(any(
    target_os = "android",
    target_env = "ohos",
    // Include the Android platform implementation on host Unix systems so that
    // `cargo check -p phantom-harmony` and other host-side validation can run.
    // JNI symbols inside the module remain gated to `target_os = "android"`.
    target_os = "macos",
    target_os = "linux"
))]
pub mod android;

#[cfg(target_os = "macos")]
pub mod macos;

/// Exempt a socket from the VPN interface so it leaves over the physical link.
///
/// Returns `true` when the socket is known to bypass the tunnel: on Android
/// that means `VpnService.protect()` accepted it, and on every other platform
/// no exemption is needed at all. `false` means the caller should expect its
/// packets to be captured by the TUN.
#[cfg(unix)]
pub fn protect_fd(fd: std::os::unix::io::RawFd) -> bool {
    android::protect_fd(fd)
}

/// Non-unix hosts have no VpnService to defer to.
#[cfg(not(unix))]
pub fn protect_fd(_fd: i32) -> bool {
    true
}
