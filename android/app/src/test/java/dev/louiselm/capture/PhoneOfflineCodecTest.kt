package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertThrows
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneOfflineCodecTest {
    @Test
    fun storesOnlyValidVersionTwoOwnerProjections() {
        val page = PhoneControlReply.SessionPage("11".repeat(16), "22".repeat(16), "33".repeat(16), 4, "ready",
            listOf(
                PhoneControlEntry.Message(1, "user", "hello", false),
                PhoneControlEntry.Tool(2, "id", "tool", "completed", "result", false),
                PhoneControlEntry.Question(3, "id", "question", listOf(PhoneControlOption("yes", "", "")), false, false),
                PhoneControlEntry.Plan(4, "id", "plan", true),
            ), 0, false, PhoneHistorySource("unverified", "unavailable", "present", true, true))
        assertEquals(page, parsePhoneControlReply(encodePhoneOfflinePage(page)))
        assertThrows(IllegalArgumentException::class.java) {
            encodePhoneOfflinePage(page.copy(entries = page.entries.reversed()))
        }
        assertThrows(IllegalArgumentException::class.java) {
            encodePhoneOfflinePage(page.copy(controllerId = "bad"))
        }
    }
}
