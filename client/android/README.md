# Phantom Android Client

Native Android VPN client.  The tunnel engine runs entirely in Rust; Kotlin is a thin VpnService shell.

## Architecture

```
Kotlin (VpnService)  ──JNI──►  Rust cdylib (phantom-client)
  ├─ establish TUN              ├─ TUN fd I/O (AsyncFd)
  ├─ detachFd()                 ├─ Packet parser / NAT
  └─ Start/Stop UI              ├─ SOCKS5 proxy
                                └─ QUIC/TCP tunnel
```

**Zero per-packet JNI**: Kotlin passes the fd once via `phantom_android_start(fd, config)`, then Rust handles all reads/writes independently.

## Build

### 1. Build Rust cdylib for Android

```bash
cd ../../

# Install Android targets (one-time)
rustup target add aarch64-linux-android

# Build
export ANDROID_NDK_HOME=$HOME/Library/Android/sdk/ndk/26.1.10909125
CARGO_TARGET_AARCH64_LINUX_ANDROID_LINKER=$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/darwin-x86_64/bin/aarch64-linux-android34-clang \
  cargo build --release -p phantom-client --lib --target aarch64-linux-android

# Output: target/aarch64-linux-android/release/libphantom_client.so
```

### 2. Open in Android Studio

1. Open `client/android/` in Android Studio.
2. Copy `libphantom_client.so` to `app/src/main/jniLibs/arm64-v8a/`.
3. Sync Gradle and run on device.

### 3. First Run

The app requests VPN permission on first start. Grant it in the system dialog.

## Project Structure

```
android/
├── app/
│   └── src/main/java/co/phantom/android/
│       ├── MainActivity.kt          # Jetpack Compose UI
│       ├── PhantomVpnService.kt     # VpnService + fd handoff
│       └── RustBridge.kt            # JNI declarations
│   └── build.gradle.kts
└── build.gradle.kts
```

## Notes

- `minSdk = 28` (Android 9).  `VpnService` has been stable since API 14.
- The Rust library name must match `System.loadLibrary("phantom_client")`.
- TUN fd ownership is transferred to Rust; do **not** close it from Kotlin after `detachFd()`.
