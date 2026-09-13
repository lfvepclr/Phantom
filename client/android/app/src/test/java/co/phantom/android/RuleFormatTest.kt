package co.phantom.android

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

/*
 * The rule parser is the contract between what the user typed and what the core
 * will actually match on, so every accepted shape and every refusal here is
 * backed by the same behaviour on the HarmonyOS client.
 */
class RuleFormatTest {

    @Test
    fun `bare domain normalises to exact match`() {
        val ok = RuleFormat.parse("WWW.Google.COM") as RuleParse.Ok
        assertEquals(RuleKind.DOMAIN, ok.kind)
        assertEquals("www.google.com", ok.value)
        assertEquals("domain:www.google.com", ok.wire)
    }

    @Test
    fun `star prefix and dot prefix become suffix`() {
        assertEquals(
            "suffix:google.com",
            (RuleFormat.parse("*.google.com") as RuleParse.Ok).wire,
        )
        assertEquals(
            "suffix:google.com",
            (RuleFormat.parse(".google.com") as RuleParse.Ok).wire,
        )
    }

    @Test
    fun `clash style line is understood`() {
        val ok = RuleFormat.parse("DOMAIN-SUFFIX,example.com") as RuleParse.Ok
        assertEquals(RuleKind.SUFFIX, ok.kind)
        assertEquals("suffix:example.com", ok.wire)
    }

    @Test
    fun `pasted url is stripped to the host`() {
        val ok = RuleFormat.parse("https://www.youtube.com/watch?v=abc") as RuleParse.Ok
        assertEquals("domain:www.youtube.com", ok.wire)
    }

    @Test
    fun `cidr is accepted and validated`() {
        assertEquals("cidr:91.108.4.0/22", (RuleFormat.parse("91.108.4.0/22") as RuleParse.Ok).wire)
        assertTrue(RuleFormat.parse("91.108.4.0/33") is RuleParse.Invalid)
    }

    @Test
    fun `regex is compiled before it is accepted`() {
        assertEquals(
            "regex:^ads?[0-9]+\\.example\\.com$",
            (RuleFormat.parse("regex:^ads?[0-9]+\\.example\\.com$") as RuleParse.Ok).wire,
        )
        assertTrue(RuleFormat.parse("regex:([unclosed") is RuleParse.Invalid)
    }

    @Test
    fun `a bare ip is refused rather than silently unmatched`() {
        val bad = RuleFormat.parse("142.250.72.14")
        assertTrue(bad is RuleParse.Invalid)
        assertTrue((bad as RuleParse.Invalid).reason.contains("IP"))
    }

    @Test
    fun `keyword must not contain a dot`() {
        assertEquals("keyword:youtube", (RuleFormat.parse("keyword:youtube") as RuleParse.Ok).wire)
        assertTrue(RuleFormat.parse("keyword:you.tube") is RuleParse.Invalid)
    }

    @Test
    fun `comments and blank lines are refused with a reason`() {
        assertTrue(RuleFormat.parse("") is RuleParse.Invalid)
        assertTrue(RuleFormat.parse("# just a comment") is RuleParse.Invalid)
    }

    @Test
    fun `ip range folds into the minimal cidr cover`() {
        assertEquals(listOf("0.0.0.0/0"), RuleFormat.ipRangeToCidrs("0.0.0.0", "255.255.255.255"))
        assertEquals(
            listOf("10.0.0.0/24"),
            RuleFormat.ipRangeToCidrs("10.0.0.0", "10.0.0.255"),
        )
        assertEquals(
            listOf("10.0.0.1/32", "10.0.0.2/31", "10.0.0.4/30", "10.0.0.8/29"),
            RuleFormat.ipRangeToCidrs("10.0.0.1", "10.0.0.15"),
        )
        assertEquals(listOf("1.2.3.4/32"), RuleFormat.ipRangeToCidrs("1.2.3.4", "1.2.3.4"))
    }

    @Test
    fun `inverted or malformed ranges are rejected`() {
        assertNull(RuleFormat.ipRangeToCidrs("10.0.0.255", "10.0.0.0"))
        assertNull(RuleFormat.ipRangeToCidrs("10.0.0", "10.0.0.255"))
        assertNull(RuleFormat.ipRangeToCidrs("10.0.0.256", "10.0.0.255"))
    }

    @Test
    fun `wire text round trips`() {
        val wire = "domain:a.com\nsuffix:b.com\nkeyword:ads"
        val entries = RuleFormat.fromWire(wire)
        assertEquals(3, entries.size)
        assertEquals(RuleKind.KEYWORD to "ads", entries[2])
        assertEquals(
            wire,
            entries.joinToString("\n") { (kind, value) -> RuleFormat.toWire(kind, value) },
        )
    }
}
