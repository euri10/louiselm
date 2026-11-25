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
}
