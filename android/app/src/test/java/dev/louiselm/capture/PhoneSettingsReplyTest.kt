package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneSettingsReplyTest {
    @Test
    fun onlyClosedBoundedSettingsAreAccepted() {
        val settings = """{"model":"Model A","effort":"High","mode":"Full access"}"""
        val page = """{"v":2,"type":"session_page","controller_id":"${"01".repeat(16)}","subject_id":"${"02".repeat(16)}","operation_id":"${"03".repeat(16)}","revision":1,"status":"ready","settings":$settings,"entries":[],"next_before":0,"complete":true,"source":{"replay":"not_applicable","archive":"unavailable","redaction":"unknown","missing":false,"truncated":false}}"""
        fun parse(text: String) = parsePhoneControlReply(text.toByteArray(Charsets.UTF_8))
        val parsed = parse(page)
        assertTrue(parsed is PhoneControlReply.SessionPage)
        assertEquals(PhoneSessionSettings("Model A", "High", "Full access"), (parsed as PhoneControlReply.SessionPage).settings)
        assertEquals(parsed.copy(settings = PhoneSessionSettings()), parsePhoneControlReply(encodePhoneOfflinePage(parsed)))
        val absent = page.replace(",\"settings\":$settings", "")
        assertEquals(PhoneSessionSettings(), (parse(absent) as PhoneControlReply.SessionPage).settings)
        for (bad in listOf(
            settings.dropLast(1) + ",\"private\":\"secret\"}",
            settings.replace("Model A", "x".repeat(257)),
            settings.replace("Model A", "A\\nB"),
            settings.replace("\"High\"", "true"),
            settings.replace(",\"mode\":\"Full access\"", ""),
        )) {
            assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED), parse(page.replace(settings, bad)))
        }
    }
}
