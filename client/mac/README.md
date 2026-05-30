# Phantom macOS Client

Native macOS menu-bar client.  The tunnel engine runs entirely in Rust; SwiftUI is a thin control shell.

## Architecture

```
SwiftUI (Menu Bar)  <->  C FFI  <->  Rust cdylib (phantom-client)
                                      ├─ TUN device (utun)
                                      ├─ Packet parser / NAT
                                      ├─ SOCKS5 proxy
                                      └─ QUIC/TCP tunnel
```

## Build

### 1. Build Rust cdylib

```bash
cd ../../
cargo build --release -p phantom-client --lib
# outputs target/release/libphantom_client.dylib
```

### 2. Create Xcode project

Open Xcode and create a new **macOS App**:
- **Interface**: SwiftUI
- **Language**: Swift
- **Minimum Deployments**: macOS 13.0 (for MenuBarExtra)

### 3. Link Rust library

1. Drag `target/release/libphantom_client.dylib` into the Xcode project.
2. In **Build Phases** > **Link Binary With Libraries**, add `libphantom_client.dylib`.
3. In **Build Settings** > **Runpath Search Paths**, add:
   ```
   @executable_path/../Frameworks
   @loader_path
   ```

### 4. Add Swift source files

Add the following files to the Xcode project:
- `PhantomMacApp.swift`
- `Bridge.swift`
- `PhantomTunnel.swift`

### 5. Run

Press **Run** in Xcode. The app appears as a lightning-bolt icon in the menu bar.

## Notes

- The Rust cdylib must be rebuilt manually after each Rust code change.
- `LSUIElement = true` hides the app from Dock; only the menu bar icon is shown.
- TUN device creation requires root or the `com.apple.vm.networking` entitlement.
