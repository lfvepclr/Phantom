import XCTest
@testable import PhantomMacKit

final class WhitelistTests: XCTestCase {
    private func accepted(_ raw: String) -> String? {
        if case .accepted(let domain) = ProxyWhitelist.normalize(raw) { return domain }
        return nil
    }

    private func rejectionReason(_ raw: String) -> String? {
        if case .rejected(let reason) = ProxyWhitelist.normalize(raw) { return reason }
        return nil
    }

    func testNormalisesWhatPeopleActuallyPaste() {
        XCTAssertEqual(accepted("  Example.COM "), "example.com")
        XCTAssertEqual(accepted("https://www.example.com/path?q=1"), "www.example.com")
        XCTAssertEqual(accepted("*.example.com"), "example.com")
        XCTAssertEqual(accepted(".example.com"), "example.com")
        XCTAssertEqual(accepted("example.com:443"), "example.com")
        XCTAssertEqual(accepted("example.com # 备注"), "example.com")
        XCTAssertEqual(accepted("DOMAIN-SUFFIX,example.com"), "example.com")
        XCTAssertEqual(accepted("DOMAIN-SUFFIX example.com"), "example.com")
        XCTAssertEqual(accepted("xn--fiqs8s.example"), "xn--fiqs8s.example")
    }

    func testRejectsThingsThatWouldNeverMatch() {
        XCTAssertNotNil(rejectionReason("192.0.2.10"))
        XCTAssertNotNil(rejectionReason("localhost"))
        XCTAssertNotNil(rejectionReason("-bad.example.com"))
        XCTAssertNotNil(rejectionReason("bad domain.com"))
        XCTAssertEqual(rejectionReason("# comment"), "注释")
        XCTAssertEqual(rejectionReason(""), "空行")
    }

    func testListImportDedupesAndExplainsRejections() {
        let result = ProxyWhitelist.normalizeList(
            "example.com, www.example.com\nhttps://cdn.example.com/x\n999.1.1.1\n# note\n"
        )
        XCTAssertEqual(result.accepted, ["example.com", "www.example.com", "cdn.example.com"])
        XCTAssertEqual(result.rejected.count, 1)
        XCTAssertTrue(result.rejected[0].contains("IP"))
    }

    func testMergeAndSerialize() {
        let merged = ProxyWhitelist.merge(["a.example.com"], ["a.example.com", "b.example.com"])
        XCTAssertEqual(merged, ["a.example.com", "b.example.com"])
        XCTAssertEqual(ProxyWhitelist.serialize(merged), "a.example.com\nb.example.com")
    }
}

final class HistoryTests: XCTestCase {
    func testRoundTrip() {
        let entries = [
            ServerHistoryEntry(uri: "phantom://a2V5@example.com:443", lastUsedAt: Date(timeIntervalSince1970: 1_700_000_000)),
            ServerHistoryEntry(
                uri: "phantom://a2V5@example.org:443",
                lastUsedAt: Date(timeIntervalSince1970: 1_700_000_100),
                verifiedAt: Date(timeIntervalSince1970: 1_700_000_050)
            ),
        ]
        let restored = parseHistory(serializeHistory(entries))
        XCTAssertEqual(restored.count, 2)
        // Most recent first.
        XCTAssertEqual(restored[0].uri, "phantom://a2V5@example.org:443")
        XCTAssertNotNil(restored[0].verifiedAt)
        XCTAssertNil(restored[1].verifiedAt)
    }

    func testUpsertMovesToFrontAndKeepsVerification() {
        let first = "phantom://a2V5@first.example.com:443"
        let second = "phantom://a2V5@second.example.com:443"
        var entries = upsertHistory([], uri: first, usedAt: Date(timeIntervalSince1970: 100))
        entries = upsertHistory(entries, uri: second, usedAt: Date(timeIntervalSince1970: 200))
        XCTAssertEqual(entries.first?.uri, second)

        // Re-using the first entry without verifying must keep its old tick.
        entries = upsertHistory(
            upsertHistory(entries, uri: first, usedAt: Date(timeIntervalSince1970: 150),
                          verifiedAt: Date(timeIntervalSince1970: 120)),
            uri: first,
            usedAt: Date(timeIntervalSince1970: 300)
        )
        XCTAssertEqual(entries.first?.uri, first)
        XCTAssertEqual(entries.first?.verifiedAt, Date(timeIntervalSince1970: 120))
    }

    func testHistoryIsCapped() {
        var entries: [ServerHistoryEntry] = []
        for index in 0..<40 {
            entries = upsertHistory(
                entries,
                uri: "phantom://a2V5@host\(index).example.com:443",
                usedAt: Date(timeIntervalSince1970: TimeInterval(index))
            )
        }
        XCTAssertEqual(entries.count, historyMax)
        XCTAssertEqual(entries.first?.uri, "phantom://a2V5@host39.example.com:443")
    }

    func testIgnoresJunkLines() {
        XCTAssertTrue(parseHistory("not-a-uri\n\nphantom://a2V5@example.com:443\t1699999999000\t0").count == 1)
    }

    func testRelativeTime() {
        let now = Date(timeIntervalSince1970: 1_700_000_000)
        XCTAssertEqual(relativeTime(now.addingTimeInterval(-10), now: now), "刚刚")
        XCTAssertEqual(relativeTime(now.addingTimeInterval(-300), now: now), "5 分钟前")
        XCTAssertEqual(relativeTime(now.addingTimeInterval(-7200), now: now), "2 小时前")
        XCTAssertEqual(relativeTime(nil, now: now), "")
    }
}

final class TunnelProbeTests: XCTestCase {
    func testGreetingIsVersion5NoAuth() {
        XCTAssertEqual(TunnelProbe.greeting(), [0x05, 0x01, 0x00])
    }

    func testPreRoutedConnectRequestUsesInternalAddressType() {
        let request = TunnelProbe.connectRequest(host: "www.gstatic.com", port: 443, preRouted: true)
        XCTAssertEqual(Array(request[0..<3]), [0x05, 0x01, 0x00])
        XCTAssertEqual(request[3], TunnelProbe.atypPreRouted)
        // The marker is followed by the *inner* address type, then the address.
        XCTAssertEqual(request[4], 0x03)
        XCTAssertEqual(request[5], 15)
        XCTAssertEqual(String(bytes: request[6..<21], encoding: .utf8), "www.gstatic.com")
        XCTAssertEqual(Array(request[21..<23]), [0x01, 0xBB])
    }

    func testPlainConnectRequestUsesDomainAddressType() {
        let request = TunnelProbe.connectRequest(host: "example.com", port: 80, preRouted: false)
        XCTAssertEqual(request[3], 0x03)
        XCTAssertEqual(request[4], 11)
        XCTAssertEqual(Array(request[16..<18]), [0x00, 0x50])
    }

    func testConnectReplyParsing() {
        XCTAssertEqual(TunnelProbe.parseConnectReply([0x05, 0x00, 0x00, 0x01])?.ok, true)
        XCTAssertEqual(TunnelProbe.parseConnectReply([0x05, 0x05, 0x00, 0x01])?.ok, false)
        XCTAssertNil(TunnelProbe.parseConnectReply([0x05]))
        XCTAssertEqual(TunnelProbe.boundAddressLength(atyp: 0x01), 6)
        XCTAssertEqual(TunnelProbe.boundAddressLength(atyp: 0x04), 18)
        XCTAssertEqual(TunnelProbe.boundAddressLength(atyp: 0x03), -1)
    }

    func testSpeedRequestShape() {
        let request = TunnelProbe.httpGetRequest(host: "dl.google.com", path: "/file.bin")
        let text = String(bytes: request, encoding: .utf8) ?? ""
        XCTAssertTrue(text.hasPrefix("GET /file.bin HTTP/1.1\r\n"))
        XCTAssertTrue(text.contains("Host: dl.google.com\r\n"))
        XCTAssertTrue(text.hasSuffix("\r\n\r\n"))
        XCTAssertEqual(
            TunnelProbe.httpStatusLine(Array("HTTP/1.1 200 OK\r\nmore".utf8)),
            "HTTP/1.1 200 OK"
        )
    }
}
