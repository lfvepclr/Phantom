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

enum WindowID {
    static let main = "main"
    static let logs = "logs"
}

/// Bridges "open a window" out of the SwiftUI environment so the termination /
/// launch hooks (which are not views) can trigger it too.
@MainActor
final class WindowBridge {
    static let shared = WindowBridge()
    var openMain: (() -> Void)?
    var openLogs: (() -> Void)?

    func showMain() {
        openMain?()
    }

    func showLogs() {
        openLogs?()
    }
}

/// Owns the process-level lifecycle.
///
/// The important part is `applicationShouldTerminate`: quitting while connected
/// used to leave the system SOCKS proxy pointing at a listener that no longer
/// exists, which looked like "the Mac lost its network" until the operator
/// reset the proxy by hand.
final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        setupCrashLogger()
        // First run: with no connection configured there is nothing to see in
        // the menu bar, so open the window that explains what to do.
        if PhantomTunnel.shared.serverURI.isEmpty {
            DispatchQueue.main.asyncAfter(deadline: .now() + 0.4) {
                WindowBridge.shared.showMain()
            }
        }
    }

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
        // The menu bar owns status + quick switches only; the real UI lives in
        // resizable windows, where the log pane finally has room to breathe.
        MenuBarExtra {
            QuickMenu(tunnel: tunnel)
        } label: {
            MenuBarLabel(tunnel: tunnel)
        }
        .menuBarExtraStyle(.menu)

        Window("Phantom", id: WindowID.main) {
            MainWindowView(tunnel: tunnel)
        }
        .defaultSize(width: 460, height: 780)
        .windowResizability(.contentMinSize)
        .commands {
            CommandGroup(replacing: .appTermination) {
                Button("退出 Phantom") { NSApp.terminate(nil) }
                    .keyboardShortcut("q", modifiers: .command)
            }
        }

        Window("Phantom 日志", id: WindowID.logs) {
            LogWindowView(tunnel: tunnel)
        }
        .defaultSize(width: 760, height: 460)
        .windowResizability(.contentMinSize)
    }
}

// MARK: - Menu bar label

/// Menu bar item: a **template** ghost whose silhouette encodes the state.
///
/// Colour cannot be used here — macOS renders menu bar extras as templates, so
/// a coloured shape comes out as a flat blob. Shape differences survive.
struct MenuBarLabel: View {
    @ObservedObject var tunnel: PhantomTunnel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Image(nsImage: MenuBarGlyph.image(for: tunnel.state))
            .help("Phantom — \(tunnel.state.title)")
            .onAppear {
                WindowBridge.shared.openMain = {
                    openWindow(id: WindowID.main)
                    NSApp.activate()
                }
                WindowBridge.shared.openLogs = {
                    openWindow(id: WindowID.logs)
                    NSApp.activate()
                }
            }
    }
}

// MARK: - Quick menu

/// What a click on the menu bar icon offers: state, the one switch people use
/// most, and the windows.
struct QuickMenu: View {
    @ObservedObject var tunnel: PhantomTunnel
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Text("Phantom — \(tunnel.state.title)")
        Text(tunnel.link.valid ? linkAddress(tunnel.link) : "未配置连接")
        if tunnel.isRunning {
            Text("↓ \(formatRate(tunnel.rates.downPerSecond))   ↑ \(formatRate(tunnel.rates.upPerSecond))")
        }
        Divider()

        Button(tunnel.isRunning ? "断开连接" : "启动连接") {
            tunnel.toggle()
        }
        .disabled(!tunnel.isRunning && tunnel.serverURI.isEmpty)

        Divider()

        Button("打开主界面") {
            openWindow(id: WindowID.main)
            NSApp.activate()
        }
        Button("日志窗口") {
            openWindow(id: WindowID.logs)
            NSApp.activate()
        }

        Divider()

        Button("退出 Phantom") {
            NSApp.terminate(nil)
        }
        .keyboardShortcut("q", modifiers: .command)
    }
}
