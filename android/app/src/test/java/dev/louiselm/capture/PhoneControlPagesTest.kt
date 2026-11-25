package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PhoneControlPagesTest {
    private fun page(revision: Long, sequences: List<Long>, next: Long = 0) = PhoneControlReply.SessionPage(
        "controller", "subject", "operation", revision, "ready",
        sequences.map { PhoneControlEntry.Message(it, "assistant", "message $it", false) }, next, true,
    )

    @Test
    fun refreshRebuildsTheOldRangeAtTheNewRevision() {
        val calls = mutableListOf<Triple<Long, Long, Int>>()
        val result = reconcilePhoneSessionPages(page(2, listOf(5, 6), 5), page(1, listOf(1, 2)), 500,
            { before, revision, limit ->
                calls += Triple(before, revision, limit)
                PhoneControlResult.Value(page(2, listOf(1, 2, 3, 4)))
            }, { throw AssertionError("unexpected restart") })
        val refresh = (result as PhoneControlResult.Value).value
        assertEquals(listOf(1L, 2L, 3L, 4L, 5L, 6L), refresh.page.entries.map { it.seq })
        assertEquals(listOf(Triple(5L, 2L, 50)), calls)
        assertEquals(false, refresh.missingOlder)
    }

    @Test
    fun stalePageRestartsFreshAndRevocationCannotExposeAnOldSnapshot() {
        val newest = page(2, listOf(5, 6), 5)
        val previous = page(1, listOf(1, 2))
        val restarted = reconcilePhoneSessionPages(newest, previous, 500,
            { _, _, _ -> PhoneControlResult.Failure(PhoneControlOutcome.STALE_SUBJECT) },
            { PhoneControlResult.Value(page(3, listOf(7, 8))) })
        val refresh = (restarted as PhoneControlResult.Value).value
        assertEquals(listOf(7L, 8L), refresh.page.entries.map { it.seq })
        assertTrue(refresh.missingOlder)
        val revoked = reconcilePhoneSessionPages(newest, previous, 500,
            { _, _, _ -> PhoneControlResult.Failure(PhoneControlOutcome.REVOKED) },
            { throw AssertionError("must not retry a revoked period") })
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.REVOKED), revoked)
    }

    @Test
    fun refreshKeepsTheNewestBoundedWindowAndAnOlderCursor() {
        val result = reconcilePhoneSessionPages(page(2, listOf(5, 6), 5), page(1, listOf(1, 2)), 4,
            { before, revision, limit ->
                assertEquals(Triple(5L, 2L, 2), Triple(before, revision, limit))
                PhoneControlResult.Value(page(2, listOf(3, 4), 3))
            }, { throw AssertionError("unexpected restart") })
        val refresh = (result as PhoneControlResult.Value).value
        assertEquals(listOf(3L, 4L, 5L, 6L), refresh.page.entries.map { it.seq })
        assertEquals(3L, refresh.page.nextBefore)
        assertTrue(refresh.missingOlder)
    }
}
