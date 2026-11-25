package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Test

class PhoneOfflineRetentionTest {
    private fun copy(id: String, bytes: Long = 1, opened: Long = 0, pinned: Boolean = false) =
        PhoneOfflineUsage(id, bytes, opened, pinned)

    @Test
    fun exactAgeAndLeastRecentlyOpenedControlEviction() {
        val month = 30L * 24 * 60 * 60 * 1000
        val entries = listOf(copy("old", opened = 0), copy("recent", opened = 1), copy("pin", pinned = true))
        assertEquals(PhoneOfflinePlan.Accept(listOf("old")),
            planPhoneOfflineRetention(entries, null, 3, month))
        assertEquals(PhoneOfflinePlan.Accept(emptyList()),
            planPhoneOfflineRetention(entries, null, 3, month - 1))
        assertEquals(PhoneOfflinePlan.Accept(listOf("old", "recent")),
            planPhoneOfflineRetention(entries, copy("new", bytes = 2, opened = month), 3, month))
    }

    @Test
    fun bothLimitsIncludePinsAndFailuresPreserveExistingCopies() {
        val pins = (0 until 200).map { copy("pin$it", pinned = true) }
        assertEquals(PhoneOfflinePlan.Refuse.SESSION_LIMIT,
            planPhoneOfflineRetention(pins, copy("new"), 1000, 1))
        assertEquals(PhoneOfflinePlan.Refuse.BYTE_LIMIT,
            planPhoneOfflineRetention(listOf(copy("pin", bytes = 9, pinned = true), copy("old")),
                copy("new", bytes = 2, opened = 1), 10, 1))
        assertEquals(PhoneOfflinePlan.Refuse.BYTE_LIMIT,
            planPhoneOfflineRetention(listOf(copy("pin", bytes = 9, pinned = true)),
                copy("pin", bytes = 11, opened = 1, pinned = true), 10, 1))
        assertEquals(PhoneOfflinePlan.Accept(emptyList()),
            planPhoneOfflineRetention(listOf(copy("pin", bytes = 9, pinned = true)),
                copy("pin", bytes = 10, opened = 1, pinned = true), 10, 1))
    }

    @Test
    fun replacementDoesNotCountTwiceAndEqualAgesHaveStableOrder() {
        val copies = listOf(copy("b", opened = 5), copy("a", opened = 5), copy("pin", pinned = true))
        assertEquals(PhoneOfflinePlan.Accept(listOf("a")),
            planPhoneOfflineRetention(copies, copy("b", bytes = 2, opened = 5), 3, 6))
        assertEquals(512L * 1024 * 1024, PHONE_OFFLINE_DEFAULT_BUDGET)
        assertEquals(200, PHONE_OFFLINE_SESSION_LIMIT)
    }
}
