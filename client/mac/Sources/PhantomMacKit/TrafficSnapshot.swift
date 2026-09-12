import Foundation

/// Live counters of the running tunnel, as published by the Rust bridge.
///
/// Same JSON document the Android/HarmonyOS bridges use, so the three clients
/// can never drift apart on counter names or units.
public struct TrafficSnapshot: Equatable, Sendable {
    public var tcpUp: UInt64 = 0
    public var tcpDown: UInt64 = 0
    public var udpUp: UInt64 = 0
    public var udpDown: UInt64 = 0
    public var connections: UInt64 = 0
    public var routeDirect: UInt64 = 0
    public var routeProxy: UInt64 = 0

    public init() {}

    /// Parse `{"up":…,"down":…,"udp_up":…,"udp_down":…,"conns":…,
    /// "route_direct":…,"route_proxy":…}`. Unknown/missing keys stay zero so an
    /// older bridge can never crash the UI.
    public init(json: String) {
        guard let data = json.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else {
            return
        }
        func value(_ key: String) -> UInt64 {
            if let number = object[key] as? NSNumber { return number.uint64Value }
            if let text = object[key] as? String { return UInt64(text) ?? 0 }
            return 0
        }
        tcpUp = value("up")
        tcpDown = value("down")
        udpUp = value("udp_up")
        udpDown = value("udp_down")
        connections = value("conns")
        routeDirect = value("route_direct")
        routeProxy = value("route_proxy")
    }

    public var totalUp: UInt64 { tcpUp &+ udpUp }
    public var totalDown: UInt64 { tcpDown &+ udpDown }
}

/// Bytes/second plus cumulative totals, derived from two snapshots.
///
/// The bridge only publishes monotonic counters; rates are a UI concern, so the
/// subtraction lives here where it can be unit-tested.
public struct TrafficRates: Equatable, Sendable {
    public var downPerSecond: Double = 0
    public var upPerSecond: Double = 0
    public var totalDown: UInt64 = 0
    public var totalUp: UInt64 = 0
    public var proxiedFlows: UInt64 = 0
    public var directFlows: UInt64 = 0

    public init() {}

    /// - Parameter elapsed: seconds since `previous` was sampled (0 → no rate).
    public init(previous: TrafficSnapshot, current: TrafficSnapshot, elapsed: TimeInterval) {
        totalDown = current.totalDown
        totalUp = current.totalUp
        proxiedFlows = current.routeProxy
        directFlows = current.routeDirect
        guard elapsed > 0 else { return }
        // Counters are monotonic; a restart resets them, so never report a
        // negative rate from a wrap-around.
        let downDelta = current.totalDown >= previous.totalDown
            ? current.totalDown - previous.totalDown : 0
        let upDelta = current.totalUp >= previous.totalUp
            ? current.totalUp - previous.totalUp : 0
        downPerSecond = Double(downDelta) / elapsed
        upPerSecond = Double(upDelta) / elapsed
    }
}
