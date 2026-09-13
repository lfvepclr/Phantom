package co.phantom.android

import java.io.InputStream
import java.net.InetSocketAddress
import java.net.Socket

/*
 * In-app probes: how long the tunnel takes to reach a destination, and how
 * much it can carry.
 *
 * Both talk to the client's own SOCKS5 ingress on loopback, so the numbers
 * include everything a browser pays (local ingress, the encrypted link to the
 * server, the server's outbound connect) rather than just the local hop.
 *
 * This mirrors `client/harmony/entry/src/main/ets/common/TunnelProbe.ets`,
 * including the wire format of the pre-routed CONNECT request.
 */

/** SOCKS5 ingress of the running client (`client.listen`). */
const val PROXY_HOST: String = "127.0.0.1"
const val PROXY_PORT: Int = 11080

/** Phantom-internal ATYP: "routing already decided, use the tunnel". */
private const val ATYP_PREROUTED = 0x80
private const val ATYP_DOMAIN = 0x03

/** Latency probe target: whitelisted, and only the TCP connect is timed. */
const val LATENCY_HOST: String = "www.gstatic.com"
const val LATENCY_PORT: Int = 443
private const val LATENCY_TIMEOUT_MS = 8000

/**
 * Throughput probe: a large plain-HTTP file on a whitelisted host. The test
 * samples for a fixed window and hangs up, so the download never completes.
 */
const val SPEEDTEST_HOST: String = "dl.google.com"
const val SPEEDTEST_PORT: Int = 80
const val SPEEDTEST_PATH: String =
    "/dl/android/studio/install/3.0.0.18/android-studio-ide-171.4408382-windows.exe"
private const val SPEEDTEST_WINDOW_MS = 5000
private const val SPEEDTEST_MAX_BYTES = 14_000_000

sealed interface ProbeResult {
    data class Latency(val ok: Boolean, val milliseconds: Long, val error: String = "") : ProbeResult

    data class Speed(
        val ok: Boolean,
        val bytes: Long,
        val elapsedMs: Long,
        val statusLine: String = "",
        val error: String = "",
    ) : ProbeResult {
        val bytesPerSecond: Long
            get() = if (elapsedMs > 0) bytes * 1000 / elapsedMs else 0
    }
}

/**
 * Round-trip cost of reaching a destination through the tunnel.
 *
 * A failure is reported rather than thrown: "测延迟" is a diagnostic button,
 * and an exception would just surface as a crash dialog.
 */
fun probeLatency(
    host: String = LATENCY_HOST,
    port: Int = LATENCY_PORT,
    timeoutMs: Int = LATENCY_TIMEOUT_MS,
): ProbeResult.Latency {
    val started = System.currentTimeMillis()
    return try {
        socks5Connect(host, port, timeoutMs).close()
        ProbeResult.Latency(true, System.currentTimeMillis() - started)
    } catch (e: Exception) {
        ProbeResult.Latency(false, 0, e.message ?: e.javaClass.simpleName)
    }
}

/**
 * Download [path] from [host] through the tunnel for a fixed window.
 *
 * Bounded by time as well as size, so the result is a rate rather than a
 * total, and the socket is closed the moment the window ends — which also
 * makes the test safe on a metered connection.
 */
fun runSpeedTest(
    host: String = SPEEDTEST_HOST,
    port: Int = SPEEDTEST_PORT,
    path: String = SPEEDTEST_PATH,
    windowMs: Int = SPEEDTEST_WINDOW_MS,
): ProbeResult.Speed {
    var socket: Socket? = null
    return try {
        socket = socks5Connect(host, port, 10_000)
        val request = buildString {
            append("GET ").append(path).append(" HTTP/1.1\r\n")
            append("Host: ").append(host).append("\r\n")
            append("User-Agent: Phantom\r\n")
            append("Accept: */*\r\n")
            append("Connection: close\r\n\r\n")
        }
        val out = socket.getOutputStream()
        out.write(request.toByteArray(Charsets.ISO_8859_1))
        out.flush()

        val input: InputStream = socket.getInputStream()
        val deadline = System.currentTimeMillis() + windowMs
        val buffer = ByteArray(64 * 1024)
        val header = StringBuilder()
        var bodyBytes = 0L
        var headersDone = false
        socket.soTimeout = 2000

        while (System.currentTimeMillis() < deadline && bodyBytes < SPEEDTEST_MAX_BYTES) {
            val read = try {
                input.read(buffer)
            } catch (e: java.net.SocketTimeoutException) {
                // A stalled origin inside the window is a result, not an error:
                // report what actually arrived.
                -1
            }
            if (read <= 0) break
            if (headersDone) {
                bodyBytes += read
                continue
            }
            header.append(String(buffer, 0, read, Charsets.ISO_8859_1))
            val marker = header.indexOf("\r\n\r\n")
            if (marker >= 0) {
                headersDone = true
                bodyBytes += header.length - (marker + 4)
            }
        }

        val elapsed = windowMs.toLong().coerceAtMost(
            (System.currentTimeMillis() - (deadline - windowMs)).coerceAtLeast(1)
        )
        val statusLine = header.lineSequence().firstOrNull()
            ?.takeIf { it.startsWith("HTTP/") } ?: ""
        ProbeResult.Speed(true, bodyBytes, elapsed, statusLine)
    } catch (e: Exception) {
        ProbeResult.Speed(false, 0, 0, error = e.message ?: e.javaClass.simpleName)
    } finally {
        try {
            socket?.close()
        } catch (_: Exception) {
        }
    }
}

/**
 * Open a tunnelled TCP stream through the local SOCKS5 ingress.
 *
 * The request carries the pre-routed marker (`ATYP 0x80` followed by a domain
 * ATYP), which tells the ingress "this destination is already routed — send it
 * through the tunnel", so the probe measures the tunnel even for a host the
 * whitelist would not have selected on its own.
 */
private fun socks5Connect(host: String, port: Int, timeoutMs: Int): Socket {
    val socket = Socket()
    try {
        socket.tcpNoDelay = true
        socket.connect(InetSocketAddress(PROXY_HOST, PROXY_PORT), timeoutMs)
        socket.soTimeout = timeoutMs
        val out = socket.getOutputStream()
        val input = socket.getInputStream()

        out.write(byteArrayOf(0x05, 0x01, 0x00))
        out.flush()
        val greeting = ByteArray(2)
        readFully(input, greeting)
        if (greeting[1].toInt() != 0x00) {
            throw IllegalStateException("代理不接受免认证方式")
        }

        val hostBytes = host.toByteArray(Charsets.US_ASCII)
        val request = ByteArray(6 + hostBytes.size + 2)
        var at = 0
        request[at++] = 0x05
        request[at++] = 0x01
        request[at++] = 0x00
        request[at++] = ATYP_PREROUTED.toByte()
        request[at++] = ATYP_DOMAIN.toByte()
        request[at++] = hostBytes.size.toByte()
        System.arraycopy(hostBytes, 0, request, at, hostBytes.size)
        at += hostBytes.size
        request[at++] = ((port shr 8) and 0xff).toByte()
        request[at] = (port and 0xff).toByte()
        out.write(request)
        out.flush()

        // Fixed-size reply: VER REP RSV ATYP BND.ADDR(4) BND.PORT(2).
        val reply = ByteArray(10)
        readFully(input, reply)
        if (reply[1].toInt() != 0x00) {
            throw IllegalStateException("隧道建立失败 (SOCKS5 错误码 ${reply[1]})")
        }
        return socket
    } catch (e: Exception) {
        try {
            socket.close()
        } catch (_: Exception) {
        }
        throw e
    }
}

private fun readFully(input: InputStream, target: ByteArray) {
    var read = 0
    while (read < target.size) {
        val n = input.read(target, read, target.size - read)
        if (n <= 0) throw IllegalStateException("连接被对端关闭")
        read += n
    }
}
