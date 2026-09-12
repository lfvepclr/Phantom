import Foundation

/// Result of normalising one whitelist line.
public enum WhitelistEntry: Equatable, Sendable {
    case accepted(String)
    case rejected(reason: String)
}

public enum ProxyWhitelist {
    /// Normalise and validate a single entry.
    ///
    /// Accepts what people actually paste — a full URL, a `*.example.com`
    /// glob, a tagged line — and reduces it to the bare domain suffix the Rust
    /// side matches against. Rejects anything that is not a hostname so a typo
    /// cannot silently become a rule that never fires.
    public static func normalize(_ raw: String) -> WhitelistEntry {
        var text = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !text.isEmpty else { return .rejected(reason: "空行") }

        if text.hasPrefix("#") { return .rejected(reason: "注释") }

        // Pasted rules often carry a tag: `DOMAIN-SUFFIX,example.com` or
        // `example.com # comment`.
        if let hash = text.firstIndex(of: "#"), text.distance(from: text.startIndex, to: hash) > 0 {
            text = String(text[..<hash]).trimmingCharacters(in: .whitespaces)
        }
        if let comma = text.firstIndex(of: ",") {
            let head = text[..<comma].uppercased()
            if ["DOMAIN", "DOMAIN-SUFFIX", "DOMAIN-KEYWORD", "RULE-SET", "PROXY", "DIRECT"].contains(head) {
                text = String(text[text.index(after: comma)...]).trimmingCharacters(in: .whitespaces)
            }
        }
        if let space = text.firstIndex(of: " ") {
            // `DOMAIN-SUFFIX example.com`
            let head = text[..<space].uppercased()
            if head.hasPrefix("DOMAIN") {
                text = String(text[text.index(after: space)...]).trimmingCharacters(in: .whitespaces)
            }
        }

        // Strip scheme and any path/query that came along with a pasted URL.
        if let range = text.range(of: "://") {
            text = String(text[range.upperBound...])
        }
        if let slash = text.firstIndex(of: "/") {
            text = String(text[..<slash])
        }
        if let query = text.firstIndex(where: { $0 == "?" || $0 == "&" }) {
            text = String(text[..<query])
        }
        // A domain suffix rule never carries a port.
        if let colon = text.lastIndex(of: ":") {
            let portPart = text[text.index(after: colon)...]
            if !portPart.isEmpty && portPart.allSatisfy({ $0.isNumber }) {
                text = String(text[..<colon])
            }
        }
        text = text.trimmingCharacters(in: CharacterSet(charactersIn: "."))
        text = text.lowercased()
        guard !text.isEmpty else { return .rejected(reason: "缺少域名") }
        if text.hasPrefix("*.") { text = String(text.dropFirst(2)) }
        if text.hasPrefix(".") { text = String(text.dropFirst()) }

        guard !isIPv4(text) else {
            return .rejected(reason: "IP 需要按 IP-CIDR 规则处理，请填域名")
        }
        guard text.count <= 253 else { return .rejected(reason: "域名过长") }
        for label in text.split(separator: ".", omittingEmptySubsequences: false) {
            guard !label.isEmpty, label.count <= 63 else {
                return .rejected(reason: "域名标签长度不合法")
            }
            let allowed = label.allSatisfy { $0.isLetter || $0.isNumber || $0 == "-" }
            guard allowed, !label.hasPrefix("-"), !label.hasSuffix("-") else {
                return .rejected(reason: "域名含非法字符")
            }
        }
        guard text.contains(".") else { return .rejected(reason: "需要形如 example.com") }
        // A fully numeric "domain" is an IP that slipped past the check above
        // (e.g. `999.1.1.1`), which the suffix matcher would never handle.
        if let tld = text.split(separator: ".").last, tld.allSatisfy({ $0.isNumber }) {
            return .rejected(reason: "IP 需要按 IP-CIDR 规则处理，请填域名")
        }
        return .accepted(text)
    }

    /// Normalise a whole blob of pasted text (newlines, commas, whitespace).
    /// Returns the accepted domains (order preserved, deduplicated) plus the
    /// rejected fragments with the reason, so the editor can explain itself.
    public static func normalizeList(_ text: String) -> (accepted: [String], rejected: [String]) {
        var accepted: [String] = []
        var rejected: [String] = []
        for rawLine in text.split(separator: "\n", omittingEmptySubsequences: false) {
            // Whole-line comments (Clash/ClashX lists are full of them) are
            // dropped before splitting, otherwise every word in the comment
            // would come back as a rejected fragment.
            let line = rawLine.trimmingCharacters(in: .whitespaces)
            if line.isEmpty || line.hasPrefix("#") { continue }
            for fragment in line.split(whereSeparator: { $0 == "," || $0 == " " || $0 == "\t" || $0 == "\r" }) {
                switch normalize(String(fragment)) {
                case .accepted(let domain):
                    if !accepted.contains(domain) { accepted.append(domain) }
                case .rejected(let reason):
                    if reason != "注释" && reason != "空行" {
                        rejected.append("\(fragment)（\(reason)）")
                    }
                }
            }
        }
        return (accepted, rejected)
    }

    /// Merge new domains into an existing list, preserving order and dropping
    /// duplicates.
    public static func merge(_ existing: [String], _ additions: [String]) -> [String] {
        var merged = existing
        for domain in additions where !merged.contains(domain) {
            merged.append(domain)
        }
        return merged
    }

    /// The text handed to the Rust side (`phantom_macos_set_proxy_domains`).
    public static func serialize(_ domains: [String]) -> String {
        domains.joined(separator: "\n")
    }

    private static func isIPv4(_ text: String) -> Bool {
        let parts = text.split(separator: ".", omittingEmptySubsequences: false)
        guard parts.count == 4 else { return false }
        return parts.allSatisfy { part in
            !part.isEmpty && part.count <= 3 && part.allSatisfy { $0.isNumber }
                && (Int(part) ?? 256) <= 255
        }
    }
}
