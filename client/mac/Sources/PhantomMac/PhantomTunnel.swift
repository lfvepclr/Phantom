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
class PhantomTunnel: ObservableObject {
    @Published var isRunning = false
    @Published var status = "Idle"
    @Published var proxyMode: ProxyMode = .smart
    /// Server URI (phantom://<base64_key>@host:port?cipher=auto#name)
    @Published var serverURI: String = ""
    /// Connection log lines from Rust.
    @Published var logs: [String] = []

    private var systemProxy: SystemProxy?
    private var logTimer: Timer?
    private var statusTimer: Timer?
    private var logCursor: UInt64 = 0

    /// Start the tunnel.  This does not immediately set `isRunning = true`;
    /// we wait until Rust reports `running` status before declaring success.
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
        logs = []
        logCursor = 0
        startLogPolling()

        Task { [weak self] in
            let rc = phantomMacosStartWithURI(serverURI, modeString)
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
            self?.pollStatusOnce()
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
                status = "Connected"
                // Enable system SOCKS5 proxy on the same port Rust is listening on.
                let port = Int(phantomMacosSocks5Port())
                var sp = SystemProxy(host: "127.0.0.1", port: port)
                sp?.enable()
                systemProxy = sp
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
            self?.pollLogsOnce()
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
