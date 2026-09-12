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

    /// Lines the datapath marked as routed straight to the internet.
    static let directMarker = "-> Direct ("
}

/// One rendered row: the text plus a stable id for `ForEach`.
public struct LogLine: Identifiable, Equatable, Sendable {
    public let id: Int
    public let text: String
    /// How many consecutive identical messages this row stands for (1 = once).
    public let repeats: Int
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

/// Filter, collapse consecutive repeats and cap the view.
///
/// The disk log keeps everything; this only decides what the pane renders, so
/// a long-running session can never grow the view without bound.
public func renderLogLines(_ lines: [String], filter: LogFilter, limit: Int = 200) -> [LogLine] {
    var rendered: [(stamp: String, message: String, repeats: Int)] = []
    for line in lines where !line.isEmpty {
        let stamp = logStamp(of: line)
        let message = logMessage(of: line)
        if filter == .tunnel && message.contains(LogFilter.directMarker) {
            continue
        }
        if var last = rendered.last, last.message == message, !rendered.isEmpty {
            last.repeats += 1
            last.stamp = stamp
            rendered[rendered.count - 1] = last
            continue
        }
        rendered.append((stamp, message, 1))
    }
    if rendered.count > limit {
        rendered = Array(rendered.suffix(limit))
    }
    return rendered.enumerated().map { index, row in
        let prefix = row.stamp.isEmpty ? "" : row.stamp + " "
        let suffix = row.repeats > 1 ? " ×\(row.repeats)" : ""
        return LogLine(id: index, text: prefix + row.message + suffix, repeats: row.repeats)
    }
}
