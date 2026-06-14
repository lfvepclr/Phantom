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
