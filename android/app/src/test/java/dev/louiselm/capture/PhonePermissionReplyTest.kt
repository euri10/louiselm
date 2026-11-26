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
class PhonePermissionReplyTest {
    private val page = """{"v":2,"type":"session_page","controller_id":"${"01".repeat(16)}","subject_id":"${"02".repeat(16)}","operation_id":"${"03".repeat(16)}","revision":1,"status":"waiting_permission","entries":[],"next_before":0,"complete":true,"source":{"replay":"not_applicable","archive":"unavailable","redaction":"unknown","missing":false,"truncated":false},"current_permission":{"title":"Edit file","options":["Allow once","Reject"],"incomplete":false}}"""
    private fun parse(value: String) = parsePhoneControlReply(value.toByteArray(Charsets.UTF_8))

    @Test
    fun currentPermissionIsBoundedAndNeverSavedAsCurrent() {
        val live = parse(page) as PhoneControlReply.SessionPage
        assertEquals("Edit file", live.currentPermission?.title)
        assertEquals(listOf("Allow once", "Reject"), live.currentPermission?.options)
        val saved = encodePhoneOfflinePage(live)
        assertFalse(saved.toString(Charsets.UTF_8).contains("current_permission"))
        assertEquals(null, (parsePhoneControlReply(saved) as PhoneControlReply.SessionPage).currentPermission)
        for (bad in listOf(
            page.replace("\"status\":\"waiting_permission\"", "\"status\":\"ready\""),
            page.replace("\"incomplete\":false", "\"incomplete\":false,\"raw_input\":{}"),
            page.replace("\"Edit file\"", "\"${"x".repeat(8193)}\""),
            page.replace("\"incomplete\":false", "\"incomplete\":true"),
            page.replace("[\"Allow once\",\"Reject\"]", "[${List(51) { "\"x\"" }.joinToString(",")} ]"),
        )) assertTrue(parse(bad) is PhoneControlReply.Failure)
    }
}
