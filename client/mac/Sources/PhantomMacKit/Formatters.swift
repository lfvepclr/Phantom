import Foundation

/// `1.2 MB/s` — throughput for the server card and the info panel.
public func formatRate(_ bytesPerSecond: Double) -> String {
    if bytesPerSecond < 1024 { return "\(Int(bytesPerSecond)) B/s" }
    if bytesPerSecond < 1024 * 1024 { return String(format: "%.1f KB/s", bytesPerSecond / 1024) }
    return String(format: "%.2f MB/s", bytesPerSecond / (1024 * 1024))
}

/// `12.3 MB` — cumulative counters.
public func formatBytes(_ bytes: UInt64) -> String {
    let value = Double(bytes)
    if value < 1024 { return "\(bytes) B" }
    if value < 1024 * 1024 { return String(format: "%.1f KB", value / 1024) }
    if value < 1024 * 1024 * 1024 { return String(format: "%.1f MB", value / (1024 * 1024)) }
    return String(format: "%.2f GB", value / (1024 * 1024 * 1024))
}

/// `12 秒` / `3 分 5 秒` / `2 小时 7 分` — connection uptime.
public func formatDuration(seconds: Int) -> String {
    let total = max(0, seconds)
    let hours = total / 3600
    let minutes = (total % 3600) / 60
    let secs = total % 60
    if hours > 0 { return "\(hours) 小时 \(minutes) 分" }
    if minutes > 0 { return "\(minutes) 分 \(secs) 秒" }
    return "\(secs) 秒"
}
