package co.phantom.android

import org.json.JSONObject

/** Result of a log fetch from Rust. */
data class LogResult(
    /** New log lines since the requested cursor. */
    val lines: List<String>,
    /** New cursor value to pass to the next [RustBridge.getLogs] call. */
    val cursor: Long,
)

/**
 * Live counters published by the tunnel, in the same JSON shape every other
 * platform bridge uses (see `client/src/stats.rs`).
 *
 * Unknown keys are ignored on purpose: a newer core may add counters, and an
 * older UI must keep rendering rather than crash.
 */
data class TrafficSnapshot(
    val up: Long = 0,
    val down: Long = 0,
    val udpUp: Long = 0,
    val udpDown: Long = 0,
    val conns: Long = 0,
    val routeDirect: Long = 0,
    val routeProxy: Long = 0,
    val routeDirectFailed: Long = 0,
) {
    companion object {
        val ZERO = TrafficSnapshot()

        fun parse(json: String): TrafficSnapshot {
            if (json.isEmpty()) return ZERO
            return try {
                val o = JSONObject(json)
                TrafficSnapshot(
                    up = o.optLong("up"),
                    down = o.optLong("down"),
                    udpUp = o.optLong("udp_up"),
                    udpDown = o.optLong("udp_down"),
                    conns = o.optLong("conns"),
                    routeDirect = o.optLong("route_direct"),
                    routeProxy = o.optLong("route_proxy"),
                    routeDirectFailed = o.optLong("route_direct_failed"),
                )
            } catch (e: Exception) {
                ZERO
            }
        }
    }
}

/** Embedded-server status codes, mirroring the tunnel's own contract. */
object ServerStatus {
    const val IDLE = 0
    const val STARTING = 1
    const val RUNNING = 2
    const val ERROR = 3
}

/**
 * The whole JNI surface of `libphantom_android.so`.
 *
 * The library name is `phantom_android` (not `phantom_client`) because the
 * cdylib is built from `client/android/rust`, the crate that also carries the
 * embedded-server bridge; the tunnel core itself lives one layer down in
 * `phantom-client`.
 *
 * Everything except [protectFd] is an ordinary `external fun` declaring a
 * `Java_co_phantom_android_RustBridge_*` symbol on the Rust side. There is no
 * per-packet JNI: the TUN fd is handed over once and the datapath stays in Rust.
 *
 * The `@JvmStatic` on every declaration is load-bearing, not stylistic: this is
 * a Kotlin `object`, so without it each `external fun` compiles to an *instance*
 * native method and the JVM hands the Rust side the singleton (`INSTANCE`) as
 * its second argument instead of the class. That argument is inside a global
 * reference, and `VpnService.protect()` is reached later through
 * `GetStaticMethodID` on it — which aborts the process with
 * "JNI DETECTED ERROR: jclass has wrong type". Making the methods static keeps
 * the JNI calling convention the `Java_..._RustBridge_*` names already assume.
 */
object RustBridge {
    init {
        System.loadLibrary("phantom_android")
    }

    /** Start tunnel using a phantom:// URI string. Returns 0 on accepted request. */
    @JvmStatic
    external fun startTunnelWithURI(fd: Int, uri: String, mode: String): Int

    /** Legacy: start tunnel with TOML config. */
    @JvmStatic
    external fun startTunnel(fd: Int, config: String): Int

    /** Stop the tunnel. */
    @JvmStatic
    external fun stopTunnel(): Int

    /** Tunnel lifecycle status: 0 idle, 1 starting, 2 running, 3 error. */
    @JvmStatic
    external fun getStatus(): Int

    /** Human-readable last error, or empty string if none. */
    @JvmStatic
    external fun getLastError(): String

    /** Drop every buffered log line, in memory and in the mirrored file. */
    @JvmStatic
    external fun clearLogs(): Int

    /** Live traffic counters as JSON. */
    @JvmStatic
    external fun getStatsJson(): String

    /** Invalidate flows bound to the previous network; returns the new epoch. */
    @JvmStatic
    external fun notifyNetworkChange(): Long

    /** Mirror the session log to `path`; `null` disables the mirror. */
    @JvmStatic
    external fun setLogPath(path: String?): Int

    /** Enable (`path`) or disable (`null`) the opt-in TUN trace. */
    @JvmStatic
    external fun setTracePath(path: String?): Int

    /**
     * Replace the user "分流白名单" rules, one `kind:value` per line.
     * Call before [startTunnelWithURI]; changes take effect on the next start.
     */
    @JvmStatic
    external fun setUserRules(text: String?): Int

    /** Start the embedded server; returns the `phantom://` URI or "" on error. */
    @JvmStatic
    external fun serverStart(workDir: String, port: Int, cipher: String, proto: String): String

    /** Stop the embedded server: 0 stopped, 1 was not running. */
    @JvmStatic
    external fun serverStop(): Int

    /** Embedded server status: 0 idle, 1 starting, 2 running, 3 error. */
    @JvmStatic
    external fun serverStatus(): Int

    /** Last embedded-server error, or "". */
    @JvmStatic
    external fun serverLastError(): String

    /**
     * The `phantom://` URI the embedded server is serving right now, or "" when
     * it is stopped. Lets the server page recover the code after being popped
     * and re-opened while the server kept running.
     */
    @JvmStatic
    external fun serversUri(): String

    /**
     * Native helper that returns "<cursor>\n<line1>\n<line2>...".
     * Use the typed wrapper [getLogs] from Kotlin.
     */
    @JvmStatic
    private external fun getLogsNative(sinceCursor: Long): String

    /**
     * The running service, kept here because `VpnService.protect()` is an
     * instance method while the sockets that need it are opened later, from a
     * Rust worker thread.
     */
    @Volatile
    private var service: PhantomVpnService? = null

    internal fun attachService(instance: PhantomVpnService?) {
        service = instance
    }

    /**
     * Called from Rust (`platform::android::protect_fd`) to route a socket
     * around the tunnel. Returns false when no service is attached, which the
     * caller treats as "this socket will be captured by the TUN".
     */
    @JvmStatic
    fun protectFd(fd: Int): Boolean = service?.protectSocket(fd) ?: false

    /**
     * Fetch new log lines since [sinceCursor].
     * Returns a [LogResult] containing the parsed lines and the new cursor.
     */
    fun getLogs(sinceCursor: Long): LogResult {
        val raw = getLogsNative(sinceCursor)
        val parts = raw.split("\n", limit = 2)
        val cursor = parts[0].toLongOrNull() ?: sinceCursor
        val lines = if (parts.size > 1) {
            parts[1].split("\n").filter { it.isNotBlank() }
        } else {
            emptyList()
        }
        return LogResult(lines, cursor)
    }

    fun stats(): TrafficSnapshot = TrafficSnapshot.parse(getStatsJson())
}
