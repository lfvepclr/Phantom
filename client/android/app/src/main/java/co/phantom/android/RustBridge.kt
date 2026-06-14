package co.phantom.android

/** Result of a log fetch from Rust. */
data class LogResult(
    /** New log lines since the requested cursor. */
    val lines: List<String>,
    /** New cursor value to pass to the next [getLogs] call. */
    val cursor: Long
)

object RustBridge {
    init {
        System.loadLibrary("phantom_client")
    }

    /** Start tunnel using a phantom:// URI string. Returns 0 on accepted request. */
    external fun startTunnelWithURI(fd: Int, uri: String, mode: String): Int

    /** Legacy: start tunnel with TOML config. */
    external fun startTunnel(fd: Int, config: String): Int

    /** Stop the tunnel. */
    external fun stopTunnel(): Int

    /** Tunnel lifecycle status: 0 idle, 1 starting, 2 running, 3 error. */
    external fun getStatus(): Int

    /** Human-readable last error, or empty string if none. */
    external fun getLastError(): String

    /**
     * Native helper that returns "<cursor>\n<line1>\n<line2>...".
     * Use the typed wrapper [getLogs] from Kotlin.
     */
    private external fun getLogsNative(sinceCursor: Long): String

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
}
