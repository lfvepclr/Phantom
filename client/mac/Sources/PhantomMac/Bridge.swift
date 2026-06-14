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

/// Fetch new log lines from the Rust log buffer since the given cursor.
/// Returns a tuple of (lines, new_cursor). Pass 0 as cursor to get all logs.
func phantomMacosGetLogs(sinceCursor: UInt64) -> (lines: [String], cursor: UInt64) {
    guard let cStr = phantom_macos_get_logs(sinceCursor) else {
        return ([], sinceCursor)
    }
    defer { phantom_macos_free_logs(cStr) }
    let full = String(cString: cStr)
    // Format: "<cursor>\n<line1>\n<line2>..."
    let parts = full.split(separator: "\n", maxSplits: 1, omittingEmptySubsequences: false)
    let newCursor = parts.first.flatMap { UInt64($0) } ?? sinceCursor
    let lines = parts.dropFirst().first?
        .split(separator: "\n", omittingEmptySubsequences: true)
        .map(String.init) ?? []
    return (lines, newCursor)
}

/// Tunnel lifecycle status mirrored from Rust.
enum PhantomTunnelStatus: Int32 {
    case idle = 0
    case starting = 1
    case running = 2
    case error = 3
}

/// Returns the current tunnel lifecycle status (idle/starting/running/error).
func phantomMacosGetStatus() -> PhantomTunnelStatus {
    let code = phantom_macos_get_status()
    return PhantomTunnelStatus(rawValue: code) ?? .error
}

/// Returns the last error message when status is `.error`, or `nil`.
func phantomMacosGetLastError() -> String? {
    guard let cStr = phantom_macos_get_last_error() else { return nil }
    defer { phantom_macos_free_logs(cStr) }
    let msg = String(cString: cStr)
    return msg.isEmpty ? nil : msg
}

// MARK: - C FFI Declarations

@_silgen_name("phantom_macos_start_with_uri")
func phantom_macos_start_with_uri(_ input: UnsafePointer<UInt8>?, _ len: Int) -> Int32

@_silgen_name("phantom_macos_stop")
func phantom_macos_stop() -> Int32

@_silgen_name("phantom_macos_get_socks5_port")
func phantom_macos_get_socks5_port() -> UInt16

@_silgen_name("phantom_macos_get_logs")
func phantom_macos_get_logs(_ sinceCursor: UInt64) -> UnsafeMutablePointer<CChar>?

@_silgen_name("phantom_macos_free_logs")
func phantom_macos_free_logs(_ ptr: UnsafeMutablePointer<CChar>)

@_silgen_name("phantom_macos_get_status")
func phantom_macos_get_status() -> Int32

@_silgen_name("phantom_macos_get_last_error")
func phantom_macos_get_last_error() -> UnsafeMutablePointer<CChar>?
