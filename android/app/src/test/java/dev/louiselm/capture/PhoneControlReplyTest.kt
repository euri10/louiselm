package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneControlReplyTest {
    private fun parse(value: String) = parsePhoneControlReply(value.toByteArray(Charsets.UTF_8))

    @Test
    fun closedRepliesPreserveTypedFailuresAndRejectAmbiguousJson() {
        val offer = """{"v":1,"type":"enrollment_offer","enrollment_id":"00112233445566778899aabbccddeeff","matching_code":"00123456","receiver_identity":"${"01".repeat(32)}","expires_at_ms":123}"""
        val parsed = parse(offer)
        assertTrue(parsed is PhoneControlReply.Offer)
        assertEquals("00123456", (parsed as PhoneControlReply.Offer).matchingCode)
        for (bad in listOf(
            offer.replace("\"v\":1", "\"v\":1,\"v\":1"),
            offer.replace("\"v\":1", "\"v\":1.0"),
            offer.replace("\"v\":1", "\"v\":1e0"),
            offer.dropLast(1) + ",\"unknown\":0}",
            offer + " {}",
            offer.replace("00123456", "123456"),
            offer.replace("00112233445566778899aabbccddeeff", "00112233445566778899AABBCCDDEEFF"),
        )) {
            assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED), parse(bad))
        }
        assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.EXPIRED), parse("""{"v":1,"type":"error","code":"expired"}"""))
        assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.UNSUPPORTED), parse("""{"v":2,"type":"error","code":"expired"}"""))
        assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.UNSUPPORTED), parse("""{"v":1,"type":"error","code":"new_code"}"""))
        assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED), parsePhoneControlReply(byteArrayOf(0xff.toByte())))
    }

    @Test
    fun sessionPagesAcceptClosedEntriesAndRejectAmbiguousContent() {
        val page = """{"v":1,"type":"session_page","controller_id":"${"01".repeat(16)}","subject_id":"${"02".repeat(16)}","operation_id":"${"03".repeat(16)}","revision":1,"status":"ready","entries":[{"kind":"message","seq":1,"role":"assistant","text":"hello","truncated":false},{"kind":"question","seq":2,"request_id":"q","question":"choose","options":[{"label":"yes","description":"","preview":""}],"multi_select":false,"truncated":false}],"next_before":0,"complete":true}"""
        val parsed = parse(page)
        assertTrue(parsed is PhoneControlReply.SessionPage)
        assertEquals(2, (parsed as PhoneControlReply.SessionPage).entries.size)
        for (bad in listOf(
            page.replace("\"seq\":2", "\"seq\":1"),
            page.replace("\"role\":\"assistant\"", "\"role\":\"system\""),
            page.replace("\"text\":\"hello\"", "\"text\":\"hello\",\"text\":\"other\""),
            page.replace("\"multi_select\":false", "\"multi_select\":false,\"extra\":true"),
            page.replace("\"status\":\"ready\"", "\"status\":\"disposed\""),
        )) {
            assertTrue(parse(bad) is PhoneControlReply.Failure)
        }
    }
}
