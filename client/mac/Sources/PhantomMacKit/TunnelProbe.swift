import Foundation
import Darwin

/// Latency probe result (a TCP handshake through the tunnel, then hung up).
public struct LatencyResult: Sendable {
    public var ok: Bool
    public var milliseconds: Int
    public var error: String
}

/// Throughput probe result (bytes actually received through the tunnel).
public struct SpeedResult: Sendable {
    public var ok: Bool
    public var bytes: UInt64
    public var elapsed: TimeInterval
    public var bytesPerSecond: Double
    /// HTTP status line as returned by the origin, for sanity checking.
    public var status: String
    public var error: String
}

public enum ProbeError: Error, CustomStringConvertible {
    case socket(String)
    case connect(String)
    case timeout
    case closed
    case socks(String)

    public var description: String {
        switch self {
        case .socket(let detail): return "创建 socket 失败：\(detail)"
        case .connect(let detail): return "连接本地 SOCKS5 失败：\(detail)"
        case .timeout: return "超时"
        case .closed: return "连接被关闭"
        case .socks(let detail): return detail
        }
    }
}

/// Latency / throughput probes that run *through the live tunnel*.
///
/// They talk to the client's own SOCKS5 ingress on loopback, which is exactly
/// the datapath a browser uses, so the numbers describe the real connection
/// rather than a side-channel test. The request uses Phantom's internal
/// `ATYP_PREROUTED` marker (`0x80`) so a probe is pinned to the tunnel even
/// when the host is not on the分流白名单 — otherwise "测速" would measure the
/// operator's home broadband, not the server.
///
/// The handshake is written by hand because the numbers are the point: only the
/// SOCKS5 CONNECT round trip is timed, with no TLS and no library overhead.
/// Mirrors `client/harmony/entry/src/main/ets/common/TunnelProbe.ets`.
public enum TunnelProbe {
    // MARK: - Targets

    /// TLS-free by design: only the TCP connect is timed.
    public static let latencyHost = "www.gstatic.com"
    public static let latencyPort: UInt16 = 443
    public static let latencyTimeout: TimeInterval = 8

    /// A large, plain-HTTP file; the probe samples for a fixed window and hangs
    /// up, so the download never completes.
    public static let speedHost = "dl.google.com"
    public static let speedPort: UInt16 = 80
    public static let speedPath =
        "/dl/android/studio/install/3.0.0.18/android-studio-ide-171.4408382-windows.exe"
    public static let speedWindow: TimeInterval = 5
    public static let speedMaxBytes = 14_000_000

    /// Phantom-internal ATYP: "routing already decided, use the tunnel".
    public static let atypPreRouted: UInt8 = 0x80
    private static let atypDomain: UInt8 = 0x03

    // MARK: - Wire format (pure, unit-tested)

    /// SOCKS5 greeting: version 5, one method, "no authentication".
    public static func greeting() -> [UInt8] {
        [0x05, 0x01, 0x00]
    }

    /// SOCKS5 CONNECT request. `preRouted` selects Phantom's internal address
    /// type, which skips the client-side分流 decision.
    ///
    /// Pre-routed framing is `[VER, CMD, RSV, 0x80, <inner ATYP>, …]`: the
    /// marker is followed by a *second* address-type byte describing the real
    /// address. Emitting `0x80` and then jumping straight to the length byte
    /// makes the client read a bogus address type and reset the connection.
    public static func connectRequest(host: String, port: UInt16, preRouted: Bool = true) -> [UInt8] {
        var bytes: [UInt8] = [0x05, 0x01, 0x00]
        if preRouted {
            bytes.append(atypPreRouted)
        }
        bytes.append(atypDomain)
        let hostBytes = Array(host.utf8)
        bytes.append(UInt8(truncatingIfNeeded: hostBytes.count))
        bytes.append(contentsOf: hostBytes)
        bytes.append(UInt8(port >> 8))
        bytes.append(UInt8(port & 0xff))
        return bytes
    }

    /// Parse the SOCKS5 reply header (4 bytes). `nil` when more data is needed.
    public static func parseConnectReply(_ bytes: [UInt8]) -> (ok: Bool, reply: UInt8)? {
        guard bytes.count >= 4, bytes[0] == 0x05 else { return nil }
        return (bytes[1] == 0x00, bytes[1])
    }

    /// Bytes of the bound address that follow the 4-byte reply header.
    public static func boundAddressLength(atyp: UInt8) -> Int {
        switch atyp {
        case 0x01: return 4 + 2      // IPv4 + port
        case 0x04: return 16 + 2     // IPv6 + port
        case 0x03: return -1         // domain: 1 length byte + name; caller reads the length
        default: return 0
        }
    }

    /// Minimal HTTP/1.1 GET; `Connection: close` keeps the origin from keeping
    /// the socket open after the window ends.
    public static func httpGetRequest(host: String, path: String) -> [UInt8] {
        let request = "GET \(path) HTTP/1.1\r\n"
            + "Host: \(host)\r\n"
            + "User-Agent: Phantom/1.0\r\n"
            + "Accept: */*\r\n"
            + "Connection: close\r\n\r\n"
        return Array(request.utf8)
    }

    /// First line of an HTTP response, e.g. `HTTP/1.1 200 OK`.
    public static func httpStatusLine(_ bytes: [UInt8]) -> String? {
        guard let text = String(bytes: bytes, encoding: .utf8) else { return nil }
        return text.split(separator: "\r\n", maxSplits: 1, omittingEmptySubsequences: false)
            .first.map(String.init)
    }

    // MARK: - Probes

    /// Measure the round trip of "open a TCP connection through the tunnel".
    public static func measureLatency(
        proxyHost: String = "127.0.0.1",
        proxyPort: UInt16,
        host: String = TunnelProbe.latencyHost,
        port: UInt16 = TunnelProbe.latencyPort,
        timeout: TimeInterval = TunnelProbe.latencyTimeout
    ) -> LatencyResult {
        let started = Date()
        do {
            let socket = try openTunnelSocket(
                proxyHost: proxyHost, proxyPort: proxyPort,
                targetHost: host, targetPort: port, timeout: timeout
            )
            close(socket)
            let ms = Int(Date().timeIntervalSince(started) * 1000)
            return LatencyResult(ok: true, milliseconds: ms, error: "")
        } catch {
            return LatencyResult(ok: false, milliseconds: 0, error: "\(error)")
        }
    }

    /// Stream a fixed window of bytes through the tunnel and report the rate.
    public static func measureSpeed(
        proxyHost: String = "127.0.0.1",
        proxyPort: UInt16,
        host: String = TunnelProbe.speedHost,
        port: UInt16 = TunnelProbe.speedPort,
        path: String = TunnelProbe.speedPath,
        window: TimeInterval = TunnelProbe.speedWindow,
        maxBytes: Int = TunnelProbe.speedMaxBytes
    ) -> SpeedResult {
        var result = SpeedResult(ok: false, bytes: 0, elapsed: 0, bytesPerSecond: 0, status: "", error: "")
        do {
            let socket = try openTunnelSocket(
                proxyHost: proxyHost, proxyPort: proxyPort,
                targetHost: host, targetPort: port, timeout: TunnelProbe.latencyTimeout
            )
            defer { close(socket) }
            try send(socket, bytes: httpGetRequest(host: host, path: path), timeout: window)

            let started = Date()
            let deadline = started.addingTimeInterval(window)
            var received: UInt64 = 0
            var header = [UInt8]()
            var buffer = [UInt8](repeating: 0, count: 65536)
            var failure: String?
            while received < UInt64(maxBytes) {
                let remaining = deadline.timeIntervalSinceNow
                if remaining <= 0 { break }
                let read: Int
                do {
                    read = try receive(socket, into: &buffer, timeout: remaining)
                } catch ProbeError.timeout {
                    // The sampling window simply ran out mid-read. Whatever was
                    // already received is the measurement — treating this as a
                    // failure would throw away a perfectly good sample (and
                    // report "超时" for a download that clearly worked).
                    break
                } catch {
                    failure = "\(error)"
                    break
                }
                if read == 0 { break }
                if header.count < 128 {
                    header.append(contentsOf: buffer[0..<min(read, 128 - header.count)])
                }
                received += UInt64(read)
            }
            let elapsed = Date().timeIntervalSince(started)
            result.bytes = received
            result.elapsed = elapsed
            result.status = httpStatusLine(header) ?? ""
            guard received > 0 else {
                result.error = failure ?? "未收到数据（服务端未开始传输？）"
                return result
            }
            result.bytesPerSecond = elapsed > 0 ? Double(received) / elapsed : 0
            result.ok = true
            return result
        } catch {
            result.error = "\(error)"
            return result
        }
    }

    // MARK: - Socket plumbing

    /// Connect to the loopback SOCKS5 listener and complete the handshake with
    /// the internal pre-routed address type.
    public static func openTunnelSocket(
        proxyHost: String,
        proxyPort: UInt16,
        targetHost: String,
        targetPort: UInt16,
        timeout: TimeInterval
    ) throws -> Int32 {
        let socket = try connectTCP(host: proxyHost, port: proxyPort, timeout: timeout)
        do {
            try send(socket, bytes: greeting(), timeout: timeout)
            let reply = try receive(exactly: 2, from: socket, timeout: timeout)
            guard reply.count == 2, reply[0] == 0x05, reply[1] == 0x00 else {
                throw ProbeError.socks("SOCKS5 握手被拒绝（客户端未就绪？）")
            }
            try send(
                socket,
                bytes: connectRequest(host: targetHost, port: targetPort, preRouted: true),
                timeout: timeout
            )
            let head = try receive(exactly: 4, from: socket, timeout: timeout)
            guard let parsed = parseConnectReply(head), parsed.ok else {
                let code = head.count > 1 ? head[1] : 0xff
                throw ProbeError.socks("SOCKS5 CONNECT 失败（reply=\(code)）")
            }
            // Drain the bound address so the stream is positioned at payload.
            let boundLength = boundAddressLength(atyp: head[3])
            if boundLength > 0 {
                _ = try receive(exactly: boundLength, from: socket, timeout: timeout)
            } else if boundLength == -1 {
                let lengthByte = try receive(exactly: 1, from: socket, timeout: timeout)
                _ = try receive(exactly: Int(lengthByte[0]) + 2, from: socket, timeout: timeout)
            }
            return socket
        } catch {
            close(socket)
            throw error
        }
    }

    private static func connectTCP(host: String, port: UInt16, timeout: TimeInterval) throws -> Int32 {
        var hints = addrinfo()
        hints.ai_family = AF_UNSPEC
        hints.ai_socktype = SOCK_STREAM
        var info: UnsafeMutablePointer<addrinfo>?
        let status = getaddrinfo(host, String(port), &hints, &info)
        guard status == 0, let first = info else {
            throw ProbeError.socket("无法解析 \(host)（getaddrinfo=\(status)）")
        }
        defer { freeaddrinfo(info) }

        var lastError = "无可用地址"
        var candidate: UnsafeMutablePointer<addrinfo>? = first
        while let entry = candidate {
            let socket = Darwin.socket(entry.pointee.ai_family, entry.pointee.ai_socktype, entry.pointee.ai_protocol)
            if socket >= 0 {
                // Non-blocking connect + poll gives us a real timeout; a
                // blocking connect() to a dead listener can hang for minutes.
                let flags = fcntl(socket, F_GETFL, 0)
                _ = fcntl(socket, F_SETFL, flags | O_NONBLOCK)
                var noDelay: Int32 = 1
                setsockopt(socket, IPPROTO_TCP, TCP_NODELAY, &noDelay, socklen_t(MemoryLayout<Int32>.size))

                let result = Darwin.connect(socket, entry.pointee.ai_addr, entry.pointee.ai_addrlen)
                if result == 0 {
                    _ = fcntl(socket, F_SETFL, flags)
                    return socket
                }
                if errno == EINPROGRESS,
                   pollWritable(socket: socket, timeout: timeout) {
                    var error: Int32 = 0
                    var length = socklen_t(MemoryLayout<Int32>.size)
                    getsockopt(socket, SOL_SOCKET, SO_ERROR, &error, &length)
                    if error == 0 {
                        _ = fcntl(socket, F_SETFL, flags)
                        return socket
                    }
                    lastError = String(cString: strerror(error))
                } else {
                    lastError = "超时"
                }
                close(socket)
            } else {
                lastError = String(cString: strerror(errno))
            }
            candidate = entry.pointee.ai_next
        }
        throw ProbeError.connect(lastError)
    }

    private static func pollWritable(socket: Int32, timeout: TimeInterval) -> Bool {
        var descriptor = pollfd(fd: socket, events: Int16(POLLOUT), revents: 0)
        let milliseconds = Int32(max(1, timeout * 1000))
        while true {
            let ready = poll(&descriptor, 1, milliseconds)
            if ready > 0 { return true }
            if ready == 0 { return false }
            if errno != EINTR { return false }
        }
    }

    private static func send(_ socket: Int32, bytes: [UInt8], timeout: TimeInterval) throws {
        var offset = 0
        while offset < bytes.count {
            var descriptor = pollfd(fd: socket, events: Int16(POLLOUT), revents: 0)
            let ready = poll(&descriptor, 1, Int32(max(1, timeout * 1000)))
            if ready == 0 { throw ProbeError.timeout }
            if ready < 0 && errno != EINTR { throw ProbeError.socket(String(cString: strerror(errno))) }
            if ready < 0 { continue }
            let written = bytes[offset...].withUnsafeBytes { raw in
                Darwin.send(socket, raw.baseAddress, raw.count, 0)
            }
            if written > 0 {
                offset += written
            } else if written < 0 && errno != EINTR && errno != EAGAIN {
                throw ProbeError.socket(String(cString: strerror(errno)))
            }
        }
    }

    private static func receive(_ socket: Int32, into buffer: inout [UInt8], timeout: TimeInterval) throws -> Int {
        var descriptor = pollfd(fd: socket, events: Int16(POLLIN), revents: 0)
        let milliseconds = Int32(max(1, timeout * 1000))
        while true {
            let ready = poll(&descriptor, 1, milliseconds)
            if ready == 0 { throw ProbeError.timeout }
            if ready < 0 {
                if errno == EINTR { continue }
                throw ProbeError.socket(String(cString: strerror(errno)))
            }
            let read = buffer.withUnsafeMutableBytes { raw in
                Darwin.recv(socket, raw.baseAddress, raw.count, 0)
            }
            if read < 0 {
                if errno == EINTR || errno == EAGAIN { continue }
                throw ProbeError.socket(String(cString: strerror(errno)))
            }
            return read
        }
    }

    private static func receive(exactly count: Int, from socket: Int32, timeout: TimeInterval) throws -> [UInt8] {
        var collected = [UInt8]()
        var buffer = [UInt8](repeating: 0, count: max(count, 1))
        let deadline = Date().addingTimeInterval(timeout)
        while collected.count < count {
            let remaining = deadline.timeIntervalSinceNow
            if remaining <= 0 { throw ProbeError.timeout }
            let read = try receive(socket, into: &buffer, timeout: remaining)
            if read == 0 { throw ProbeError.closed }
            collected.append(contentsOf: buffer[0..<min(read, count - collected.count)])
        }
        return collected
    }
}
