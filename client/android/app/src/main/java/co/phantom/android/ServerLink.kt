package co.phantom.android

/*
 * Parsing helpers for `phantom://` quick links.
 *
 * The UI never has to show the raw string: every screen that mentions a
 * connection renders the structural fields produced here instead.
 *
 * Format:
 *   phantom://<base64 server key>@<host>:<port>?psk=<base64>&cipher=<c>&proto=<p>#<name>
 *
 * This mirrors `client/harmony/entry/src/main/ets/common/ServerLink.ets`, down
 * to the persisted history encoding — the two clients share a phone in the
 * field, and a link that imports on one must render identically on the other.
 */

/** Structured view of a connection string. */
data class ServerLink(
    /** False for malformed input, so the UI can say "格式不正确" instead of crashing. */
    val valid: Boolean = false,
    /** Base64 server public key (the URI authority). */
    val key: String = "",
    /** Pre-shared key from the query string. */
    val psk: String = "",
    val host: String = "",
    val port: Int = 0,
    /** `auto` | `aes-256-gcm` | `aes-128-gcm` | `ascon-128` | `chacha20-poly1305`. */
    val cipher: String = "auto",
    /** `tcp` | `quic`. */
    val proto: String = "tcp",
    /** `#fragment` — the node label chosen when the server was bootstrapped. */
    val name: String = "",
)

/** Parse a quick link. Never throws: malformed input comes back `valid = false`. */
fun parseServerUri(uri: String): ServerLink {
    val trimmed = uri.trim()
    if (!trimmed.startsWith("phantom://")) {
        return ServerLink()
    }
    var rest = trimmed.substring("phantom://".length)
    var name = ""
    var psk = ""
    var cipher = "auto"
    var proto = "tcp"

    val hash = rest.indexOf('#')
    if (hash >= 0) {
        name = rest.substring(hash + 1).trim()
        rest = rest.substring(0, hash)
    }
    val query = rest.indexOf('?')
    if (query >= 0) {
        for (pair in rest.substring(query + 1).split('&')) {
            val eq = pair.indexOf('=')
            if (eq < 0) continue
            val value = pair.substring(eq + 1)
            when (pair.substring(0, eq)) {
                "psk" -> psk = value
                "cipher" -> if (value.isNotEmpty()) cipher = value
                "proto" -> if (value.isNotEmpty()) proto = value
            }
        }
        rest = rest.substring(0, query)
    }

    val at = rest.lastIndexOf('@')
    if (at < 0) {
        return ServerLink()
    }
    val key = rest.substring(0, at)
    val authority = rest.substring(at + 1)

    // Bracketed IPv6 (`[::1]:443`) or the usual host:port.
    val host: String
    val port: Int
    if (authority.startsWith("[")) {
        val close = authority.indexOf(']')
        if (close < 0) {
            return ServerLink()
        }
        host = authority.substring(1, close)
        val colon = authority.indexOf(':', close)
        port = if (colon < 0) 443 else toPort(authority.substring(colon + 1))
    } else {
        val colon = authority.lastIndexOf(':')
        if (colon < 0) {
            host = authority
            port = 443
        } else {
            host = authority.substring(0, colon)
            port = toPort(authority.substring(colon + 1))
        }
    }

    return ServerLink(
        valid = key.isNotEmpty() && host.isNotEmpty() && port > 0,
        key = key,
        psk = psk,
        host = host,
        port = port,
        cipher = cipher,
        proto = proto,
        name = name,
    )
}

private fun toPort(text: String): Int {
    val trimmed = text.trim()
    if (trimmed.isEmpty() || !trimmed.all { it.isDigit() }) {
        return 0
    }
    val value = trimmed.toIntOrNull() ?: return 0
    return if (value in 1..65535) value else 0
}

/** Node label for the UI: the fragment when present, otherwise the address. */
fun linkTitle(link: ServerLink): String =
    when {
        !link.valid -> "未配置"
        link.name.isNotEmpty() -> link.name
        else -> "${link.host}:${link.port}"
    }

/** One-line description of where traffic goes, e.g. `1.2.3.4:443 · TCP · AES-256`. */
fun linkSummary(link: ServerLink): String =
    if (!link.valid) {
        "尚未填写连接串"
    } else {
        "${link.host}:${link.port} · ${link.proto.uppercase()} · ${cipherLabel(link.cipher)}"
    }

fun cipherLabel(cipher: String): String = when (cipher) {
    "aes-256-gcm" -> "AES-256-GCM"
    "aes-128-gcm" -> "AES-128-GCM"
    "chacha20-poly1305" -> "ChaCha20"
    "ascon-128" -> "Ascon-128"
    "auto" -> "自动（AES-256 优先）"
    else -> cipher
}

/**
 * First characters of a base64 key/PSK: enough to tell two nodes apart without
 * putting the whole secret on screen.
 */
fun shortFingerprint(value: String): String =
    if (value.length <= 10) value else value.substring(0, 10) + "…"

// ---------------------------------------------------------------------------
// Connection history
// ---------------------------------------------------------------------------

/**
 * One remembered connection.
 *
 * `verifiedMs` is only set once a tunnel with this URI actually reached
 * running, so a tick next to an entry means "this one worked on this phone",
 * not merely "it was typed once".
 */
data class ServerHistoryEntry(
    val uri: String,
    val lastUsedMs: Long,
    val verifiedMs: Long,
)

/** How many connections are remembered (most recent first). */
const val HISTORY_MAX: Int = 20

/**
 * Persisted form: one entry per line, `uri\tlastUsedMs\tverifiedMs`.
 *
 * Tab-separated keeps the parser trivial (a URI never contains a tab) and the
 * preference readable when dumped with `adb shell run-as`.
 */
fun serializeHistory(entries: List<ServerHistoryEntry>): String =
    entries.joinToString("\n") { "${it.uri}\t${it.lastUsedMs}\t${it.verifiedMs}" }

fun parseHistory(text: String): List<ServerHistoryEntry> {
    if (text.isEmpty()) {
        return emptyList()
    }
    val entries = mutableListOf<ServerHistoryEntry>()
    for (line in text.split('\n')) {
        val trimmed = line.trim()
        if (trimmed.isEmpty()) continue
        val parts = trimmed.split('\t')
        val uri = parts[0].trim()
        if (!uri.startsWith("phantom://")) continue
        entries += ServerHistoryEntry(
            uri = uri,
            lastUsedMs = parts.getOrNull(1)?.toLongOrNull() ?: 0L,
            verifiedMs = parts.getOrNull(2)?.toLongOrNull() ?: 0L,
        )
    }
    return sortAndDedupe(entries)
}

/**
 * Record a use of `uri`: move it to the front, keep the newest timestamps and
 * drop anything past [HISTORY_MAX].
 */
fun upsertHistory(
    entries: List<ServerHistoryEntry>,
    uri: String,
    usedAtMs: Long,
    verifiedAtMs: Long,
): List<ServerHistoryEntry> {
    val verified = maxOf(verifiedAtMs, entries.firstOrNull { it.uri == uri }?.verifiedMs ?: 0L)
    val next = entries.filter { it.uri != uri } + ServerHistoryEntry(uri, usedAtMs, verified)
    return sortAndDedupe(next).take(HISTORY_MAX)
}

/** Stamp `uri` as proven-good without touching its position in the list. */
fun markVerified(
    entries: List<ServerHistoryEntry>,
    uri: String,
    verifiedAtMs: Long,
): List<ServerHistoryEntry> =
    entries.map { if (it.uri == uri) it.copy(verifiedMs = verifiedAtMs) else it }

fun removeFromHistory(
    entries: List<ServerHistoryEntry>,
    uri: String,
): List<ServerHistoryEntry> = entries.filter { it.uri != uri }

private fun sortAndDedupe(entries: List<ServerHistoryEntry>): List<ServerHistoryEntry> =
    entries.sortedByDescending { it.lastUsedMs }.distinctBy { it.uri }

/** `刚刚` / `5 分钟前` / `3 天前`, for the history menu. */
fun relativeTime(thenMs: Long, nowMs: Long): String {
    if (thenMs <= 0) return ""
    val seconds = ((nowMs - thenMs) / 1000).coerceAtLeast(0)
    if (seconds < 60) return "刚刚"
    val minutes = seconds / 60
    if (minutes < 60) return "$minutes 分钟前"
    val hours = minutes / 60
    if (hours < 24) return "$hours 小时前"
    return "${hours / 24} 天前"
}
