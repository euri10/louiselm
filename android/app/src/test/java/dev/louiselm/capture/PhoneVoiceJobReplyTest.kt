package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class PhoneVoiceJobReplyTest {
    @Test fun repliesAreClosedAndBoundToTheirOperationAndRecording() {
        val operation = "11".repeat(16)
        val id = "01010101-0101-0101-0101-010101010101"
        val success = """{"v":1,"type":"voice_job","operation_id":"$operation","id":"$id","state":"pending"}"""
        assertEquals(PhoneVoiceJobResponse.Accepted(operation, id, "pending"), parsePhoneVoiceJobResponse(success.toByteArray()))
        val invalid = listOf(
            success.replace("\"state\":\"pending\"", "\"state\":\"pending\",\"state\":\"failed\""),
            success.replace("\"pending\"", "\"unknown\""),
            success.replace("\"v\":1", "\"v\":2"),
            success.replace("\"pending\"", "\"pending\",\"extra\":0"),
            success + "{}",
        )
        for (item in invalid) assertEquals(PhoneVoiceJobResponse.Failure(PhoneControlOutcome.MALFORMED), parsePhoneVoiceJobResponse(item.toByteArray()))
        assertEquals(PhoneVoiceJobResponse.Failure(PhoneControlOutcome.OUT_OF_SCOPE), parsePhoneVoiceJobResponse(
            """{"v":1,"type":"error","code":"out_of_scope"}""".toByteArray()))
    }
}
