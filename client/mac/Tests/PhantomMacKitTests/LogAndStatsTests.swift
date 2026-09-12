import XCTest
@testable import PhantomMacKit

final class LogBufferTests: XCTestCase {
    private let lines = [
        "17:26:44 INFO route www.google.com:443 -> Proxy (whitelist)",
        "17:26:45 INFO route v.youku.com:443 -> Direct (final)",
        "17:26:46 INFO route www.youtube.com:443 -> Proxy (whitelist)",
        "no-timestamp line",
        "17:26:47 INFO route www.youtube.com:443 -> Proxy (whitelist)",
        "17:26:48 INFO route www.youtube.com:443 -> Proxy (whitelist)",
    ]

    func testStampAndMessage() {
        XCTAssertEqual(logStamp(of: lines[0]), "17:26:44")
        XCTAssertEqual(logStamp(of: lines[3]), "")
        XCTAssertEqual(logMessage(of: lines[0]), "INFO route www.google.com:443 -> Proxy (whitelist)")
        XCTAssertEqual(logMessage(of: lines[3]), "no-timestamp line")
    }

    func testTunnelFilterHidesDirectFlows() {
        let tunnel = renderLogLines(lines, filter: .tunnel)
        XCTAssertFalse(tunnel.contains { $0.text.contains("youku") })
        XCTAssertTrue(tunnel.contains { $0.text.contains("google") })

        let all = renderLogLines(lines, filter: .all)
        XCTAssertTrue(all.contains { $0.text.contains("youku") })
    }

    func testConsecutiveRepeatsCollapse() {
        // Only *consecutive* repeats collapse: the same route reused later is a
        // separate event, so the timestamp of the run's last line is the one
        // shown next to `×N`.
        let rendered = renderLogLines(lines, filter: .all)
        // google, youku, youtube, no-timestamp, youtube ×2 — the run at the end
        // is the only one that collapses.
        XCTAssertEqual(rendered.count, 5)
        let youtube = rendered.last
        XCTAssertEqual(youtube?.repeats, 2)
        XCTAssertEqual(youtube?.text.hasSuffix("×2"), true)
        XCTAssertEqual(youtube?.text.hasPrefix("17:26:48"), true)
    }

    func testViewIsCappedToTheTail() {
        let many = (0..<500).map { "line \($0)" }
        let rendered = renderLogLines(many, filter: .all, limit: 200)
        XCTAssertEqual(rendered.count, 200)
        XCTAssertEqual(rendered.last?.text, "line 499")
    }
}

final class TrafficSnapshotTests: XCTestCase {
    func testParsesBridgeJson() {
        let snapshot = TrafficSnapshot(
            json: "{\"up\":10,\"down\":20,\"udp_up\":1,\"udp_down\":2,"
                + "\"conns\":3,\"route_direct\":4,\"route_proxy\":5}"
        )
        XCTAssertEqual(snapshot.tcpUp, 10)
        XCTAssertEqual(snapshot.tcpDown, 20)
        XCTAssertEqual(snapshot.totalUp, 11)
        XCTAssertEqual(snapshot.totalDown, 22)
        XCTAssertEqual(snapshot.connections, 3)
        XCTAssertEqual(snapshot.routeDirect, 4)
        XCTAssertEqual(snapshot.routeProxy, 5)
    }

    func testUnknownAndMalformedJsonStayZero() {
        XCTAssertEqual(TrafficSnapshot(json: "not json").tcpDown, 0)
        XCTAssertEqual(TrafficSnapshot(json: "{}").routeProxy, 0)
        XCTAssertEqual(TrafficSnapshot(json: "{\"down\":\"2048\"}").tcpDown, 2048)
    }

    func testRatesComeFromCounterDeltas() {
        var previous = TrafficSnapshot()
        previous.tcpDown = 1_000
        previous.tcpUp = 500
        var current = TrafficSnapshot()
        current.tcpDown = 3_000
        current.tcpUp = 1_500
        current.routeProxy = 7
        current.routeDirect = 2

        let rates = TrafficRates(previous: previous, current: current, elapsed: 2)
        XCTAssertEqual(rates.downPerSecond, 1000, accuracy: 0.01)
        XCTAssertEqual(rates.upPerSecond, 500, accuracy: 0.01)
        XCTAssertEqual(rates.totalDown, 3_000)
        XCTAssertEqual(rates.proxiedFlows, 7)
        XCTAssertEqual(rates.directFlows, 2)
    }

    func testRestartNeverReportsNegativeRates() {
        var previous = TrafficSnapshot()
        previous.tcpDown = 9_000
        let rates = TrafficRates(previous: previous, current: TrafficSnapshot(), elapsed: 1)
        XCTAssertEqual(rates.downPerSecond, 0)
    }
}

final class FormatterTests: XCTestCase {
    func testRates() {
        XCTAssertEqual(formatRate(512), "512 B/s")
        XCTAssertEqual(formatRate(2048), "2.0 KB/s")
        XCTAssertEqual(formatRate(3 * 1024 * 1024), "3.00 MB/s")
    }

    func testBytes() {
        XCTAssertEqual(formatBytes(999), "999 B")
        XCTAssertEqual(formatBytes(2048), "2.0 KB")
        XCTAssertEqual(formatBytes(5 * 1024 * 1024), "5.0 MB")
    }

    func testDuration() {
        XCTAssertEqual(formatDuration(seconds: 12), "12 秒")
        XCTAssertEqual(formatDuration(seconds: 185), "3 分 5 秒")
        XCTAssertEqual(formatDuration(seconds: 7620), "2 小时 7 分")
    }
}
