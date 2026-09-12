import XCTest
@testable import PhantomMacKit

/// Live probe against a *running* client's SOCKS5 ingress.
///
/// Skipped unless `PHANTOM_PROBE_PORT` is set, e.g.
/// `PHANTOM_PROBE_PORT=11080 xcrun swift test --filter TunnelProbeLiveTests`
/// while Phantom is connected. This is the only way to prove the probe really
/// travels the tunnel (and that the pre-routed handshake is accepted) rather
/// than merely encoding the right bytes.
final class TunnelProbeLiveTests: XCTestCase {
    func testLatencyAndSpeedThroughRunningClient() throws {
        guard let raw = ProcessInfo.processInfo.environment["PHANTOM_PROBE_PORT"],
              let port = UInt16(raw), port > 0 else {
            throw XCTSkip("设置 PHANTOM_PROBE_PORT=<本机 SOCKS5 端口> 时对真实隧道做一次探测")
        }

        let latency = TunnelProbe.measureLatency(proxyPort: port)
        XCTAssertTrue(latency.ok, "链路延迟探测失败：\(latency.error)")
        XCTAssertGreaterThan(latency.milliseconds, 0)

        // 3s window: a couple of MB at this server's 3 Mbps uplink, so the probe
        // never meaningfully eats the link it is measuring.
        let speed = TunnelProbe.measureSpeed(proxyPort: port, window: 3)
        XCTAssertTrue(speed.ok, "测速探测失败：\(speed.error)")
        XCTAssertGreaterThan(speed.bytes, 0)

        print(
            "PROBE latency=\(latency.milliseconds) ms "
                + "status=\"\(speed.status)\" bytes=\(speed.bytes) "
                + "rate=\(formatRate(speed.bytesPerSecond)) elapsed=\(String(format: "%.2f", speed.elapsed))s"
        )
    }
}
