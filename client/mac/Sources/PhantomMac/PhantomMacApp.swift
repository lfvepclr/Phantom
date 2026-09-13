import SwiftUI
import AppKit
import PhantomMacKit

// MARK: - Crash logging
//
// Write fatal signals / uncaught exceptions to ~/PhantomMac-crash.log so launch
// failures are visible even though this app has no Dock icon. Global state is
// required because C function-pointer callbacks cannot capture context.

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

// MARK: - Window plumbing
//
// There is exactly one window now: the menu-bar popover. The logs used to live
// in a second window, which needed a registry of concrete `NSWindow`s just to
// raise it (`openWindow(id:)` is a no-op for an already-open window) and could
// still end up stuck behind another app. Folding the log into the popover
// removed both the window and the problem.

/// Owns the process-level lifecycle.
///
/// The important part is `applicationShouldTerminate`: quitting while connected
/// used to leave the system SOCKS proxy pointing at a listener that no longer
/// exists, which looked like "the Mac lost its network" until the operator
/// reset the proxy by hand.
@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        setupCrashLogger()
        // There is no main window to open any more — the UI lives in the menu
        // bar popover, which cannot be opened programmatically. A fresh install
        // would otherwise look like nothing happened at all, so say once what
        // the icon is and where to click.
        guard PhantomTunnel.shared.serverURI.isEmpty,
              !UserDefaults.standard.bool(forKey: Self.onboardedKey) else { return }
        Task { @MainActor in
            // Let the menu bar item appear first: an alert that beats its own
            // icon to the screen is confusing.
            try? await Task.sleep(for: .milliseconds(800))
            let alert = NSAlert()
            alert.messageText = "Phantom 已在菜单栏运行"
            alert.informativeText = "点击菜单栏的幽灵图标即可打开面板、粘贴连接串并启动隧道。"
            alert.addButton(withTitle: "知道了")
            alert.showsSuppressionButton = true
            alert.suppressionButton?.title = "不再提示"
            alert.runModal()
            if alert.suppressionButton?.state == .on {
                UserDefaults.standard.set(true, forKey: Self.onboardedKey)
            }
        }
    }

    /// Set once the user has seen (and dismissed) the first-run explanation.
    private static let onboardedKey = "phantom.onboarded"

    func applicationShouldTerminate(_ sender: NSApplication) -> NSApplication.TerminateReply {
        PhantomTunnel.shared.shutdown()
        return .terminateNow
    }
}

// MARK: - App entry

@main
struct PhantomMacApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) private var delegate
    @StateObject private var tunnel: PhantomTunnel

    init() {
        _tunnel = StateObject(wrappedValue: PhantomTunnel.shared)
    }

    var body: some Scene {
        // The dashboard *is* the menu bar popover: one click to see the state,
        // one click to start or stop. Everything that is not a daily control
        // is a page pushed inside the popover, and the log keeps whatever height
        // is left.
        MenuBarExtra {
            DashboardPopover(tunnel: tunnel)
        } label: {
            MenuBarLabel(tunnel: tunnel)
        }
        .menuBarExtraStyle(.window)
        .commands {
            CommandGroup(replacing: .appTermination) {
                Button("退出 Phantom") { NSApp.terminate(nil) }
                    .keyboardShortcut("q", modifiers: .command)
            }
        }
    }
}

// MARK: - Menu bar label

/// Menu bar item: a **template** ghost whose silhouette encodes the state.
///
/// Colour cannot be used here — macOS renders menu bar extras as templates, so
/// a coloured shape comes out as a flat blob. Shape differences survive.
struct MenuBarLabel: View {
    @ObservedObject var tunnel: PhantomTunnel

    var body: some View {
        // A fresh install has no connection and no window to explain itself, so
        // the icon carries a small dot until something is configured.
        Image(nsImage: MenuBarGlyph.image(for: tunnel.state, needsAttention: !tunnel.link.valid))
            .help(
                tunnel.link.valid
                    ? "Phantom — \(tunnel.state.title)"
                    : "Phantom — 点击配置连接串"
            )
    }
}
