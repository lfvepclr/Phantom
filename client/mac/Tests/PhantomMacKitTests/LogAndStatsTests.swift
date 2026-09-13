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

    /// The pane is ~50 monospace columns wide, so the severity token is carried
    /// out-of-band instead of eating five of them on every line.
    func testLevelIsSplitOutOfTheRenderedText() {
        XCTAssertEqual(splitLevel("INFO route a:443 -> Proxy (whitelist)").level, .info)
        XCTAssertEqual(splitLevel("WARN system proxy not set").level, .warn)
        XCTAssertEqual(splitLevel("ERROR tunnel exited").level, .error)
        // A message that merely starts with a word keeps its text intact.
        XCTAssertEqual(splitLevel("Informational note").text, "Informational note")
        XCTAssertEqual(splitLevel("Informational note").level, .other)

        let rendered = renderLogLines(lines, filter: .tunnel)
        XCTAssertEqual(rendered.first?.level, .info)
        XCTAssertFalse(rendered.contains { $0.text.contains("INFO ") })
        XCTAssertTrue(rendered.first?.text.hasSuffix("-> Proxy (whitelist)") == true)
    }

    func testPaddingSpacesAreSqueezed() {
        XCTAssertEqual(collapseSpaces("INFO  route a:443"), "INFO route a:443")
        XCTAssertEqual(
            renderLogLines(["17:26:44 INFO  route a:443 -> Proxy (whitelist)"], filter: .all)
                .first?.text,
            "17:26:44 route a:443 -> Proxy (whitelist)"
        )
    }

    func testTunnelFilterHidesDirectFlows() {
        let tunnel = renderLogLines(lines, filter: .tunnel)
        XCTAssertFalse(tunnel.contains { $0.text.contains("youku") })
        XCTAssertTrue(tunnel.contains { $0.text.contains("google") })

        let all = renderLogLines(lines, filter: .all)
        XCTAssertTrue(all.contains { $0.text.contains("youku") })
    }

    /// macOS points the system proxy at our SOCKS5/HTTP listener, so the lines
    /// it sees are the ones those transports emit — not the TUN ones. They used
    /// to spell the verdict `-> DIRECT (`, which the pane did not match, so
    /// 「仅隧道」 still showed every direct flow.
    func testTunnelFilterUnderstandsTheProxyTransportSpelling() {
        let socks = [
            "17:26:44 INFO SOCKS5 target: v.youku.com:443 (cmd=0x1)",
            "17:26:44 INFO route v.youku.com:443 -> DIRECT (final)",
            "17:26:44 INFO Direct connection established -> v.youku.com:443",
            "17:26:45 INFO SOCKS5 target: www.google.com:443 (cmd=0x1)",
            "17:26:45 INFO route www.google.com:443 -> PROXY (whitelist)",
            "17:26:45 INFO Relay done ↑ www.google.com:443 (1200 bytes up)",
        ]
        let tunnel = renderLogLines(socks, filter: .tunnel)
        XCTAssertEqual(tunnel.map(\.text), [
            "17:26:45 SOCKS5 target: www.google.com:443 (cmd=0x1)",
            "17:26:45 route www.google.com:443 -> PROXY (whitelist)",
            "17:26:45 Relay done ↑ www.google.com:443 (1200 bytes up)",
        ])
        // Nothing is hidden from the full view — the disk log is the record of
        // everything; this is only about what the narrow pane shows.
        XCTAssertEqual(renderLogLines(socks, filter: .all).count, socks.count)
    }

    func testTunnelFilterHidesTheHttpProxyDirectLines() {
        let http = [
            "17:26:44 INFO HTTP CONNECT → v.youku.com:443 (tunnel)",
            "17:26:44 INFO route v.youku.com:443 -> DIRECT (final)",
            "17:26:44 INFO Direct HTTP connection established → v.youku.com:443",
        ]
        XCTAssertTrue(renderLogLines(http, filter: .tunnel).isEmpty)
    }

    /// A flow that failed direct and was retried through the tunnel is tunnel
    /// traffic; its reason text mentions "direct" and must not be filtered out.
    func testDirectFallbackRetriedThroughTheTunnelStaysVisible() {
        let lines = [
            "17:26:44 INFO route 142.250.0.1:443 -> Proxy (direct connect timed out; retrying through the tunnel)",
            "17:26:44 INFO route 1.2.3.4:443 -> Direct (dns local) 1.2.3.4",
        ]
        let tunnel = renderLogLines(lines, filter: .tunnel)
        XCTAssertEqual(tunnel.map(\.text), [
            "17:26:44 route 142.250.0.1:443 -> Proxy (direct connect timed out; retrying through the tunnel)"
        ])
    }

    func testRouteClassificationIsCaseInsensitiveAndBannerAware() {
        XCTAssertTrue(LogRoute.isDirect("route a:443 -> Direct (final)"))
        XCTAssertTrue(LogRoute.isDirect("route a:443 -> DIRECT (final)"))
        XCTAssertFalse(LogRoute.isDirect("route a:443 -> Proxy (whitelist)"))
        XCTAssertFalse(LogRoute.isDirect("route a:443 -> Proxy (direct unreachable earlier on this network)"))
        XCTAssertEqual(LogRoute.directTarget("route a:443 -> Direct (final)"), "a:443")
        XCTAssertNil(LogRoute.directTarget("route a:443 -> Proxy (whitelist)"))
        XCTAssertEqual(LogRoute.bannerTarget("SOCKS5 target: a:443 (cmd=0x1)"), "a:443")
        XCTAssertEqual(LogRoute.bannerTarget("HTTP CONNECT → a:443 (tunnel)"), "a:443")
        XCTAssertNil(LogRoute.bannerTarget("route a:443 -> Direct (final)"))
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
