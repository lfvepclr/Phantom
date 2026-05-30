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

    private var systemProxy: SystemProxy?

    func start() {
        guard !isRunning else { return }

        let modeString: String
        switch proxyMode {
        case .global: modeString = "proxy"
        case .smart:  modeString = "smart"
        case .direct: modeString = "direct"
        }
        let config = """
        [[servers]]
        name = "default"
        address = "127.0.0.1:443"
        public_key = ""

        [client]
        listen = "127.0.0.1:11080"
        mode = "\(modeString)"
        """

        status = "Starting..."
        let rc = phantomMacosStart(configToml: config)
        if rc == 0 {
            isRunning = true
            status = "Connected"
            // Enable system SOCKS5 proxy.
            var sp = SystemProxy(host: "127.0.0.1", port: 11080)
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
