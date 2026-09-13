package co.phantom.android

import org.junit.Assert.assertEquals
import org.junit.Test

/** Log filtering and formatting rules, as shown in the pane and the details sheet. */
class LogFormatTest {

    @Test
    fun hidesDirectTrafficByDefault() {
        val lines = listOf(
            "10:00:00 route www.google.com:443 -> Proxy (whitelist)",
            "10:00:01 route v.youku.com:443 -> Direct (final)",
        )
        assertEquals(1, visibleLogLines(lines, showDirect = false).size)
        assertEquals(2, visibleLogLines(lines, showDirect = true).size)
    }

    /** The SOCKS5/HTTP listeners spell the verdict `-> DIRECT (`; both spellings
     *  must be understood, or a system-proxy session filters nothing. */
    @Test
    fun hidesTheProxyTransportSpellingToo() {
        val lines = listOf(
            "10:00:00 INFO SOCKS5 target: v.youku.com:443 (cmd=0x1)",
            "10:00:00 INFO route v.youku.com:443 -> DIRECT (final)",
            "10:00:00 INFO Direct connection established -> v.youku.com:443",
            "10:00:01 INFO SOCKS5 target: www.google.com:443 (cmd=0x1)",
            "10:00:01 INFO route www.google.com:443 -> PROXY (whitelist)",
        )
        assertEquals(
            listOf(
                "10:00:01 INFO SOCKS5 target: www.google.com:443 (cmd=0x1)",
                "10:00:01 INFO route www.google.com:443 -> PROXY (whitelist)",
            ),
            visibleLogLines(lines, showDirect = false),
        )
        assertEquals(5, visibleLogLines(lines, showDirect = true).size)
    }

    @Test
    fun hidesHttpProxyDirectLines() {
        val lines = listOf(
            "10:00:00 INFO HTTP CONNECT → v.youku.com:443 (tunnel)",
            "10:00:00 INFO route v.youku.com:443 -> DIRECT (final)",
            "10:00:00 INFO Direct HTTP connection established → v.youku.com:443",
        )
        assertEquals(0, visibleLogLines(lines, showDirect = false).size)
    }

    /** A flow retried through the tunnel is tunnel traffic, even though its
     *  reason text mentions the failed direct attempt. */
    @Test
    fun keepsTunnelTrafficThatMentionsADirectAttempt() {
        val lines = listOf(
            "10:00:00 INFO route 142.250.0.1:443 -> Proxy (direct connect timed out; retrying)",
            "10:00:00 INFO route 1.2.3.4:443 -> Direct (dns local) 1.2.3.4",
        )
        assertEquals(
            listOf("10:00:00 INFO route 142.250.0.1:443 -> Proxy (direct connect timed out; retrying)"),
            visibleLogLines(lines, showDirect = false),
        )
    }

    @Test
    fun collapsesConsecutiveRepeats() {
        val lines = listOf(
            "10:00:00 Hello verification passed",
            "10:00:01 Hello verification passed",
            "10:00:02 Hello verification passed",
        )
        val visible = visibleLogLines(lines, showDirect = false)
        assertEquals(1, visible.size)
        assertEquals("10:00:02 Hello verification passed ×3", visible[0])
    }

    @Test
    fun keepsOnlyTheNewestLines() {
        val lines = (1..500).map { "10:00:00 line $it" }
        val visible = visibleLogLines(lines, showDirect = false, limit = 200)
        assertEquals(200, visible.size)
        assertEquals("10:00:00 line 500", visible.last())
    }

    @Test
    fun formatsRatesAndSizes() {
        assertEquals("512 B/s", formatRate(512))
        assertEquals("1.0 KB/s", formatRate(1024))
        assertEquals("1.50 MB/s", formatRate(1024 * 1024 * 3 / 2))
        assertEquals("900 B", formatSize(900))
        assertEquals("2.0 KB", formatSize(2048))
        assertEquals("3.00 GB", formatSize(3L * 1024 * 1024 * 1024))
    }

    @Test
    fun formatsUptime() {
        assertEquals("0:05", formatDuration(5))
        assertEquals("2:05", formatDuration(125))
        assertEquals("1:02:09", formatDuration(3729))
    }
}
