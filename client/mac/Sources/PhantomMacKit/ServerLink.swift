import Foundation

/// Parsed `phantom://` quick link.
///
/// The UI never shows the raw string: every screen renders the structural
/// fields produced here (address, transport, cipher) instead, because a
/// base64 blob tells the operator nothing about *where* the traffic goes.
///
/// Format:
///   `phantom://<base64 server key>@<host>:<port>?psk=<base64>&cipher=<c>&proto=<p>#<name>`
///
/// Mirrors `client/harmony/entry/src/main/ets/common/ServerLink.ets` so both
/// clients describe the same URI identically.
public struct ServerLink: Equatable, Sendable {
    /// Base64 server public key (the URI authority).
    public var key: String = ""
    /// Pre-shared key from the query string.
    public var psk: String = ""
    public var host: String = ""
    public var port: Int = 0
    /// `auto` | `aes-256-gcm` | `aes-128-gcm` | `ascon-128` | `chacha20-poly1305`.
    public var cipher: String = "auto"
    /// `tcp` | `quic`.
    public var proto: String = "tcp"
    /// `#fragment` — the node label chosen when the server was bootstrapped.
    public var name: String = ""
    public var valid: Bool = false

    public init() {}
}

/// Parse a quick link. Never traps: malformed input comes back `valid == false`
/// so the UI can say "连接串格式不正确" instead of crashing.
public func parseServerURI(_ uri: String) -> ServerLink {
    var link = ServerLink()
    let trimmed = uri.trimmingCharacters(in: .whitespacesAndNewlines)
    guard trimmed.hasPrefix("phantom://") else { return link }

    var rest = String(trimmed.dropFirst("phantom://".count))

    if let hash = rest.firstIndex(of: "#") {
        link.name = rest[rest.index(after: hash)...].trimmingCharacters(in: .whitespaces)
        rest = String(rest[..<hash])
    }

    if let query = rest.firstIndex(of: "?") {
        for pair in rest[rest.index(after: query)...].split(separator: "&") {
            let parts = pair.split(separator: "=", maxSplits: 1, omittingEmptySubsequences: false)
            guard parts.count == 2 else { continue }
            let key = String(parts[0])
            let value = String(parts[1])
            switch key {
            case "psk": link.psk = value
            case "cipher" where !value.isEmpty: link.cipher = value
            case "proto" where !value.isEmpty: link.proto = value
            default: break
            }
        }
        rest = String(rest[..<query])
    }

    guard let at = rest.lastIndex(of: "@") else { return link }
    link.key = String(rest[..<at])
    let authority = String(rest[rest.index(after: at)...])

    if authority.hasPrefix("[") {
        // Bracketed IPv6: `[::1]:443`.
        guard let close = authority.firstIndex(of: "]") else { return link }
        link.host = String(authority[authority.index(after: authority.startIndex)..<close])
        let afterHost = authority.index(after: close)
        if let colon = authority[afterHost...].firstIndex(of: ":") {
            link.port = parsePort(String(authority[authority.index(after: colon)...]))
        } else {
            link.port = 443
        }
    } else if let colon = authority.lastIndex(of: ":") {
        link.host = String(authority[..<colon])
        link.port = parsePort(String(authority[authority.index(after: colon)...]))
    } else {
        link.host = authority
        link.port = 443
    }

    link.valid = !link.key.isEmpty && !link.host.isEmpty && link.port > 0
    return link
}

private func parsePort(_ text: String) -> Int {
    let trimmed = text.trimmingCharacters(in: .whitespaces)
    guard !trimmed.isEmpty, trimmed.allSatisfy({ $0.isNumber }), let value = Int(trimmed) else {
        return 0
    }
    return (1...65535).contains(value) ? value : 0
}

/// `203.0.113.10:443` — a connection has no meaningful name, so lists show what
/// the operator can actually recognise: where it connects to.
public func linkAddress(_ link: ServerLink) -> String {
    link.valid ? "\(link.host):\(link.port)" : "未配置"
}

/// One-line description of where traffic goes, e.g. `host:443 · TCP · AES-256-GCM`.
public func linkSummary(_ link: ServerLink) -> String {
    guard link.valid else { return "尚未填写连接串" }
    return "\(link.host):\(link.port) · \(link.proto.uppercased()) · \(cipherLabel(link.cipher))"
}

public func cipherLabel(_ cipher: String) -> String {
    switch cipher {
    case "aes-256-gcm": return "AES-256-GCM"
    case "aes-128-gcm": return "AES-128-GCM"
    case "chacha20-poly1305": return "ChaCha20"
    case "ascon-128": return "Ascon-128"
    case "auto": return "自动（AES-256 优先）"
    default: return cipher
    }
}

/// First characters of a base64 key/PSK: enough to tell two nodes apart without
/// putting the whole secret on screen.
public func shortFingerprint(_ value: String) -> String {
    value.count <= 10 ? value : String(value.prefix(10)) + "…"
}
