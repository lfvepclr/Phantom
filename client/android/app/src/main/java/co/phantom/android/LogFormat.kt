package co.phantom.android

/*
 * Log and number formatting, kept free of Android types so the rules are
 * unit-testable on the JVM. Mirrors the HarmonyOS client's `Index.ets`
 * helpers, including the repeat collapsing — a phone that keeps re-resolving
 * the same domestic domain would otherwise push everything useful out of the
 * 200-line window.
 */

/** `17:26:44` prefix we stamp onto each received line, or "" when absent. */
fun stampOf(line: String): String =
    if (line.length > 9 && line[2] == ':' && line[5] == ':' && line[8] == ' ') {
        line.substring(0, 8)
    } else {
        ""
    }

/** The line without its timestamp, used for dedupe comparisons. */
fun messageOf(line: String): String {
    val stamp = stampOf(line)
    return if (stamp.isEmpty()) line else line.substring(stamp.length).trimStart()
}

/** `INFO route …` -> `route …`, so matching does not have to know the level. */
fun logBody(message: String): String {
    for (token in LEVEL_TOKENS) {
        if (message.startsWith("$token ")) return message.substring(token.length + 1).trimStart()
    }
    return message
}

private val LEVEL_TOKENS = listOf("INFO", "WARN", "ERROR", "DEBUG", "TRACE")

/**
 * Tokens that only ever appear on a line whose flow went straight out.
 *
 * Matched case-insensitively because the TUN datapath writes `-> Direct (` and
 * the SOCKS5/HTTP listeners write `-> DIRECT (`; the clients used to match only
 * the first spelling, so their "tunnel only" filter let every direct flow of a
 * system-proxy session through.
 */
private val DIRECT_TOKENS = listOf(
    "-> direct (",
    "direct connection established",
    "direct connect failed",
    "direct http resolve failed",
    "direct http connect failed",
    "direct http connection established",
    "flow end (direct)",
)

/** A proxied verdict is never direct, whatever its reason text mentions. */
private val PROXY_TOKENS = listOf("-> proxy (")

fun isDirectLogLine(message: String): Boolean {
    val lower = message.lowercase()
    if (PROXY_TOKENS.any { lower.contains(it) }) return false
    return DIRECT_TOKENS.any { lower.contains(it) }
}

/** The target of a `route <target> -> Direct (…)` line, when there is one. */
fun directTargetOf(message: String): String? {
    if (!isDirectLogLine(message)) return null
    return routeTargetOf(logBody(message))
}

private fun routeTargetOf(body: String): String? {
    val arrow = body.indexOf(" -> ")
    if (arrow < 0) return null
    val head = body.substring(0, arrow)
    val keyword = head.lastIndexOf("route ")
    if (keyword < 0) return null
    val target = head.substring(keyword + "route ".length)
    return target.ifEmpty { null }
}

/**
 * The target of a request banner (`SOCKS5 target: …`, `HTTP CONNECT → …`).
 *
 * Those lines are written *before* the routing verdict, so on their own they
 * cannot say which way the flow went; the pane decides that by looking up the
 * verdict that follows.
 */
fun bannerTargetOf(message: String): String? {
    val body = logBody(message)
    for (prefix in listOf("SOCKS5 target: ", "HTTP CONNECT → ", "HTTP proxy → ")) {
        if (!body.startsWith(prefix)) continue
        val rest = body.substring(prefix.length)
        val end = rest.indexOf(" (")
        val target = if (end < 0) rest else rest.substring(0, end)
        return target.ifEmpty { null }
    }
    return null
}

/**
 * One rendered log line, tagged with a monotonic id.
 *
 * The id exists so the pane's `LazyColumn` can key its items: the visible
 * window is a sliding tail, so without a stable identity every append would
 * look like "all 200 rows changed" and Compose would rebind the lot. The id is
 * assigned once, when the line arrives, and survives filtering and collapsing.
 */
data class LogEntry(val id: Long, val text: String)

/**
 * Filter, collapse and cap the lines the log pane renders.
 *
 * Direct-routed traffic is the noisy majority (every background app talking to
 * a domestic server) and is rarely what you are debugging, so it is hidden
 * unless explicitly enabled; the mirrored file keeps everything meanwhile.
 *
 * Collapsing rewrites the text of the previous entry while keeping its id, so
 * "×3" growing on a line does not read as a new row.
 */
fun visibleLogEntries(
    raw: List<LogEntry>,
    showDirect: Boolean,
    limit: Int = LOG_VIEW_LINES,
): List<LogEntry> {
    val out = mutableListOf<LogEntry>()
    var prevMessage = ""
    var repeat = 0
    // A banner is logged before its verdict, so the verdicts have to be known
    // up front before any banner can be judged.
    val directTargets = if (showDirect) {
        emptySet()
    } else {
        raw.mapNotNull { directTargetOf(messageOf(it.text)) }.toSet()
    }
    for (entry in raw) {
        val line = entry.text
        if (line.isEmpty()) continue
        val message = messageOf(line)
        if (!showDirect && isDirectLogLine(message)) continue
        if (!showDirect && bannerTargetOf(message)?.let { it in directTargets } == true) continue
        if (message == prevMessage && out.isNotEmpty()) {
            repeat++
            val prev = out[out.size - 1]
            out[out.size - 1] = prev.copy(text = "${stampOf(line)} $message ×${repeat + 1}".trim())
            continue
        }
        prevMessage = message
        repeat = 0
        out += entry
    }
    return if (out.size <= limit) out else out.takeLast(limit)
}

/**
 * String-only view of [visibleLogEntries], for callers that carry no ids.
 *
 * Kept as the narrow entry point for tests and any future consumer that only
 * has text.
 */
fun visibleLogLines(raw: List<String>, showDirect: Boolean, limit: Int = LOG_VIEW_LINES): List<String> =
    visibleLogEntries(
        raw.mapIndexed { index, line -> LogEntry(index.toLong(), line) },
        showDirect,
        limit,
    ).map { it.text }

/** `1.2 MB/s` style, so a busy tunnel stays readable. */
fun formatRate(bytesPerSecond: Long): String = when {
    bytesPerSecond < 1024 -> "$bytesPerSecond B/s"
    bytesPerSecond < 1024 * 1024 -> String.format("%.1f KB/s", bytesPerSecond / 1024.0)
    else -> String.format("%.2f MB/s", bytesPerSecond / (1024.0 * 1024.0))
}

fun formatSize(bytes: Long): String = when {
    bytes < 1024 -> "$bytes B"
    bytes < 1024 * 1024 -> String.format("%.1f KB", bytes / 1024.0)
    bytes < 1024L * 1024 * 1024 -> String.format("%.1f MB", bytes / (1024.0 * 1024.0))
    else -> String.format("%.2f GB", bytes / (1024.0 * 1024.0 * 1024.0))
}

/** `2:05` / `1:02:09`, for the connection-details uptime row. */
fun formatDuration(seconds: Long): String {
    val hours = seconds / 3600
    val minutes = (seconds % 3600) / 60
    val secs = seconds % 60
    return if (hours > 0) {
        String.format("%d:%02d:%02d", hours, minutes, secs)
    } else {
        String.format("%d:%02d", minutes, secs)
    }
}
