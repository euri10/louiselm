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
}
