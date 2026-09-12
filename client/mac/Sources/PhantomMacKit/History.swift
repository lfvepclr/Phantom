import Foundation

/// One remembered connection.
///
/// `verifiedAt` is only set once a tunnel with this URI actually reached
/// `running`, so a tick next to an entry means "this one worked on this Mac",
/// not merely "it was typed once".
public struct ServerHistoryEntry: Equatable, Sendable {
    public var uri: String
    public var lastUsedAt: Date
    public var verifiedAt: Date?

    public init(uri: String, lastUsedAt: Date, verifiedAt: Date? = nil) {
        self.uri = uri
        self.lastUsedAt = lastUsedAt
        self.verifiedAt = verifiedAt
    }
}

/// How many connections are remembered (most recent first).
public let historyMax = 20

/// Persisted form: one entry per line, `uri \t lastUsedEpochMs \t verifiedEpochMs`.
///
/// A tab-separated line keeps the parser trivial (a URI never contains a tab)
/// and keeps the stored preference readable when dumped from the terminal.
public func serializeHistory(_ entries: [ServerHistoryEntry]) -> String {
    entries.map { entry in
        let verified = entry.verifiedAt.map { String(Int($0.timeIntervalSince1970 * 1000)) } ?? "0"
        return "\(entry.uri)\t\(Int(entry.lastUsedAt.timeIntervalSince1970 * 1000))\t\(verified)"
    }.joined(separator: "\n")
}

public func parseHistory(_ text: String) -> [ServerHistoryEntry] {
    var entries: [ServerHistoryEntry] = []
    for rawLine in text.split(separator: "\n", omittingEmptySubsequences: true) {
        let line = rawLine.trimmingCharacters(in: .whitespaces)
        guard !line.isEmpty else { continue }
        let parts = line.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
        let uri = parts.first?.trimmingCharacters(in: .whitespaces) ?? ""
        guard uri.hasPrefix("phantom://") else { continue }
        let lastUsed = (parts.count > 1 ? epochMillisToDate(parts[1]) : nil)
            ?? Date(timeIntervalSince1970: 0)
        let verified = parts.count > 2 ? epochMillisToDate(parts[2]) : nil
        entries.append(ServerHistoryEntry(uri: uri, lastUsedAt: lastUsed, verifiedAt: verified))
    }
    return sortAndDedupe(entries)
}

/// Record a use: move the URI to the front, keep the newest verified stamp and
/// its original `verifiedAt` when it was not re-verified this time.
public func upsertHistory(
    _ entries: [ServerHistoryEntry],
    uri: String,
    usedAt: Date = Date(),
    verifiedAt: Date? = nil
) -> [ServerHistoryEntry] {
    var verified = verifiedAt
    for entry in entries where entry.uri == uri {
        guard let previous = entry.verifiedAt else { continue }
        if let current = verified {
            verified = max(previous, current)
        } else {
            verified = previous
        }
    }
    var next = entries.filter { $0.uri != uri }
    next.append(ServerHistoryEntry(uri: uri, lastUsedAt: usedAt, verifiedAt: verified))
    return Array(sortAndDedupe(next).prefix(historyMax))
}

public func removeFromHistory(_ entries: [ServerHistoryEntry], uri: String) -> [ServerHistoryEntry] {
    entries.filter { $0.uri != uri }
}

private func sortAndDedupe(_ entries: [ServerHistoryEntry]) -> [ServerHistoryEntry] {
    var seen = Set<String>()
    var unique: [ServerHistoryEntry] = []
    for entry in entries.sorted(by: { $0.lastUsedAt > $1.lastUsedAt }) where !seen.contains(entry.uri) {
        seen.insert(entry.uri)
        unique.append(entry)
    }
    return unique
}

private func epochMillisToDate(_ text: String) -> Date? {
    guard let millis = Double(text.trimmingCharacters(in: .whitespaces)), millis > 0 else {
        return nil
    }
    return Date(timeIntervalSince1970: millis / 1000)
}

/// `刚刚` / `5 分钟前` / `3 天前`, for the history menu.
public func relativeTime(_ then: Date?, now: Date = Date()) -> String {
    guard let then, then.timeIntervalSince1970 > 0 else { return "" }
    let seconds = max(0, Int(now.timeIntervalSince(then)))
    if seconds < 60 { return "刚刚" }
    let minutes = seconds / 60
    if minutes < 60 { return "\(minutes) 分钟前" }
    let hours = minutes / 60
    if hours < 24 { return "\(hours) 小时前" }
    return "\(hours / 24) 天前"
}
