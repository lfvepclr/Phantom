package co.phantom.android

import androidx.test.ext.junit.runners.AndroidJUnit4
import androidx.test.platform.app.InstrumentationRegistry
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Exercises the real JNI surface on a device, so a renamed `extern "system"`
 * symbol or a mismatched signature fails here rather than in front of a user.
 *
 * The embedded-server tests really do bind a port on the phone: that is the
 * point. They always tear the server down again, and [resetServerState] makes
 * them order-independent, because the bridge keeps its server state in
 * process-wide statics that JUnit does not know about.
 */
@RunWith(AndroidJUnit4::class)
class RustBridgeInstrumentedTest {

    private fun workDir(): String =
        InstrumentationRegistry.getInstrumentation().targetContext.filesDir.absolutePath

    companion object {
        /**
         * Unprivileged, because an Android app cannot bind below 1024: the
         * core's `0` means "the default", which is 443, and that answers
         * `EACCES`. The core then probes upwards from whatever it is given, so
         * a busy port is not a failure.
         */
        private const val TEST_PORT = 8443
    }

    @Before
    fun resetServerState() {
        when (RustBridge.serverStatus()) {
            ServerStatus.RUNNING, ServerStatus.STARTING -> RustBridge.serverStop()
            // Only idle or error may start a server, so the shortest way back
            // to idle from an error is a successful start followed by a stop.
            ServerStatus.ERROR -> {
                val uri = RustBridge.serverStart(workDir(), TEST_PORT, "auto", "tcp")
                if (uri.isNotEmpty()) RustBridge.serverStop()
            }
        }
        assertEquals(
            // Carrying the reason into the message is the difference between
            // "the bridge is broken" and "the bridge says port 443 is taken":
            // without it a failure here hides the error that caused it.
            "bridge should start every test from idle " +
                "(last error: '${RustBridge.serverLastError()}')",
            ServerStatus.IDLE,
            RustBridge.serverStatus(),
        )
    }

    @Test
    fun libraryLoads_andTunnelStatusIsInitiallyIdle() {
        // Loading RustBridge runs System.loadLibrary("phantom_android"); if the
        // .so is missing or its JNI symbols were renamed this throws.
        val status = RustBridge.getStatus()
        assertTrue("Expected tunnel status to be idle (0), got $status", status == 0)
    }

    @Test
    fun getLogsReturnsEmptyInitially() {
        RustBridge.clearLogs()
        val result = RustBridge.getLogs(0)
        assertTrue("Expected empty initial logs", result.lines.isEmpty())
        assertEquals("Cursor should start at 0", 0L, result.cursor)
    }

    @Test
    fun statsJsonParsesIntoSnapshot() {
        val snapshot = RustBridge.stats()
        // The tunnel is not running, so every counter must exist and be zero —
        // a missing key would parse as zero, but a malformed payload would
        // throw inside JSONObject and fall back to ZERO for every field.
        assertEquals(0L, snapshot.up)
        assertEquals(0L, snapshot.down)
        assertEquals(0L, snapshot.conns)
    }

    @Test
    fun serversUriIsEmptyWhileServerIsStopped() {
        assertEquals("", RustBridge.serversUri())
    }

    @Test
    fun serverStartExposesUriThroughServersUri_andStopClearsIt() {
        val uri = RustBridge.serverStart(workDir(), TEST_PORT, "auto", "tcp")
        try {
            assertTrue(
                "serverStart should return a phantom:// URI, got '$uri' " +
                    "(last error: '${RustBridge.serverLastError()}')",
                uri.startsWith("phantom://"),
            )
            assertEquals(ServerStatus.RUNNING, RustBridge.serverStatus())
            // The page reads the URI back through this getter after being
            // popped and re-opened, so it must hand back the exact same string.
            assertEquals(uri, RustBridge.serversUri())
        } finally {
            RustBridge.serverStop()
        }
        assertEquals(ServerStatus.IDLE, RustBridge.serverStatus())
        assertEquals("stopping must forget the URI", "", RustBridge.serversUri())
    }

    @Test
    fun serverRejectsUnknownCipherWithAnError() {
        val uri = RustBridge.serverStart(workDir(), 0, "not-a-cipher", "tcp")
        assertEquals("an unknown cipher must not start a server", "", uri)
        assertEquals(ServerStatus.ERROR, RustBridge.serverStatus())
        assertTrue(
            "error should name the cipher, got '${RustBridge.serverLastError()}'",
            RustBridge.serverLastError().contains("unknown cipher"),
        )
    }
}
