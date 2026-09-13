import Foundation

/// Which log lines the pane shows.
///
/// Smart mode sends most flows direct, so the raw stream is dominated by lines
/// that say "nothing happened here". `tunnel` keeps only the flows that were
/// actually proxied — the ones an operator debugging "is my traffic really
/// going through the server?" cares about.
public enum LogFilter: String, CaseIterable, Sendable {
    case tunnel
    case all

    public var label: String {
        switch self {
        case .tunnel: return "仅隧道"
        case .all: return "全部"
        }
    }
}

/// Reading the routing breadcrumbs the Rust core writes (`INFO route … -> …`).
///
/// The core emits one canonical line shape from every transport — TUN, the
/// SOCKS5 listener and the HTTP proxy — but they were not always spelled the
/// same: the SOCKS5/HTTP paths used `-> DIRECT (` while this filter matched the
/// TUN spelling `-> Direct (`. macOS rides the SOCKS5 path (it points the
/// system proxy at the local listener), so "仅隧道" hid next to nothing. Match
/// case-insensitively, and fall back to the per-flow lines those paths emit.
public enum LogRoute {
    /// Tokens that only ever appear on a line whose flow went straight out.
    static let directTokens = [
        "-> direct (",
        "direct connection established",
        "direct connect failed",
        "direct http resolve failed",
        "direct http connect failed",
        "direct http connection established",
        "flow end (direct)",
    ]

    /// Tokens that mark a proxied verdict, checked first so a line such as
    /// `route 1.2.3.4:443 -> Proxy (direct connect failed: …)` — a flow that
    /// did take the tunnel — is never mistaken for a direct one.
    static let proxyTokens = ["-> proxy ("]

    public static func isDirect(_ message: String) -> Bool {
        let lower = message.lowercased()
        if proxyTokens.contains(where: lower.contains) { return false }
        return directTokens.contains(where: lower.contains)
    }

    /// The target of a `route <target> -> Direct (…)` line, when there is one.
    public static func directTarget(_ message: String) -> String? {
        guard isDirect(message) else { return nil }
        return target(ofRouteLine: splitLevel(message).text)
    }

    /// The target of a per-request banner line, which is logged *before* the
    /// routing verdict and therefore cannot say on its own which way it went.
    public static func bannerTarget(_ message: String) -> String? {
        // Callers hand us the raw ring-buffer line, severity token included.
        let body = splitLevel(message).text
        for prefix in ["SOCKS5 target: ", "HTTP CONNECT → ", "HTTP proxy → "] {
            guard let rest = body.dropPrefix(prefix), let end = rest.range(of: " (") else {
                continue
            }
            let target = rest[rest.startIndex..<end.lowerBound]
            return target.isEmpty ? nil : String(target)
        }
        return nil
    }

    /// `<target>` out of `route <target> -> …`.
    static func target(ofRouteLine message: String) -> String? {
        guard let arrow = message.range(of: " -> ") else { return nil }
        let head = message[message.startIndex..<arrow.lowerBound]
        guard let keyword = head.range(of: "route ", options: .backwards) else { return nil }
        let target = head[keyword.upperBound...]
        return target.isEmpty ? nil : String(target)
    }
}

/// One rendered row: the text plus a stable id for `ForEach`.
public struct LogLine: Identifiable, Equatable, Sendable {
    public let id: Int
    public let text: String
    /// How many consecutive identical messages this row stands for (1 = once).
    public let repeats: Int
    /// Severity, carried separately because the token itself is stripped from
    /// `text` — the pane is ~50 monospace columns wide and `INFO ` costs five
    /// of them on every route line.
    public let level: LogLevel
}

/// Severity of a rendered row, used for colouring.
public enum LogLevel: String, Sendable {
    case info, warn, error, other
}

/// `17:26:44` prefix written by the bridge, or `""` when the line has none.
public func logStamp(of line: String) -> String {
    let chars = Array(line)
    guard chars.count > 9,
          chars[2] == ":", chars[5] == ":", chars[8] == " ",
          chars[0].isNumber, chars[1].isNumber, chars[3].isNumber, chars[4].isNumber,
          chars[6].isNumber, chars[7].isNumber else {
        return ""
    }
    return String(chars[0..<8])
}

/// The message with the bridge's timestamp removed, used to detect repeats.
public func logMessage(of line: String) -> String {
    logStamp(of: line).isEmpty ? line : String(line.dropFirst(9))
}

extension String {
    /// `self` without a leading `prefix`, or `nil` when it is not there.
    func dropPrefix(_ prefix: String) -> Substring? {
        hasPrefix(prefix) ? dropFirst(prefix.count) : nil
    }
}

/// Split `INFO …` into its severity and the rest.
///
/// Only the five tokens `tracing` emits are recognised; anything else is left
/// alone so a message that merely starts with a word stays intact.
public func splitLevel(_ message: String) -> (level: LogLevel, text: String) {
    for (token, level) in [("INFO", LogLevel.info), ("WARN", .warn), ("ERROR", .error)] {
        if message.hasPrefix(token + " ") {
            // The token is followed by padding from the formatter's level
            // column; that column is ours to drop, whitespace included.
            return (level, String(message.dropFirst(token.count).drop { $0 == " " }))
        }
    }
    if message.hasPrefix("DEBUG ") || message.hasPrefix("TRACE ") {
        return (.other, String(message.dropFirst(5).drop { $0 == " " }))
    }
    return (.other, message)
}

/// Filter, collapse consecutive repeats and cap the view.
///
/// The disk log keeps everything; this only decides what the pane renders, so
/// a long-running session can never grow the view without bound.
public func renderLogLines(_ lines: [String], filter: LogFilter, limit: Int = 200) -> [LogLine] {
    // A request banner (`SOCKS5 target: …`, `HTTP CONNECT → …`) is logged
    // before its routing verdict, so the only way to know whether it belongs
    // to a tunnelled flow is to look up the verdict that follows it.
    let directTargets = Set(lines.compactMap { LogRoute.directTarget(logMessage(of: $0)) })
    var rendered: [(stamp: String, message: String, repeats: Int, level: LogLevel)] = []
    for line in lines where !line.isEmpty {
        let stamp = logStamp(of: line)
        let message = logMessage(of: line)
        if filter == .tunnel {
            if LogRoute.isDirect(message) { continue }
            if let target = LogRoute.bannerTarget(message), directTargets.contains(target) {
                continue
            }
        }
        if var last = rendered.last, last.message == message, !rendered.isEmpty {
            last.repeats += 1
            last.stamp = stamp
            rendered[rendered.count - 1] = last
            continue
        }
        let split = splitLevel(message)
        rendered.append((stamp, message, 1, split.level))
    }
    if rendered.count > limit {
        rendered = Array(rendered.suffix(limit))
    }
    return rendered.enumerated().map { index, row in
        let prefix = row.stamp.isEmpty ? "" : row.stamp + " "
        let suffix = row.repeats > 1 ? " ×\(row.repeats)" : ""
        let body = collapseSpaces(splitLevel(row.message).text)
        return LogLine(
            id: index,
            text: prefix + body + suffix,
            repeats: row.repeats,
            level: row.level
        )
    }
}

/// Squeeze runs of spaces down to one.
///
/// `tracing` pads the severity column, so a rendered line carries two spaces
/// more often than not — invisible in isolation, but it is another character
/// stolen from a line that has to fit in a 50-column pane.
public func collapseSpaces(_ text: String) -> String {
    var out = ""
    out.reserveCapacity(text.count)
    var previousWasSpace = false
    for character in text {
        if character == " " {
            if previousWasSpace { continue }
            previousWasSpace = true
        } else {
            previousWasSpace = false
        }
        out.append(character)
    }
    return out
}
