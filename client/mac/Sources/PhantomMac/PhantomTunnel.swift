import Foundation
import Combine
import PhantomMacKit

enum ProxyMode: String, CaseIterable, Sendable {
    case global = "Global"
    case smart = "Auto"
    case direct = "Direct"

    /// Label shown in the segmented control (matches the HarmonyOS wording).
    var label: String {
        switch self {
        case .global: return "全局"
        case .smart: return "智能"
        case .direct: return "直连"
        }
    }

    /// Value the Rust side understands.
    var wireValue: String {
        switch self {
        case .global: return "proxy"
        case .smart: return "smart"
        case .direct: return "direct"
        }
    }
}

/// Manages the Phantom tunnel lifecycle on macOS.
///
/// The tunnel itself (SOCKS5 ingress, packet processing, encryption) runs
/// entirely inside the Rust cdylib; this class is the control plane and the
/// single source of truth every window reads from.
@MainActor
final class PhantomTunnel: ObservableObject {
    /// One instance for the whole app: the menu bar, the main window, the log
    /// window and the termination hook must never disagree about the state.
    static let shared = PhantomTunnel()

    // MARK: - Published state

    @Published private(set) var isRunning = false
    @Published private(set) var status = "Idle"

    /// Proxy mode, persisted so a restart keeps the operator's choice.
    @Published var proxyMode: ProxyMode = .smart {
        didSet { UserDefaults.standard.set(proxyMode.rawValue, forKey: Self.modeKey) }
    }

    /// Server URI (`phantom://…`). Persisted: the URI is long, and re-pasting it
    /// on every launch is the difference between a usable app and an annoying one.
    @Published var serverURI: String = "" {
        didSet { UserDefaults.standard.set(serverURI, forKey: Self.uriKey) }
    }

    /// Raw log lines from Rust (bounded — see `logBufferLimit`).
    @Published private(set) var logs: [String] = []
    @Published var logFilter: LogFilter = .tunnel {
        didSet { UserDefaults.standard.set(logFilter.rawValue, forKey: Self.logFilterKey) }
    }
    @Published private(set) var logPaused = false

    /// Live counters, refreshed once a second while connected.
    @Published private(set) var rates = TrafficRates()

    /// Extra proxy-whitelist domains maintained in the editor (built-in
    /// censored-domain list is always active on top of these).
    @Published private(set) var whitelistEntries: [String] = []

    /// Remembered connections, most recent first.
    @Published private(set) var history: [ServerHistoryEntry] = []

    @Published private(set) var startedAt: Date?
    @Published private(set) var latencyText = ""
    @Published private(set) var speedText = ""
    @Published private(set) var latencyBusy = false
    @Published private(set) var speedBusy = false

    // MARK: - Derived state

    var link: ServerLink { parseServerURI(serverURI) }

    var state: PhantomState {
        if isRunning { return .running }
        if status.hasPrefix("Error") || status.hasPrefix("Start failed") {
            return .error(status)
        }
        if status == "Starting..." || status == "Connecting..." { return .connecting }
        return .idle
    }

    /// Lines the panes render (filtered, deduplicated, capped).
    var visibleLogLines: [LogLine] {
        renderLogLines(logs, filter: logFilter, limit: Self.logViewLimit)
    }

    var uptimeSeconds: Int {
        guard let startedAt else { return 0 }
        return max(0, Int(Date().timeIntervalSince(startedAt)))
    }

    var socksPort: UInt16 { phantomMacosSocks5Port() }

    // MARK: - Storage

    private static let uriKey = "phantom.serverURI"
    private static let modeKey = "phantom.proxyMode"
    private static let domainsKey = "phantom.proxyDomains"
    private static let historyKey = "phantom.history"
    private static let logFilterKey = "phantom.logFilter"

    /// Raw lines kept in memory (the disk log keeps far more).
    private static let logBufferLimit = 1000
    /// Lines rendered in a pane.
    private static let logViewLimit = 200

    private var systemProxy: SystemProxy?
    private var statusTimer: Timer?
    private var statsTimer: Timer?
    private var logTimer: Timer?
    private var logCursor: UInt64 = 0
    private var previousSnapshot = TrafficSnapshot()
    private var lastSampleAt = Date()

    private init() {
        let defaults = UserDefaults.standard
        if let savedURI = defaults.string(forKey: Self.uriKey), !savedURI.isEmpty {
            serverURI = savedURI
        }
        if let raw = defaults.string(forKey: Self.modeKey),
           let savedMode = ProxyMode(rawValue: raw) {
            proxyMode = savedMode
        }
        if let raw = defaults.string(forKey: Self.logFilterKey),
           let savedFilter = LogFilter(rawValue: raw) {
            logFilter = savedFilter
        }
        if let savedDomains = defaults.string(forKey: Self.domainsKey) {
            whitelistEntries = ProxyWhitelist.normalizeList(savedDomains).accepted
        }
        if let savedHistory = defaults.string(forKey: Self.historyKey) {
            history = parseHistory(savedHistory)
        }
    }

    // MARK: - Lifecycle

    func toggle() {
        isRunning ? stop() : start()
    }

    /// Start the tunnel. `isRunning` only flips once Rust reports `running`.
    func start() {
        guard !isRunning else { return }
        guard !serverURI.isEmpty else {
            status = "Error: server URI required"
            return
        }

        // Hand the operator's whitelist entries to Rust before the tunnel starts.
        _ = phantomMacosSetProxyDomains(ProxyWhitelist.serialize(whitelistEntries))

        status = "Starting..."
        logs = []
        logCursor = 0
        previousSnapshot = TrafficSnapshot()
        lastSampleAt = Date()
        rates = TrafficRates()
        latencyText = ""
        speedText = ""
        startLogPolling()

        let uri = serverURI
        let mode = proxyMode.wireValue
        let usedAt = Date()
        history = upsertHistory(history, uri: uri, usedAt: usedAt)
        persistHistory()

        Task { [weak self] in
            let rc = phantomMacosStartWithURI(uri, mode)
            await MainActor.run {
                guard let self else { return }
                if rc != 0 {
                    let err = phantomMacosGetLastError() ?? "unknown error (rc=\(rc))"
                    self.status = "Error: \(err)"
                    self.isRunning = false
                    self.stopLogPolling()
                    return
                }
                self.startStatusPolling()
            }
        }
    }

    func stop() {
        guard isRunning || status.hasPrefix("Starting") || status.hasPrefix("Connecting") else {
            return
        }
        stopStatusPolling()
        // Restore the system proxy before tearing the listener down.
        systemProxy?.disable()
        systemProxy = nil
        _ = phantomMacosStop()
        isRunning = false
        status = "Stopped"
        startedAt = nil
        rates = TrafficRates()
        stopLogPolling()
    }

    /// Called from the termination hook: never leave the machine pointing at a
    /// SOCKS5 listener that is about to disappear.
    func shutdown() {
        stop()
        persistHistory()
    }

    // MARK: - Status + telemetry polling

    private func startStatusPolling() {
        stopStatusPolling()
        statusTimer = Timer.scheduledTimer(withTimeInterval: 0.2, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.pollStatusOnce() }
        }
        statsTimer = Timer.scheduledTimer(withTimeInterval: 1.0, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.pollStatsOnce() }
        }
        pollStatsOnce()
    }

    private func stopStatusPolling() {
        statusTimer?.invalidate()
        statusTimer = nil
        statsTimer?.invalidate()
        statsTimer = nil
    }

    private func pollStatusOnce() {
        switch phantomMacosGetStatus() {
        case .starting:
            if !status.hasPrefix("Connecting") { status = "Connecting..." }

        case .running:
            if !isRunning {
                isRunning = true
                startedAt = Date()
                markCurrentVerified()
                let port = Int(phantomMacosSocks5Port())
                var configured = false
                if var proxy = SystemProxy(host: "127.0.0.1", port: port) {
                    configured = proxy.enable()
                    systemProxy = configured ? proxy : nil
                }
                if configured {
                    status = "Connected"
                } else {
                    status = "Connected — SOCKS5 127.0.0.1:\(port)（系统代理未设置）"
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
            startedAt = nil
            systemProxy?.disable()
            systemProxy = nil
            stopStatusPolling()
            // Keep log polling alive briefly so the error log stays visible.
            DispatchQueue.main.asyncAfter(deadline: .now() + 1.0) { [weak self] in
                self?.stopLogPolling()
            }

        case .idle:
            isRunning = false
            status = "Idle"
            startedAt = nil
            stopStatusPolling()
        }
    }

    private func pollStatsOnce() {
        guard isRunning else { return }
        let snapshot = TrafficSnapshot(json: phantomMacosGetStatsJson())
        let now = Date()
        let elapsed = now.timeIntervalSince(lastSampleAt)
        rates = TrafficRates(previous: previousSnapshot, current: snapshot, elapsed: elapsed)
        previousSnapshot = snapshot
        lastSampleAt = now
    }

    // MARK: - Probes

    func measureLatency() {
        guard isRunning, !latencyBusy else { return }
        latencyBusy = true
        latencyText = "测量中…"
        let port = socksPort
        Task { [weak self] in
            let result = await Task.detached {
                TunnelProbe.measureLatency(proxyPort: port)
            }.value
            await MainActor.run {
                guard let self else { return }
                self.latencyBusy = false
                self.latencyText = result.ok
                    ? "\(result.milliseconds) ms（含服务端出口）"
                    : "失败：\(result.error)"
            }
        }
    }

    func measureSpeed() {
        guard isRunning, !speedBusy else { return }
        speedBusy = true
        speedText = "测速中（约 5 秒）…"
        let port = socksPort
        Task { [weak self] in
            let result = await Task.detached {
                TunnelProbe.measureSpeed(proxyPort: port)
            }.value
            await MainActor.run {
                guard let self else { return }
                self.speedBusy = false
                if result.ok {
                    let status = result.status.isEmpty ? "" : "，源站 \(result.status)"
                    self.speedText = "↓ \(formatRate(result.bytesPerSecond))"
                        + "（\(formatBytes(result.bytes)) / "
                        + String(format: "%.1f", result.elapsed) + " 秒"
                        + "，源站 \(TunnelProbe.speedHost)\(status)）"
                } else {
                    self.speedText = "失败：\(result.error)"
                }
            }
        }
    }

    // MARK: - Whitelist editing

    /// Add one entry. Returns an error message when the input is unusable.
    @discardableResult
    func addWhitelistEntry(_ raw: String) -> String? {
        switch ProxyWhitelist.normalize(raw) {
        case .rejected(let reason):
            return reason
        case .accepted(let domain):
            guard !whitelistEntries.contains(domain) else { return nil }
            whitelistEntries.append(domain)
            persistWhitelist()
            return nil
        }
    }

    func removeWhitelistEntry(_ domain: String) {
        whitelistEntries.removeAll { $0 == domain }
        persistWhitelist()
    }

    /// Bulk import; returns a human-readable summary of what was added/skipped.
    func importWhitelist(_ text: String) -> String {
        let parsed = ProxyWhitelist.normalizeList(text)
        let before = whitelistEntries.count
        whitelistEntries = ProxyWhitelist.merge(whitelistEntries, parsed.accepted)
        persistWhitelist()
        let added = whitelistEntries.count - before
        var summary = "已添加 \(added) 条"
        if parsed.rejected.count > 0 {
            summary += "，跳过 \(parsed.rejected.count) 条：" + parsed.rejected.prefix(3).joined(separator: "、")
        }
        return summary
    }

    var whitelistExport: String { ProxyWhitelist.serialize(whitelistEntries) }

    // MARK: - Log helpers

    func clearLogs() {
        logs = []
        logCursor = 0
    }

    func toggleLogPaused() {
        logPaused.toggle()
        if !logPaused { pollLogsOnce() }
    }

    // MARK: - History

    func useHistoryEntry(_ entry: ServerHistoryEntry) {
        // Fill only — connecting stays an explicit user action.
        serverURI = entry.uri
    }

    func forgetHistoryEntry(_ entry: ServerHistoryEntry) {
        history = removeFromHistory(history, uri: entry.uri)
        persistHistory()
    }

    private func markCurrentVerified() {
        guard !serverURI.isEmpty else { return }
        history = upsertHistory(history, uri: serverURI, verifiedAt: Date())
        persistHistory()
    }

    private func persistHistory() {
        UserDefaults.standard.set(serializeHistory(history), forKey: Self.historyKey)
    }

    private func persistWhitelist() {
        UserDefaults.standard.set(ProxyWhitelist.serialize(whitelistEntries), forKey: Self.domainsKey)
    }

    // MARK: - Log polling

    private func startLogPolling() {
        stopLogPolling()
        logTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.pollLogsOnce() }
        }
    }

    private func stopLogPolling() {
        logTimer?.invalidate()
        logTimer = nil
    }

    private func pollLogsOnce() {
        guard !logPaused else { return }
        let result = phantomMacosGetLogs(sinceCursor: logCursor)
        logCursor = result.cursor
        guard !result.lines.isEmpty else { return }
        logs.append(contentsOf: result.lines)
        if logs.count > Self.logBufferLimit {
            logs.removeFirst(logs.count - Self.logBufferLimit)
        }
    }
}
