import AppKit

/// Normalised tunnel state, shared by the menu bar, the window header and the
/// quick menu so all three can never disagree about what "connected" means.
public enum PhantomState: Equatable, Sendable {
    case idle
    case connecting
    case running
    case error(String)

    public var title: String {
        switch self {
        case .idle: return "未连接"
        case .connecting: return "连接中…"
        case .running: return "已连接"
        case .error: return "错误"
        }
    }

    /// Fallback glyph when the drawn image cannot be produced.
    public var symbolName: String {
        switch self {
        case .idle: return "moon.zzz"
        case .connecting: return "arrow.triangle.2.circlepath"
        case .running: return "shield.fill"
        case .error: return "exclamationmark.triangle.fill"
        }
    }

    public var accent: NSColor {
        switch self {
        case .idle: return .secondaryLabelColor
        case .connecting: return .systemYellow
        case .running: return .systemGreen
        case .error: return .systemRed
        }
    }

    public var isRunning: Bool {
        if case .running = self { return true }
        return false
    }

    public var isError: Bool {
        if case .error = self { return true }
        return false
    }
}
