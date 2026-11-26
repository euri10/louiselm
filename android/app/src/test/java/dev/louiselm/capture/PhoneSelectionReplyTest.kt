package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneSelectionReplyTest {
    @Test
    fun acceptsOnlyBoundedClosedSelectionPages() {
        val page = """{"v":1,"type":"selection_list","controller_id":"${"01".repeat(16)}","operation_id":"${"02".repeat(16)}","agents":["codex"],"projects":[{"id":"${"03".repeat(16)}","path":"/work"}],"next_after":"","operate_sessions":true}"""
        fun parse(value: String) = parsePhoneControlReply(value.toByteArray(Charsets.UTF_8))
        val parsed = parse(page)
        assertTrue(parsed is PhoneControlReply.SelectionList)
        parsed as PhoneControlReply.SelectionList
        assertEquals(listOf("codex"), parsed.agents)
        assertEquals(listOf(PhoneSelectionProject("03".repeat(16), "/work")), parsed.projects)
        assertTrue(parsed.operateSessions)
        val unicode = page.replace("\"agents\":[\"codex\"]", "\"agents\":[\"\uE000\",\"😀\"]")
        assertTrue(parse(unicode) is PhoneControlReply.SelectionList)
        for (bad in listOf(
            page.replace("\"operate_sessions\":true", "\"operate_sessions\":true,\"extra\":1"),
            page.replace("\"path\":\"/work\"", "\"path\":\"/work\",\"extra\":1"),
            page.replace("\"agents\":[\"codex\"]", "\"agents\":[\"codex\",\"codex\"]"),
            page.replace("\"next_after\":\"\"", "\"next_after\":\"${"04".repeat(16)}\""),
        )) assertTrue(parse(bad) is PhoneControlReply.Failure)
    }
}
