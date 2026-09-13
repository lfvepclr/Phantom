package co.phantom.android

/*
 * Parsing, normalising and serialising user whitelist rules.
 *
 * Pure JVM on purpose: the HarmonyOS client runs the same rules (RuleFormat.ets),
 * and both must accept or reject exactly the same input, or a rule that works on
 * one phone silently does nothing on the other. The wire format this produces is
 * what `RustBridge.setUserRules` hands to the core — one `kind:value` per line.
 */

/** The five rule shapes the core understands. */
enum class RuleKind(val prefix: String, val label: String) {
    DOMAIN("domain", "精确域名"),
    SUFFIX("suffix", "通配域名"),
    KEYWORD("keyword", "关键字"),
    REGEX("regex", "正则"),
    CIDR("cidr", "IP 网段");

    companion object {
        fun from(prefix: String): RuleKind? = entries.firstOrNull { it.prefix == prefix }
    }
}

/** Outcome of parsing one line the user typed. */
sealed interface RuleParse {
    /** Accepted; [wire] is the line to hand to the core. */
    data class Ok(val kind: RuleKind, val value: String, val wire: String) : RuleParse

    /** Rejected; [reason] is shown to the user next to the input. */
    data class Invalid(val reason: String) : RuleParse
}

object RuleFormat {

    /**
     * Parse one line of user input.
     *
     * Deliberately forgiving about *formatting* and strict about *meaning*: a
     * pasted URL or a Clash-style `DOMAIN-SUFFIX,example.com` is normalised away,
     * while a bare IP is refused with a pointer to the CIDR kind — because an IP
     * in a domain rule would silently never match.
     */
    fun parse(raw: String): RuleParse {
        var text = raw.trim()
        if (text.isEmpty()) return RuleParse.Invalid("空行")
        if (text.startsWith("#")) return RuleParse.Invalid("注释")
        text.substringBefore('#').trim().let { if (it.isNotEmpty()) text = it }

        // An explicit kind, either our own `suffix:host` or a Clash-style head.
        // Both have to be stripped off here, or the value handed down would
        // still carry the prefix it was recognised by.
        var declared: RuleKind? = null
        if (text.contains(':')) {
            RuleKind.from(text.substringBefore(':').trim())?.let { kind ->
                declared = kind
                text = text.substringAfter(':').trim()
            }
        }
        val head = text.substringBefore(',').substringBefore(' ').uppercase()
        CLASH_KINDS[head]?.let { clashKind ->
            declared = clashKind
            text = text.substringAfter(',').substringAfter(' ').trim()
        }

        // Only a real URL carries a scheme, and only then may path, query and
        // fragment be stripped: doing it unconditionally would eat the `/22`
        // out of a CIDR and the `?` out of a regex.
        if (text.contains("://")) {
            text = text.substringAfter("://")
                .substringBefore('/')
                .substringBefore('?')
                .substringBefore('&')
        }

        return parsePlain(text, declared)
    }

    /** Serialise a validated entry into the wire format. */
    fun toWire(kind: RuleKind, value: String): String = "${kind.prefix}:$value"

    /** Split wire text back into entries, dropping anything unparseable. */
    fun fromWire(text: String): List<Pair<RuleKind, String>> = text
        .split('\n')
        .mapNotNull { line ->
            val kind = RuleKind.from(line.substringBefore(':').trim())
            val value = line.substringAfter(':', "").trim()
            if (kind == null || value.isEmpty()) null else kind to value
        }

    /**
     * Fold an inclusive IPv4 range into the smallest set of CIDR blocks.
     *
     * The rule engine only speaks CIDR, so an `a.b.c.d - e.f.g.h` entry has to be
     * expressed as a cover. Returns `null` when the range is not a valid, ordered
     * pair of IPv4 addresses.
     */
    fun ipRangeToCidrs(from: String, to: String): List<String>? {
        val start = parseIpv4(from) ?: return null
        val end = parseIpv4(to) ?: return null
        if (start > end) return null

        val out = mutableListOf<String>()
        var lo = start
        val hi = end
        while (lo <= hi) {
            // A block must start on its own boundary, so its size is capped by
            // how many low bits of `lo` are zero.
            val aligned = if (lo == 0L) 32 else java.lang.Long.numberOfTrailingZeros(lo)
            var size = 0
            while (size < aligned && lo + (1L shl size) - 1 < hi) size++
            out.add("${longToIp(lo)}/${32 - size}")
            lo += 1L shl size
        }
        return out
    }

    // ---------------------------------------------------------------------

    /** Clash-style rule heads, so a pasted rule from another tool still works. */
    private val CLASH_KINDS = mapOf(
        "DOMAIN" to RuleKind.DOMAIN,
        "DOMAIN-SUFFIX" to RuleKind.SUFFIX,
        "DOMAIN-KEYWORD" to RuleKind.KEYWORD,
        "DOMAIN-REGEX" to RuleKind.REGEX,
        "IP-CIDR" to RuleKind.CIDR,
    )

    private fun parsePlain(raw: String, kind: RuleKind?): RuleParse {
        var text = raw.trim().lowercase()
        if (text.isEmpty()) return RuleParse.Invalid("内容为空")

        // `*.example.com` and `.example.com` are both the suffix kind.
        if (kind == null && (text.startsWith("*.") || text.startsWith("."))) {
            text = text.removePrefix("*.").removePrefix(".")
            return finish(RuleKind.SUFFIX, text)
        }
        val resolved = kind ?: inferKind(text)
        return when (resolved) {
            RuleKind.CIDR -> if (parseIpv4Cidr(text) == null) {
                RuleParse.Invalid("网段要形如 91.108.4.0/22")
            } else {
                finish(RuleKind.CIDR, text)
            }

            RuleKind.REGEX -> {
                try {
                    Regex(text)
                } catch (e: Exception) {
                    return RuleParse.Invalid("正则无法编译：${e.message?.take(40) ?: "语法错误"}")
                }
                finish(RuleKind.REGEX, text)
            }

            RuleKind.KEYWORD -> if (text.contains('.')) {
                RuleParse.Invalid("关键字不要带点；按后缀匹配请用通配域名")
            } else {
                finish(RuleKind.KEYWORD, text)
            }

            else -> {
                // A bare IP is never a domain: it would silently never match.
                if (parseIpv4(text) != null) {
                    RuleParse.Invalid("这是 IP，请改用「IP 网段」")
                } else if (!isHostname(text)) {
                    RuleParse.Invalid("不是合法的域名")
                } else {
                    // Both DOMAIN and SUFFIX take a bare host here; the `*.`
                    // prefix has already been peeled off above.
                    finish(resolved, text)
                }
            }
        }
    }

    private fun inferKind(value: String): RuleKind = when {
        value.contains('/') -> RuleKind.CIDR
        else -> RuleKind.DOMAIN
    }

    private fun finish(kind: RuleKind, value: String): RuleParse =
        RuleParse.Ok(kind, value, toWire(kind, value))

    private fun isHostname(text: String): Boolean {
        if (text.length > 253) return false
        if (!text.contains('.')) return false
        for (label in text.split('.')) {
            if (label.isEmpty() || label.length > 63) return false
            if (!label.all { it.isLetterOrDigit() || it == '-' }) return false
            if (label.startsWith("-") || label.endsWith("-")) return false
        }
        return true
    }

    /** Dotted quad → unsigned value in the low 32 bits, or `null`. */
    private fun parseIpv4(text: String): Long? {
        val parts = text.split('.')
        if (parts.size != 4) return null
        var value = 0L
        for (part in parts) {
            if (part.isEmpty() || part.length > 3 || part.any { !it.isDigit() }) return null
            val octet = part.toLongOrNull() ?: return null
            if (octet > 255) return null
            value = (value shl 8) or octet
        }
        return value
    }

    private fun parseIpv4Cidr(text: String): Long? {
        val slash = text.indexOf('/')
        if (slash <= 0) return null
        val addr = parseIpv4(text.substring(0, slash)) ?: return null
        val prefix = text.substring(slash + 1).toIntOrNull() ?: return null
        if (prefix !in 0..32) return null
        return addr
    }

    private fun longToIp(value: Long): String =
        "${(value ushr 24) and 0xFF}.${(value ushr 16) and 0xFF}.${(value ushr 8) and 0xFF}." +
            "${value and 0xFF}"
}
