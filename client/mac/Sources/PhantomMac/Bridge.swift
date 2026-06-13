import Foundation

/// C FFI bridge to the Rust `phantom-client` cdylib.
///
/// Build the Rust library first:
///   cargo build --release -p phantom-client --lib
/// Then link `libphantom_client.dylib` in the Xcode project.

func phantomMacosStartWithURI(_ uri: String, _ mode: String) -> Int32 {
    var result: Int32 = -1
    let combined = "\(uri)|\(mode)"
    let byteCount = combined.utf8.count
    combined.withCString { cStr in
        // `cStr` is `UnsafePointer<Int8>` (i.e. `CChar`). The Rust FFI takes
        // `*const u8`; re-bind the same memory as `UInt8` for the call.
        cStr.withMemoryRebound(to: UInt8.self, capacity: byteCount) { uintPtr in
            result = phantom_macos_start_with_uri(uintPtr, byteCount)
        }
    }
    return result
}

func phantomMacosStop() -> Int32 {
    return phantom_macos_stop()
}

/// Returns the local SOCKS5 listen port that the Rust side is using.
/// Swift uses this to set the system proxy to the correct port.
func phantomMacosSocks5Port() -> UInt16 {
    return phantom_macos_get_socks5_port()
}

// MARK: - C FFI Declarations

@_silgen_name("phantom_macos_start_with_uri")
func phantom_macos_start_with_uri(_ input: UnsafePointer<UInt8>?, _ len: Int) -> Int32

@_silgen_name("phantom_macos_stop")
func phantom_macos_stop() -> Int32

@_silgen_name("phantom_macos_get_socks5_port")
func phantom_macos_get_socks5_port() -> UInt16
