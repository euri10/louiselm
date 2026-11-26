package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertArrayEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner

@RunWith(RobolectricTestRunner::class)
class PhoneVoiceArchiveReplyTest {
    @Test fun acceptsBoundedArchivedVoiceHistory() {
        val body = """{"v":1,"type":"voice_history","operation_id":"${"11".repeat(16)}",
            "records":[{"id":"11111111-1111-4111-8111-111111111111","controller":"${"22".repeat(16)}",
            "subject":"${"33".repeat(16)}","project":"/project","agent":"codex","has_result":true}],"next_after":""}"""
        assertEquals("VoiceHistory", parsePhoneControlReply(body.toByteArray()).javaClass.simpleName)
    }

    @Test fun zeroUuidCursorIsDistinctFromTheFirstPage() {
        val first = phoneVoiceHistoryPayload(null, 1)
        val next = phoneVoiceHistoryPayload("00000000-0000-0000-0000-000000000000", 1)
        assertEquals(0, first[0].toInt())
        assertEquals(1, next[0].toInt())
        assertArrayEquals(first.copyOfRange(1, 18), next.copyOfRange(1, 18))
    }

    @Test fun resultLengthIsRequiredAndBounded() {
        val body = """{"v":1,"type":"voice_result","operation_id":"${"11".repeat(16)}",
            "id":"11111111-1111-4111-8111-111111111111","receiver":"${"22".repeat(32)}",
            "controller":"${"33".repeat(16)}","subject":"${"44".repeat(16)}","project":"/project","agent":"codex",
            "draft":"${"55".repeat(32)}","revision":"${"66".repeat(32)}","offset":0,"next_offset":0,"total_bytes":2,"text":"é"}"""
        val result = parsePhoneControlReply(body.toByteArray()) as PhoneControlReply.VoiceResult
        assertEquals(2, result.totalBytes)
        for (bad in listOf(body.replace("\"total_bytes\":2,", ""), body.replace("\"total_bytes\":2", "\"total_bytes\":-1"),
            body.replace("\"total_bytes\":2", "\"total_bytes\":67108865"))) {
            assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED), parsePhoneControlReply(bad.toByteArray()))
        }
    }
}
