package co.phantom.android

import androidx.test.ext.junit.runners.AndroidJUnit4
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith

@RunWith(AndroidJUnit4::class)
class RustBridgeInstrumentedTest {

    @Test
    fun libraryLoads_andStatusIsInitiallyIdle() {
        // Loading RustBridge should succeed on devices that ship the native .so.
        val status = RustBridge.getStatus()
        assertTrue("Expected initial status to be idle (0), got $status", status == 0)
    }

    @Test
    fun getLogsReturnsEmptyInitially() {
        val result = RustBridge.getLogs(0)
        assertTrue("Expected empty initial logs", result.lines.isEmpty())
        assertEquals("Cursor should start at 0", 0L, result.cursor)
    }
}
