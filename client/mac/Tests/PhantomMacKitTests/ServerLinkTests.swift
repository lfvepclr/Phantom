import XCTest
@testable import PhantomMacKit

final class ServerLinkTests: XCTestCase {
    func testParsesMinimalUriWithDefaultPort() {
        let link = parseServerURI("phantom://dGVzdA==@example.com")
        XCTAssertTrue(link.valid)
        XCTAssertEqual(link.host, "example.com")
        XCTAssertEqual(link.port, 443)
        XCTAssertEqual(link.cipher, "auto")
        XCTAssertEqual(link.proto, "tcp")
    }

    func testParsesFullUri() {
        let link = parseServerURI(
            "phantom://c2VydmVyLWtleQ==@203.0.113.10:8443"
                + "?psk=cHNr&cipher=aes-256-gcm&proto=quic#tokyo"
        )
        XCTAssertTrue(link.valid)
        XCTAssertEqual(link.key, "c2VydmVyLWtleQ==")
        XCTAssertEqual(link.psk, "cHNr")
        XCTAssertEqual(link.host, "203.0.113.10")
        XCTAssertEqual(link.port, 8443)
        XCTAssertEqual(link.cipher, "aes-256-gcm")
        XCTAssertEqual(link.proto, "quic")
        XCTAssertEqual(link.name, "tokyo")
    }

    func testParsesBracketedIPv6() {
        let link = parseServerURI("phantom://a2V5@[2001:db8::1]:9443")
        XCTAssertTrue(link.valid)
        XCTAssertEqual(link.host, "2001:db8::1")
        XCTAssertEqual(link.port, 9443)
    }

    func testRejectsMalformedInput() {
        XCTAssertFalse(parseServerURI("").valid)
        XCTAssertFalse(parseServerURI("https://example.com").valid)
        XCTAssertFalse(parseServerURI("phantom://a2V5@example.com:not-a-port").valid)
        XCTAssertFalse(parseServerURI("phantom://a2V5@example.com:70000").valid)
    }

    func testPresentationHelpers() {
        let link = parseServerURI("phantom://a2V5@example.com:443?cipher=chacha20-poly1305")
        XCTAssertEqual(linkAddress(link), "example.com:443")
        XCTAssertEqual(linkSummary(link), "example.com:443 · TCP · ChaCha20")
        XCTAssertEqual(cipherLabel("auto"), "自动（AES-256 优先）")
        XCTAssertEqual(shortFingerprint("0123456789abcdef"), "0123456789…")
        XCTAssertEqual(shortFingerprint("short"), "short")
        XCTAssertEqual(linkAddress(ServerLink()), "未配置")
    }
}
