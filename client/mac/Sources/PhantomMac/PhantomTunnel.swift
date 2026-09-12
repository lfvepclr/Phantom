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
/// a control-plane wrapper that polls the real Rust state machine.
@MainActor
class PhantomTunnel: ObservableObject {
    @Published var isRunning = false
    @Published var status = "Idle"
    /// Proxy mode, persisted so a restart keeps the operator's choice.
    @Published var proxyMode: ProxyMode = .smart {
        didSet {
            UserDefaults.standard.set(proxyMode.rawValue, forKey: Self.modeKey)
        }
    }
    /// Server URI (phantom://<base64_key>@host:port?cipher=auto#name)
    /// Persisted in UserDefaults: the URI is long and pasting it on every launch
    /// is the difference between "usable menu-bar app" and "annoying one".
    @Published var serverURI: String = "" {
        didSet {
            UserDefaults.standard.set(serverURI, forKey: Self.uriKey)
        }
    }
    /// Connection log lines from Rust.
    @Published var logs: [String] = []

    /// Extra proxy-whitelist entries (one domain per line). Persisted so the
    /// operator maintains it once; the built-in censored-domain list is always
    /// active on top of these.
    @Published var proxyDomainsText: String = "" {
        didSet {
            UserDefaults.standard.set(proxyDomainsText, forKey: Self.domainsKey)
        }
    }

    private static let uriKey = "phantom.serverURI"
    private static let modeKey = "phantom.proxyMode"
    private static let domainsKey = "phantom.proxyDomains"

    private var systemProxy: SystemProxy?
    private var logTimer: Timer?
    private var statusTimer: Timer?
    private var logCursor: UInt64 = 0

    init() {
        // Restore the last used server URI / mode (empty when never configured).
        let defaults = UserDefaults.standard
        if let savedURI = defaults.string(forKey: Self.uriKey), !savedURI.isEmpty {
            serverURI = savedURI
        }
        if let raw = defaults.string(forKey: Self.modeKey),
           let savedMode = ProxyMode(rawValue: raw) {
            proxyMode = savedMode
        }
        if let savedDomains = defaults.string(forKey: Self.domainsKey) {
            proxyDomainsText = savedDomains
        }
    }

    /// Start the tunnel.  This does not immediately set `isRunning = true`;
    /// we wait until Rust reports `running` status before declaring success.
    func start() {
        guard !isRunning else { return }

        guard !serverURI.isEmpty else {
            status = "Error: server URI required"
            return
        }

        // Hand the user's whitelist entries to Rust before the tunnel starts.
        _ = phantomMacosSetProxyDomains(proxyDomainsText)

        let modeString: String
        switch proxyMode {
        case .global: modeString = "proxy"
        case .smart:  modeString = "smart"
        case .direct: modeString = "direct"
        }

        status = "Starting..."
        logs = []
        logCursor = 0
        startLogPolling()

        let uri = serverURI
        Task { [weak self] in
            let rc = phantomMacosStartWithURI(uri, modeString)
            await MainActor.run {
                guard let self else { return }
                if rc != 0 {
                    let err = phantomMacosGetLastError() ?? "unknown error (rc=\(rc))"
                    self.status = "Error: \(err)"
                    self.isRunning = false
                    self.stopLogPolling()
                    return
                }
                // Rust accepted the request; start polling the real state.
                self.startStatusPolling()
            }
        }
    }

    func stop() {
        guard isRunning || status.hasPrefix("Starting") || status.hasPrefix("Connecting") else { return }
        stopStatusPolling()
        // Restore system proxy before stopping tunnel.
        systemProxy?.disable()
        systemProxy = nil
        let _ = phantomMacosStop()
        isRunning = false
        status = "Stopped"
        stopLogPolling()
    }

    // MARK: - Status polling

    private func startStatusPolling() {
        stopStatusPolling()
        statusTimer = Timer.scheduledTimer(withTimeInterval: 0.2, repeats: true) { [weak self] _ in
            // Timer fires on the main run loop; the closure itself is @Sendable.
            MainActor.assumeIsolated {
                self?.pollStatusOnce()
            }
        }
    }

    private func stopStatusPolling() {
        statusTimer?.invalidate()
        statusTimer = nil
    }

    private func pollStatusOnce() {
        let rustStatus = phantomMacosGetStatus()
        switch rustStatus {
        case .starting:
            if !status.hasPrefix("Connecting") {
                status = "Connecting..."
            }

        case .running:
            if !isRunning {
                isRunning = true
                // Enable system SOCKS5 proxy on the same port Rust is listening on.
                let port = Int(phantomMacosSocks5Port())
                var configured = false
                if var proxy = SystemProxy(host: "127.0.0.1", port: port) {
                    configured = proxy.enable()
                    systemProxy = configured ? proxy : nil
                }
                if configured {
                    status = "Connected"
                } else {
                    // SOCKS5 is up and the tunnel works; only the system-wide
                    // proxy switch failed (the network service could not be
                    // changed by networksetup).
                    status = "Connected — SOCKS5 127.0.0.1:\(port) (系统代理未设置)"
                    logs.append(
                        "[WARN] System proxy was not set on the active network service. "
                            + "SOCKS5 itself works on 127.0.0.1:\(port); set the system proxy "
                            + "manually with: bash scripts/mac-sysproxy.sh on"
                    )
                }
            }

        case .error:
            let err = phantomMacosGetLastError() ?? "Tunnel failed"
            isRunning = false
            status = "Error: \(err)"
            systemProxy?.disable()
            systemProxy = nil
            stopStatusPolling()
            // Keep log polling alive briefly so the error log is visible.
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { [weak self] in
                self?.stopLogPolling()
            }

        case .idle:
            isRunning = false
            status = "Idle"
            stopStatusPolling()
        }
    }

    // MARK: - Log polling

    private func startLogPolling() {
        stopLogPolling()
        logTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                self?.pollLogsOnce()
            }
        }
    }

    private func stopLogPolling() {
        logTimer?.invalidate()
        logTimer = nil
    }

    private func pollLogsOnce() {
        let result = phantomMacosGetLogs(sinceCursor: logCursor)
        logCursor = result.cursor
        if !result.lines.isEmpty {
            DispatchQueue.main.async { [weak self] in
                guard let self else { return }
                self.logs.append(contentsOf: result.lines)
                // Keep at most 200 lines to avoid unbounded growth.
                if self.logs.count > 200 {
                    self.logs.removeFirst(self.logs.count - 200)
                }
            }
        }
    }
}
