import Foundation

/// Manages macOS system-wide SOCKS5 proxy settings via networksetup.
///
/// On start: saves current proxy state, then sets SOCKS5 to Phantom's local proxy.
/// On stop: restores the previously saved state.
struct SystemProxy {
    private var savedState: ProxyState?
    private let service: String
    private let proxyHost: String
    private let proxyPort: Int

    struct ProxyState {
        var enabled: Bool
        var host: String
        var port: String
    }

    init?(host: String, port: Int) {
        self.proxyHost = host
        self.proxyPort = port
        guard let service = SystemProxy.detectActiveNetworkService() else {
            return nil
        }
        self.service = service
    }

    /// Enable Phantom SOCKS5 proxy on the active network service.
    mutating func enable() {
        savedState = readCurrentSOCKSState()
        _ = runNetworkSetup(args: ["-setsocksfirewallproxy", service, proxyHost, String(proxyPort)])
        _ = runNetworkSetup(args: ["-setsocksfirewallproxystate", service, "on"])
    }

    /// Restore previously saved proxy state.
    mutating func disable() {
        guard let state = savedState else {
            _ = runNetworkSetup(args: ["-setsocksfirewallproxystate", service, "off"])
            return
        }
        if state.enabled {
            _ = runNetworkSetup(args: ["-setsocksfirewallproxy", service, state.host, state.port])
            _ = runNetworkSetup(args: ["-setsocksfirewallproxystate", service, "on"])
        } else {
            _ = runNetworkSetup(args: ["-setsocksfirewallproxystate", service, "off"])
        }
        savedState = nil
    }

    /// Detect the primary active network service (e.g. "Wi-Fi").
    private static func detectActiveNetworkService() -> String? {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/sbin/networksetup")
        task.arguments = ["-listnetworkserviceorder"]
        let pipe = Pipe()
        task.standardOutput = pipe
        do {
            try task.run()
            task.waitUntilExit()
        } catch {
            return nil
        }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        guard let output = String(data: data, encoding: .utf8) else { return nil }

        // Look for the service marked with an asterisk (active).
        for line in output.split(separator: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("(1)") {
                // Example: "(1) Wi-Fi"
                let parts = trimmed.split(separator: ")", maxSplits: 1, omittingEmptySubsequences: true)
                if parts.count == 2 {
                    return parts[1].trimmingCharacters(in: .whitespaces)
                }
            }
        }
        // Fallback: return "Wi-Fi" if nothing found.
        return "Wi-Fi"
    }

    private func readCurrentSOCKSState() -> ProxyState {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/sbin/networksetup")
        task.arguments = ["-getsocksfirewallproxy", service]
        let pipe = Pipe()
        task.standardOutput = pipe
        do {
            try task.run()
            task.waitUntilExit()
        } catch {
            return ProxyState(enabled: false, host: "", port: "")
        }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        guard let output = String(data: data, encoding: .utf8) else {
            return ProxyState(enabled: false, host: "", port: "")
        }

        var enabled = false
        var host = ""
        var port = ""
        for line in output.split(separator: "\n") {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.hasPrefix("Enabled: ") {
                enabled = trimmed.dropFirst("Enabled: ".count).trimmingCharacters(in: .whitespaces) == "Yes"
            } else if trimmed.hasPrefix("Server: ") {
                host = String(trimmed.dropFirst("Server: ".count).trimmingCharacters(in: .whitespaces))
            } else if trimmed.hasPrefix("Port: ") {
                port = String(trimmed.dropFirst("Port: ".count).trimmingCharacters(in: .whitespaces))
            }
        }
        return ProxyState(enabled: enabled, host: host, port: port)
    }

    private func runNetworkSetup(args: [String]) -> Int32 {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/sbin/networksetup")
        task.arguments = args
        do {
            try task.run()
            task.waitUntilExit()
        } catch {
            return -1
        }
        return task.terminationStatus
    }
}
