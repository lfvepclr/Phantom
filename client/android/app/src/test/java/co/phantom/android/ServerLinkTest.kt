package co.phantom.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

/**
 * The link/history rules are shared with the HarmonyOS client, so they are
 * tested as pure functions: a regression here shows up as "the phone that
 * imports the QR shows a different server" rather than as a crash.
 */
class ServerLinkTest {

    @Test
    fun parsesFullLink() {
        val link = parseServerUri(
            "phantom://c2VydmVyLWtleQ==@198.51.100.7:8443" +
                "?psk=cHNr&cipher=aes-256-gcm&proto=quic#tokyo"
        )
        assertTrue(link.valid)
        assertEquals("c2VydmVyLWtleQ==", link.key)
        assertEquals("198.51.100.7", link.host)
        assertEquals(8443, link.port)
        assertEquals("cHNr", link.psk)
        assertEquals("aes-256-gcm", link.cipher)
        assertEquals("quic", link.proto)
        assertEquals("tokyo", link.name)
    }

    @Test
    fun defaultsPortAndCipher() {
        val link = parseServerUri("phantom://a2V5@example.com")
        assertTrue(link.valid)
        assertEquals(443, link.port)
        assertEquals("auto", link.cipher)
        assertEquals("tcp", link.proto)
    }

    @Test
    fun parsesBracketedIpv6() {
        val link = parseServerUri("phantom://a2V5@[2001:db8::1]:9443")
        assertTrue(link.valid)
        assertEquals("2001:db8::1", link.host)
        assertEquals(9443, link.port)
    }

    @Test
    fun rejectsMalformedInput() {
        assertFalse(parseServerUri("https://example.com").valid)
        assertFalse(parseServerUri("phantom://a2V5@example.com:not-a-port").valid)
        assertFalse(parseServerUri("phantom://a2V5@example.com:70000").valid)
        assertFalse(parseServerUri("phantom://example.com:443").valid)
    }

    @Test
    fun historyRoundTrips() {
        val entries = listOf(
            ServerHistoryEntry("phantom://a2V5@example.com:443", 1_700_000_000_000, 0),
            ServerHistoryEntry("phantom://a2V5@example.org:443", 1_700_000_001_000, 1_700_000_002_000),
        )
        val restored = parseHistory(serializeHistory(entries))
        assertEquals(2, restored.size)
        // Newest first, regardless of the order they were written in.
        assertEquals("phantom://a2V5@example.org:443", restored[0].uri)
        assertEquals(1_700_000_002_000, restored[0].verifiedMs)
    }

    @Test
    fun upsertMovesToFrontAndKeepsVerification() {
        val first = upsertHistory(emptyList(), "phantom://a2V5@first.example.com:443", 10, 0)
        val verified = markVerified(first, "phantom://a2V5@first.example.com:443", 20)
        val second = upsertHistory(verified, "phantom://a2V5@second.example.com:443", 30, 0)
        val again = upsertHistory(second, "phantom://a2V5@first.example.com:443", 40, 0)

        assertEquals("phantom://a2V5@first.example.com:443", again[0].uri)
        // Re-using an entry must not erase the fact that it once worked.
        assertEquals(20, again[0].verifiedMs)
    }

    @Test
    fun upsertCapsHistory() {
        var entries = emptyList<ServerHistoryEntry>()
        for (i in 0 until HISTORY_MAX + 5) {
            entries = upsertHistory(entries, "phantom://a2V5@host$i.example.com:443", i.toLong(), 0)
        }
        assertEquals(HISTORY_MAX, entries.size)
        assertEquals("phantom://a2V5@host${HISTORY_MAX + 4}.example.com:443", entries[0].uri)
    }

    @Test
    fun parseHistorySkipsJunk() {
        val entries = parseHistory("not-a-uri\n\nphantom://a2V5@example.com:443\t1699999999000\t0")
        assertEquals(1, entries.size)
        assertEquals(1_699_999_999_000, entries[0].lastUsedMs)
    }

    @Test
    fun relativeTimeBuckets() {
        val now = 1_700_000_000_000
        assertEquals("刚刚", relativeTime(now - 5_000, now))
        assertEquals("5 分钟前", relativeTime(now - 5 * 60_000, now))
        assertEquals("3 小时前", relativeTime(now - 3 * 3_600_000, now))
        assertEquals("2 天前", relativeTime(now - 2 * 86_400_000, now))
        assertEquals("", relativeTime(0, now))
    }
}
