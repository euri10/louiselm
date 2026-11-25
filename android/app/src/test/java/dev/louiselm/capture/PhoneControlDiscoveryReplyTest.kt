package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneControlDiscoveryReplyTest {
    private fun parse(text: String) = parsePhoneControlReply(text.toByteArray(Charsets.UTF_8))

    @Test
    fun acceptsBoundedDiscoveryAndRejectsAmbiguousRows() {
        val controller = "11".repeat(16)
        val operation = "22".repeat(16)
        val subject = "33".repeat(16)
        val controllers = """{"v":1,"type":"controller_list","operation_id":"$operation","controllers":["$controller"],"next_after":""}"""
        val sessions = """{"v":1,"type":"session_list","controller_id":"$controller","operation_id":"$operation","sessions":[{"subject_id":"$subject","name":"same","agent":"mock","project":"/work","status":"ready","truncated":false}],"next_after":""}"""
        assertFalse(parse(controllers) is PhoneControlReply.Failure)
        assertFalse(parse(sessions) is PhoneControlReply.Failure)
        val malformed = PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED)
        assertEquals(malformed, parse(controllers.replace("[\"$controller\"]", "[\"$controller\",\"$controller\"]")))
        assertEquals(malformed, parse(sessions.replace("\"truncated\":false", "\"truncated\":false,\"extra\":1")))
        assertEquals(malformed, parse(sessions.replace("\"name\":\"same\"", "\"name\":\"${"x".repeat(513)}\"")))
    }
}
