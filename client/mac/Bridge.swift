import Foundation

/// C FFI bridge to the Rust `phantom-client` cdylib.
///
/// Build the Rust library first:
///   cargo build --release -p phantom-client --lib
/// Then link `libphantom_client.dylib` in the Xcode project.

func phantomMacosStart(configToml: String) -> Int32 {
    var result: Int32 = -1
    configToml.withCString { cStr in
        result = phantom_macos_start(UnsafePointer<UInt8>(cStr), configToml.utf8.count)
    }
    return result
}

func phantomMacosStop() -> Int32 {
    return phantom_macos_stop()
}

// MARK: - C FFI Declarations

@_silgen_name("phantom_macos_start")
func phantom_macos_start(_ config: UnsafePointer<UInt8>?, _ len: Int) -> Int32

@_silgen_name("phantom_macos_stop")
func phantom_macos_stop() -> Int32
