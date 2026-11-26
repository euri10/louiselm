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
class PhoneInteractionReplyTest {
    private val question = """{"kind":"question","seq":1,"request_id":"7","question":"Choose?","options":[{"label":"Yes","description":"Proceed","preview":"preview"}],"multi_select":true,"truncated":false}"""
    private val page = """{"v":2,"type":"session_page","controller_id":"${"01".repeat(16)}","subject_id":"${"02".repeat(16)}","operation_id":"${"03".repeat(16)}","revision":1,"status":"prompting","entries":[$question],"next_before":0,"complete":true,"source":{"replay":"not_applicable","archive":"unavailable","redaction":"unknown","missing":false,"truncated":false},"current_interactions":{"entries":[$question],"incomplete":false}}"""
    private fun parse(value: String) = parsePhoneControlReply(value.toByteArray(Charsets.UTF_8))

    @Test
    fun currentRequestsAreBoundedAndNeverSavedAsCurrent() {
        val live = parse(page) as PhoneControlReply.SessionPage
        val current = requireNotNull(live.currentInteractions)
        assertEquals(live.entries, current.entries)
        assertTrue((current.entries.single() as PhoneControlEntry.Question).multiSelect)
        val saved = encodePhoneOfflinePage(live)
        assertFalse(saved.toString(Charsets.UTF_8).contains("current_interactions"))
        assertEquals(null, (parsePhoneControlReply(saved) as PhoneControlReply.SessionPage).currentInteractions)
        assertTrue(parse(page.replace("\"complete\":true", "\"complete\":false").replace("\"incomplete\":false", "\"incomplete\":true")) is PhoneControlReply.SessionPage)
        for (bad in listOf(
            page.replace("\"incomplete\":false", "\"incomplete\":false,\"respond\":\"secret\""),
            page.replace("\"entries\":[$question],\"incomplete\":false", "\"entries\":[],\"incomplete\":false"),
            page.replace("\"entries\":[$question],\"incomplete\":false", "\"entries\":[$question,$question],\"incomplete\":false"),
            page.replace("\"incomplete\":false", "\"incomplete\":true"),
            page.replace("\"current_interactions\":{\"entries\":[$question],\"incomplete\":false}", "\"current_interactions\":null"),
            page.replace("\"entries\":[$question],\"incomplete\":false", "\"entries\":[{\"kind\":\"message\",\"seq\":2,\"role\":\"assistant\",\"text\":\"Ordinary?\",\"truncated\":false}],\"incomplete\":false"),
            page.replace("\"entries\":[$question],\"incomplete\":false", "\"entries\":[${question.replace("Choose?", "Different?")}],\"incomplete\":false"),
        )) assertTrue(parse(bad) is PhoneControlReply.Failure)
        assertEquals(1, live.entries.size)
    }
}
