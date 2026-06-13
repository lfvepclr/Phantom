import Foundation
import Combine

enum ProxyMode: String, CaseIterable {
    case global = "Global"
    case smart = "Auto"
    case direct = "Direct"
}

/// Manages the Phantom tunnel lifecycle on macOS.
///
/// The actual tunnel (utun creation, packet processing, encryption)
/// runs entirely inside the Rust cdylib.  This Swift class is purely
/// a control-plane wrapper.
class PhantomTunnel: ObservableObject {
    @Published var isRunning = false
    @Published var status = "Idle"
    @Published var proxyMode: ProxyMode = .smart
    /// Server URI (phantom://<base64_key>@host:port?cipher=auto#name)
    @Published var serverURI: String = ""

    private var systemProxy: SystemProxy?

    func start() {
        guard !isRunning else { return }

        guard !serverURI.isEmpty else {
            status = "Error: server URI required"
            return
        }

        let modeString: String
        switch proxyMode {
        case .global: modeString = "proxy"
        case .smart:  modeString = "smart"
        case .direct: modeString = "direct"
        }

        status = "Starting..."
        let rc = phantomMacosStartWithURI(serverURI, modeString)
        if rc == 0 {
            isRunning = true
            status = "Connected"
            // Enable system SOCKS5 proxy on the same port Rust is listening on.
            let port = Int(phantomMacosSocks5Port())
            var sp = SystemProxy(host: "127.0.0.1", port: port)
            sp?.enable()
            systemProxy = sp
        } else {
            status = "Start failed (\(rc))"
        }
    }

    func stop() {
        guard isRunning else { return }
        // Restore system proxy before stopping tunnel.
        systemProxy?.disable()
        systemProxy = nil
        let _ = phantomMacosStop()
        isRunning = false
        status = "Stopped"
    }
}
