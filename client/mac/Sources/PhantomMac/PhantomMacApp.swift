import SwiftUI
import AppKit

// MARK: - Crash logging
// Write fatal signals / uncaught exceptions to ~/PhantomMac-crash.log
// so launch failures are visible even when the app has no window.
// Uses global variables because C function-pointer closures cannot capture context.

private let kCrashLogPath = NSHomeDirectory() + "/PhantomMac-crash.log"

private func writeCrashLog(_ message: String) {
    try? message.write(toFile: kCrashLogPath, atomically: true, encoding: .utf8)
    fputs(message, stderr)
}

private func uncaughtExceptionHandler(_ exception: NSException) {
    let msg = "[\(Date())] Uncaught exception: \(exception.name.rawValue)\n"
        + "  reason: \(exception.reason ?? "nil")\n"
        + "  call stack:\n\(exception.callStackSymbols.joined(separator: "\n"))\n"
    writeCrashLog(msg)
}

private func signalHandler(_ code: Int32) {
    let name: String
    switch code {
    case SIGABRT: name = "SIGABRT"
    case SIGSEGV: name = "SIGSEGV"
    case SIGBUS:  name = "SIGBUS"
    case SIGILL:  name = "SIGILL"
    case SIGFPE:  name = "SIGFPE"
    default:      name = "SIG\(code)"
    }
    writeCrashLog("[\(Date())] Fatal signal: \(name) (\(code))\n")
    signal(code, SIG_DFL)
    raise(code)
}

private func setupCrashLogger() {
    NSSetUncaughtExceptionHandler(uncaughtExceptionHandler)
    for sig in [SIGABRT, SIGSEGV, SIGBUS, SIGILL, SIGFPE] {
        signal(sig, signalHandler)
    }
}

/// Ensure the SPM resource bundle is accessible at the .app root.
/// SPM's `resource_bundle_accessor` only searches two locations:
///   1. `Bundle.main.bundleURL` root directory (e.g. Phantom.app/PhantomMac_PhantomMac.bundle)
///   2. The build-time hardcoded path
/// But macOS .app bundles place resources in `Contents/Resources/`,
/// so we create a symlink at runtime (same pattern as Hora).
/// This MUST be called before any `Bundle.module` access.
private func ensureResourceBundleAccessible() {
    let bundleName = "PhantomMac_PhantomMac.bundle"
    let appBundleURL = Bundle.main.bundleURL
    let expectedPath = appBundleURL.appendingPathComponent(bundleName)

    if FileManager.default.fileExists(atPath: expectedPath.path) {
        return  // Symlink already exists
    }

    let resourceDir = appBundleURL.appendingPathComponent("Contents/Resources")
    let resourceBundlePath = resourceDir.appendingPathComponent(bundleName)

    if FileManager.default.fileExists(atPath: resourceBundlePath.path) {
        do {
            try FileManager.default.createSymbolicLink(
                atPath: expectedPath.path,
                withDestinationPath: "Contents/Resources/" + bundleName
            )
            fputs("[Phantom] Created symlink for resource bundle: \(expectedPath.path) -> Contents/Resources/\(bundleName)\n", stderr)
        } catch {
            fputs("[Phantom] Failed to create symlink for resource bundle: \(error)\n", stderr)
            // Fallback: try copying
            do {
                try FileManager.default.copyItem(atPath: resourceBundlePath.path, toPath: expectedPath.path)
                fputs("[Phantom] Copied resource bundle to expected location\n", stderr)
            } catch {
                fputs("[Phantom] Failed to copy resource bundle: \(error)\n", stderr)
            }
        }
    } else {
        fputs("[Phantom] WARNING: Resource bundle \(bundleName) not found in Contents/Resources/\n", stderr)
    }
}

// MARK: - App entry

@main
struct PhantomMacApp: App {
    @StateObject private var tunnel = PhantomTunnel()

    init() {
        // MUST be called before any Bundle.module access (including loadMenuBarIcon)
        ensureResourceBundleAccessible()
        setupCrashLogger()
    }

    var body: some Scene {
        // .window style keeps the popover open during interaction (unlike .menu).
        MenuBarExtra {
            ContentView(tunnel: tunnel)
        } label: {
            MenuBarLabel(state: PhantomState(tunnel: tunnel))
        }
        .menuBarExtraStyle(.window)
    }
}

// MARK: - Menu bar label

/// Composable label that reflects tunnel state via SF Symbols and a status dot.
/// Drawn as a layered Image so the popover's background isn't affected.
private struct MenuBarLabel: View {
    let state: PhantomState

    var body: some View {
        // Vector ghost, tinted by state. Drawn rather than loaded from a PNG:
        // the previous asset was a full-colour illustration, and forcing it to
        // `isTemplate` flattened the dark plate into the "black square" the
        // menu bar used to show.
        ZStack(alignment: .topTrailing) {
            switch state {
            case .running:
                GhostGlyph()
                    .fill(state.indicatorColor, style: FillStyle(eoFill: true))
            case .connecting:
                GhostGlyph()
                    .stroke(state.indicatorColor, lineWidth: 1.3)
            case .error:
                GhostGlyph()
                    .stroke(state.indicatorColor, lineWidth: 1.3)
                Image(systemName: "exclamationmark.circle.fill")
                    .font(.system(size: 7, weight: .bold))
                    .foregroundStyle(state.indicatorColor)
                    .offset(x: 3, y: -3)
            case .idle:
                GhostGlyph()
                    .stroke(state.indicatorColor, lineWidth: 1.2)
            }
        }
        .frame(width: 16, height: 16)
        .help("Phantom — \(state.title)")
    }
}

/// Minimal ghost silhouette with cut-out eyes.
///
/// One `Path` holding the body plus the two eye circles, filled with the
/// even-odd rule so the eyes come out as holes; stroking the same path gives
/// the "off" outline look.
struct GhostGlyph: Shape {
    func path(in rect: CGRect) -> Path {
        var path = Path()
        let w = rect.width
        let h = rect.height
        let r = w * 0.5
        let bodyBottom = h * 0.82

        // Head: semicircle (left-mid → top → right-mid).
        path.move(to: CGPoint(x: rect.minX, y: rect.minY + r))
        path.addArc(
            center: CGPoint(x: rect.minX + r, y: rect.minY + r),
            radius: r,
            startAngle: .degrees(180),
            endAngle: .degrees(0),
            clockwise: false
        )
        // Right flank down to the hem.
        path.addLine(to: CGPoint(x: rect.maxX, y: rect.minY + bodyBottom))
        // Three hem scallops, right → left.
        let scallop = w / 3
        var x = rect.maxX
        var up = true
        for _ in 0..<3 {
            let next = max(rect.minX, x - scallop)
            path.addQuadCurve(
                to: CGPoint(x: next, y: rect.minY + bodyBottom),
                control: CGPoint(
                    x: (x + next) / 2,
                    y: rect.minY + bodyBottom + (up ? -h * 0.14 : h * 0.10)
                )
            )
            x = next
            up.toggle()
        }
        path.addLine(to: CGPoint(x: rect.minX, y: rect.minY + r))
        path.closeSubpath()

        // Eyes (holes under even-odd fill).
        let eyeR = w * 0.09
        let eyeY = rect.minY + r * 0.95
        for dx in [rect.minX + w * 0.34, rect.minX + w * 0.66] {
            path.addEllipse(
                in: CGRect(
                    x: dx - eyeR,
                    y: eyeY - eyeR,
                    width: eyeR * 2,
                    height: eyeR * 2
                )
            )
        }
        return path
    }
}

// MARK: - PhantomState

/// Normalized state for the tunnel — used by both UI and menu bar.
enum PhantomState: Equatable {
    case idle
    case connecting
    case running
    case error(String)

    @MainActor
    init(tunnel: PhantomTunnel) {
        if tunnel.isRunning {
            self = .running
        } else if tunnel.status.hasPrefix("Error") || tunnel.status.hasPrefix("Start failed") {
            self = .error(tunnel.status)
        } else if tunnel.status == "Starting..." || tunnel.status == "Connecting..." {
            self = .connecting
        } else {
            self = .idle
        }
    }

    var title: String {
        switch self {
        case .idle:       return "Disconnected"
        case .connecting: return "Connecting…"
        case .running:    return "Connected"
        case .error:      return "Error"
        }
    }

    var iconName: String {
        switch self {
        case .idle:       return "moon.zzz"
        case .connecting: return "arrow.triangle.2.circlepath"
        case .running:    return "shield.fill"
        case .error:      return "exclamationmark.triangle.fill"
        }
    }

    var indicatorColor: Color {
        switch self {
        case .idle:       return .secondary
        case .connecting: return .yellow
        case .running:    return .green
        case .error:      return .red
        }
    }

    /// Whether the colored status dot is visible in the menu bar.
    var indicatorOpacity: Double {
        switch self {
        case .idle:       return 0.0   // hide dot when idle to keep menu bar clean
        case .connecting: return 1.0
        case .running:    return 1.0
        case .error:      return 1.0
        }
    }

    var isError: Bool {
        if case .error = self { return true }
        return false
    }
}

// MARK: - Popover content

struct ContentView: View {
    @ObservedObject var tunnel: PhantomTunnel

    private var state: PhantomState { PhantomState(tunnel: tunnel) }

    var body: some View {
        VStack(spacing: 0) {
            Header(state: state)
                .padding(.horizontal, 16)
                .padding(.top, 14)
                .padding(.bottom, 12)
            Divider()
            InputSection(tunnel: tunnel, state: state)
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
            Divider()
            StatusSection(state: state, statusText: tunnel.status, latency: nil)
                .padding(.horizontal, 16)
                .padding(.vertical, 10)
            if state == .running || state == .connecting {
                Divider()
                LogSection(logs: tunnel.logs)
            }
            Divider()
            Footer()
                .padding(.horizontal, 12)
                .padding(.vertical, 10)
        }
        .frame(width: 380)
    }
}

// MARK: - Header

private struct Header: View {
    let state: PhantomState

    var body: some View {
        HStack(alignment: .center, spacing: 10) {
            // App icon — round, tinted by state
            ZStack {
                Circle()
                    .fill(
                        LinearGradient(
                            colors: [
                                Color(red: 0.30, green: 0.18, blue: 0.78),
                                Color(red: 0.10, green: 0.65, blue: 0.95),
                            ],
                            startPoint: .topLeading,
                            endPoint: .bottomTrailing
                        )
                    )
                if let base = PhantomMacApp.loadMenuBarIcon(template: false) {
                    Image(nsImage: base)
                        .resizable()
                        .interpolation(.high)
                        .frame(width: 22, height: 22)
                } else {
                    Image(systemName: "shield.lefthalf.filled")
                        .foregroundStyle(.white)
                        .font(.system(size: 14, weight: .bold))
                }
            }
            .frame(width: 32, height: 32)
            .shadow(color: Color.black.opacity(0.15), radius: 2, y: 1)

            VStack(alignment: .leading, spacing: 1) {
                Text("Phantom")
                    .font(.system(size: 15, weight: .semibold))
                Text(state.title)
                    .font(.system(size: 11))
                    .foregroundStyle(.secondary)
            }

            Spacer(minLength: 0)

            // Status pill
            HStack(spacing: 5) {
                statusGlyph
                    .frame(width: 8, height: 8)
                Text(state.title)
                    .font(.system(size: 11, weight: .medium))
            }
            .foregroundStyle(state.indicatorColor)
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(
                Capsule()
                    .fill(state.indicatorColor.opacity(0.12))
            )
        }
    }

    @ViewBuilder
    private var statusGlyph: some View {
        switch state {
        case .connecting:
            ProgressView()
                .scaleEffect(0.55)
                .frame(width: 8, height: 8)
                .tint(state.indicatorColor)
        default:
            Circle().fill(state.indicatorColor)
        }
    }
}

// MARK: - Input section

private struct InputSection: View {
    @ObservedObject var tunnel: PhantomTunnel
    let state: PhantomState

    var body: some View {
        VStack(alignment: .leading, spacing: 10) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Server")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(.secondary)
                HStack(spacing: 6) {
                    Image(systemName: "link")
                        .foregroundStyle(.tertiary)
                        .font(.system(size: 11))
                    TextField("phantom://key@host:port?cipher=auto#name", text: $tunnel.serverURI)
                        .textFieldStyle(.plain)
                        .font(.system(size: 12, design: .monospaced))
                        .disabled(tunnel.isRunning || state == .connecting)
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 6)
                .background(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .fill(Color(nsColor: .textBackgroundColor))
                )
                .overlay(
                    RoundedRectangle(cornerRadius: 6, style: .continuous)
                        .strokeBorder(Color.secondary.opacity(0.18), lineWidth: 0.5)
                )
            }

            VStack(alignment: .leading, spacing: 6) {
                Text("Mode")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(.secondary)
                Picker("Mode", selection: $tunnel.proxyMode) {
                    ForEach(ProxyMode.allCases, id: \.self) { mode in
                        Label(mode.shortLabel, systemImage: mode.icon)
                            .tag(mode)
                    }
                }
                .pickerStyle(.segmented)
                .labelsHidden()
                .disabled(tunnel.isRunning || state == .connecting)
            }

            // Proxy whitelist (Smart mode): only these destinations are
            // tunnelled; everything else goes direct.
            DisclosureGroup {
                VStack(alignment: .leading, spacing: 4) {
                    TextEditor(text: $tunnel.proxyDomainsText)
                        .font(.system(size: 11, design: .monospaced))
                        .frame(height: 72)
                        .padding(4)
                        .background(
                            RoundedRectangle(cornerRadius: 6, style: .continuous)
                                .fill(Color(nsColor: .textBackgroundColor))
                        )
                        .overlay(
                            RoundedRectangle(cornerRadius: 6, style: .continuous)
                                .strokeBorder(Color.secondary.opacity(0.18), lineWidth: 0.5)
                        )
                        .disabled(tunnel.isRunning || state == .connecting)
                    Text("One domain per line (subdomains included). 内置被墙清单已启用，这里只填额外需要走代理的域名。")
                        .font(.system(size: 10))
                        .foregroundStyle(.secondary)
                }
                .padding(.top, 4)
            } label: {
                Text("分流白名单")
                    .font(.system(size: 11, weight: .medium))
                    .foregroundStyle(.secondary)
            }

            // Primary action button
            Button(action: {
                tunnel.isRunning ? tunnel.stop() : tunnel.start()
            }) {
                HStack(spacing: 6) {
                    Image(systemName: tunnel.isRunning ? "stop.fill" : "play.fill")
                        .font(.system(size: 12, weight: .semibold))
                    Text(tunnel.isRunning ? "Stop" : "Start")
                        .font(.system(size: 13, weight: .semibold))
                }
                .frame(maxWidth: .infinity, minHeight: 32)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(.white)
            .background(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .fill(buttonGradient)
            )
            .overlay(
                RoundedRectangle(cornerRadius: 7, style: .continuous)
                    .strokeBorder(Color.black.opacity(0.1), lineWidth: 0.5)
            )
            .shadow(color: buttonShadowColor.opacity(0.35), radius: 4, y: 1)
            .keyboardShortcut(.return, modifiers: [])
        }
    }

    private var buttonGradient: LinearGradient {
        if tunnel.isRunning {
            return LinearGradient(
                colors: [Color(red: 0.95, green: 0.30, blue: 0.30), Color(red: 0.80, green: 0.20, blue: 0.20)],
                startPoint: .top, endPoint: .bottom)
        } else {
            return LinearGradient(
                colors: [Color(red: 0.20, green: 0.70, blue: 0.45), Color(red: 0.10, green: 0.55, blue: 0.30)],
                startPoint: .top, endPoint: .bottom)
        }
    }

    private var buttonShadowColor: Color {
        tunnel.isRunning ? .red : .green
    }
}

// MARK: - Status section

private struct StatusSection: View {
    let state: PhantomState
    let statusText: String
    let latency: String?

    var body: some View {
        HStack(spacing: 8) {
            Image(systemName: state.iconName)
                .font(.system(size: 11))
                .foregroundStyle(state.indicatorColor)
                .rotationEffect(.degrees(state == .connecting ? 360 : 0))
                .animation(
                    state == .connecting
                        ? .linear(duration: 1.2).repeatForever(autoreverses: false)
                        : .default,
                    value: state == .connecting
                )
            Text(statusText)
                .font(.system(size: 11, design: .monospaced))
                .foregroundStyle(state.isError ? .red : .secondary)
                .lineLimit(1)
                .truncationMode(.middle)
            Spacer(minLength: 0)
            if let latency {
                Text(latency)
                    .font(.system(size: 11, weight: .medium, design: .monospaced))
                    .foregroundStyle(.secondary)
            }
        }
    }
}

// MARK: - Log section

private struct LogSection: View {
    let logs: [String]

    var body: some View {
        ScrollViewReader { proxy in
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 1) {
                    if logs.isEmpty {
                        Text("Waiting for logs…")
                            .font(.system(size: 11, design: .monospaced))
                            .foregroundStyle(.tertiary)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(.vertical, 8)
                    } else {
                        ForEach(Array(logs.enumerated()), id: \.offset) { idx, line in
                            Text(line)
                                .font(.system(size: 10.5, design: .monospaced))
                                .foregroundStyle(color(for: line))
                                .textSelection(.enabled)
                                .frame(maxWidth: .infinity, alignment: .leading)
                                .id(idx)
                        }
                    }
                }
                .padding(.horizontal, 10)
                .padding(.vertical, 8)
            }
            .frame(maxHeight: 160)
            .background(
                RoundedRectangle(cornerRadius: 6, style: .continuous)
                    .fill(Color.black.opacity(0.75))
            )
            .padding(.horizontal, 12)
            .padding(.bottom, 8)
            .onChange(of: logs.count) {
                withAnimation(.easeOut(duration: 0.15)) {
                    proxy.scrollTo(logs.count - 1, anchor: .bottom)
                }
            }
        }
    }

    private func color(for line: String) -> Color {
        if line.contains("ERROR") || line.contains("error") || line.contains("failed") {
            return Color(red: 1.0, green: 0.45, blue: 0.45)
        } else if line.contains("WARN") || line.contains("warn") {
            return Color(red: 1.0, green: 0.78, blue: 0.35)
        } else if line.contains("INFO") || line.contains("info") {
            return Color(red: 0.50, green: 0.85, blue: 1.0)
        } else {
            return Color.white.opacity(0.85)
        }
    }
}

// MARK: - Footer

private struct Footer: View {
    @State private var isHovered = false

    var body: some View {
        HStack {
            Text("Phantom v1.0")
                .font(.system(size: 10))
                .foregroundStyle(.tertiary)
            Spacer()
            Button {
                NSApplication.shared.terminate(nil)
            } label: {
                HStack(spacing: 4) {
                    Image(systemName: "power")
                        .font(.system(size: 10))
                    Text("Quit")
                        .font(.system(size: 11))
                }
                .padding(.horizontal, 8)
                .padding(.vertical, 4)
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .foregroundStyle(isHovered ? .primary : .secondary)
            .background(
                RoundedRectangle(cornerRadius: 5)
                    .fill(isHovered ? Color.secondary.opacity(0.12) : .clear)
            )
            .onHover { isHovered = $0 }
        }
    }
}

// MARK: - ProxyMode helpers

private extension ProxyMode {
    var shortLabel: String {
        switch self {
        case .global: return "Global"
        case .smart:  return "Auto"
        case .direct: return "Direct"
        }
    }

    var icon: String {
        switch self {
        case .global: return "globe"
        case .smart:  return "wand.and.stars"
        case .direct: return "arrow.up.right"
        }
    }
}

// MARK: - Helpers

extension PhantomMacApp {
    /// Load MenuBarIcon.png from the SPM resource bundle via NSImage.
    /// - Parameter template: When true, sets `isTemplate=true` so macOS renders
    ///   the icon as a monochrome template (adapts to light/dark menu bar).
    ///   When false, preserves the original full-color icon.
    static func loadMenuBarIcon(template: Bool = false) -> NSImage? {
        let bundle = Bundle.module
        // Try @2x first (Retina), then base
        let candidates = [
            ("MenuBarIcon@2x", "png"),
            ("MenuBarIcon", "png"),
        ]
        for (name, ext) in candidates {
            if let url = bundle.url(forResource: name, withExtension: ext),
               let image = NSImage(contentsOf: url) {
                image.size = NSSize(width: 18, height: 18)
                image.isTemplate = template
                return image
            }
        }
        return nil
    }
}
