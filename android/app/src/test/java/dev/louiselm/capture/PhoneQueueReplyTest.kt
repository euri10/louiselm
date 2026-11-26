package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneQueueReplyTest {
    @Test
    fun queueIsClosedBoundedAndNeverSavedAsCurrent() {
        val item = """{"id":"${"11".repeat(16)}","text":"waiting","truncated":false}"""
        val queue = """{"revision":4,"paused":true,"uncertain":false,"entries":[$item],"active":{"entry":{"id":"${"22".repeat(16)}","text":"active","truncated":false},"status":"dispatched"},"replacement":{"entry":{"id":"${"33".repeat(16)}","text":"reserved","truncated":false},"status":"unsent","cancellation":"unknown"}}"""
        val page = """{"v":2,"type":"session_page","controller_id":"${"01".repeat(16)}","subject_id":"${"02".repeat(16)}","operation_id":"${"03".repeat(16)}","revision":1,"status":"prompting","queue":$queue,"entries":[],"next_before":0,"complete":true,"source":{"replay":"not_applicable","archive":"unavailable","redaction":"unknown","missing":false,"truncated":false}}"""
        fun parse(value: String) = parsePhoneControlReply(value.toByteArray(Charsets.UTF_8))
        val live = parse(page)
        assertTrue(live is PhoneControlReply.SessionPage)
        val current = requireNotNull((live as PhoneControlReply.SessionPage).queue)
        assertEquals(listOf("waiting"), current.entries.map { it.text })
        assertEquals("dispatched", current.active?.status)
        assertEquals("unsent", current.replacement?.status)
        assertEquals("unknown", current.replacement?.cancellation)
        val saved = encodePhoneOfflinePage(live)
        assertFalse(String(saved, Charsets.UTF_8).contains("waiting"))
        assertEquals(null, (parsePhoneControlReply(saved) as PhoneControlReply.SessionPage).queue)
        for (bad in listOf(
            queue.replace("\"entries\":[$item]", "\"entries\":[$item,$item]"),
            queue.replace("\"entries\":[$item]", "\"entries\":[${List(21) { item }.joinToString()}]"),
            queue.replace("\"waiting\"", "\"${"x".repeat(513)}\""),
            queue.replace("\"waiting\"", "\"waiting\",\"prompt\":\"secret\""),
            queue.replaceFirst("\"truncated\":false", "\"truncated\":true"),
            queue.replace("\"dispatched\"", "\"completed\""),
            queue.replace("\"unknown\"", "\"maybe\""),
        )) {
            assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED), parse(page.replace(queue, bad)))
        }
    }
}
