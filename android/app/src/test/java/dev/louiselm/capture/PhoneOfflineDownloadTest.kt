package dev.louiselm.capture

import javax.crypto.spec.SecretKeySpec
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneOfflineDownloadTest {
    @get:Rule val temporary = TemporaryFolder()
    private val key = SecretKeySpec(ByteArray(32) { 1 }, "AES")
    private val target = PhoneControlTarget("11".repeat(16), "22".repeat(16))
    private val source = PhoneOfflineSource("aa".repeat(32), target, "name", "agent", "project")
    private fun page(seq: Long, next: Long) = PhoneControlReply.SessionPage(target.controllerId, target.subjectId, "33".repeat(16),
        1, "ready", listOf(PhoneControlEntry.Message(seq, "assistant", "message $seq", false)), next, true,
        PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false))

    @Test
    fun keepOfflineReadsOlderPagesOnOneRevisionWithoutRenewingInteraction() {
        val store = PhoneOfflineStore(temporary.newFolder())
        val requests = mutableListOf<Triple<Long, Long, Boolean>>()
        val result = downloadPhoneOffline(store, source, key, 1_000_000, 10, true, { true }, {}) { cursor, revision, interaction ->
            requests.add(Triple(cursor, revision, interaction))
            PhoneControlResult.Value(if (cursor == 0L) page(2, 2) else page(1, 0))
        }
        assertEquals(PhoneControlResult.Value(PhoneOfflineDownload(PhoneOfflineSave.SAVED, null)), result)
        assertEquals(listOf(Triple(0L, 0L, true), Triple(2L, 1L, false)), requests)
        val saved = store.list(source.receiver, key).copies.single()
        assertTrue(saved.pinned)
        assertFalse(saved.incomplete)
        assertEquals(page(1, 0), store.readPage(source, 1, key))
    }

    @Test
    fun interruptionLabelsAnInitialPartialPinButPreservesAnExistingCopy() {
        val store = PhoneOfflineStore(temporary.newFolder())
        val partial = downloadPhoneOffline(store, source, key, 1_000_000, 10, true, { true }, {}) { cursor, _, _ ->
            if (cursor == 0L) PhoneControlResult.Value(page(2, 2)) else PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        }
        assertEquals(PhoneControlResult.Value(PhoneOfflineDownload(PhoneOfflineSave.SAVED, PhoneControlOutcome.UNAVAILABLE)), partial)
        assertTrue(store.list(source.receiver, key).copies.single().incomplete)
        val old = store.readPage(source, 0, key)
        val refresh = downloadPhoneOffline(store, source, key, 1_000_000, 20, false, { true }, {}) { cursor, _, _ ->
            if (cursor == 0L) PhoneControlResult.Value(page(3, 3)) else PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        }
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE), refresh)
        assertEquals(old, store.readPage(source, 0, key))
    }

    @Test
    fun failedRefreshKeepsAPreviouslyCompletePinAndMarksItOutdated() {
        val store = PhoneOfflineStore(temporary.newFolder())
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(page(1, 0)), true, 10, 10, 1_000_000, key) { true })
        val old = store.list(source.receiver, key).copies.single()
        assertFalse(old.incomplete)
        val refresh = downloadPhoneOffline(store, source, key, 1_000_000, 20, false, { true }, {}) { _, _, interaction ->
            assertFalse(interaction)
            PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        }
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE), refresh)
        val retained = store.list(source.receiver, key).copies.single()
        assertFalse(retained.incomplete)
        assertTrue(retained.refreshFailed)
        assertEquals(old.openedAtMs, retained.openedAtMs)
        assertEquals(old.refreshedAtMs, retained.refreshedAtMs)
        assertEquals(page(1, 0), store.readPage(source, 0, key))
        val success = downloadPhoneOffline(store, source, key, 1_000_000, 30, false, { true }, {}) { _, _, _ ->
            PhoneControlResult.Value(page(2, 0))
        }
        assertEquals(PhoneControlResult.Value(PhoneOfflineDownload(PhoneOfflineSave.SAVED, null)), success)
        val updated = store.list(source.receiver, key).copies.single()
        assertFalse(updated.incomplete)
        assertFalse(updated.refreshFailed)
        assertEquals(old.openedAtMs, updated.openedAtMs)
        assertEquals(30, updated.refreshedAtMs)
    }

    @Test
    fun overBudgetRefreshRetainsThePinAndMarksItOutdated() {
        val store = PhoneOfflineStore(temporary.newFolder())
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(page(1, 0)), true, 10, 10, 1_000_000, key) { true })
        val refresh = downloadPhoneOffline(store, source, key, 1, 20, false, { true }, {}) { _, _, _ ->
            PhoneControlResult.Value(page(2, 0))
        }
        assertEquals(PhoneControlResult.Value(PhoneOfflineDownload(PhoneOfflineSave.BYTE_LIMIT, null)), refresh)
        assertTrue(store.list(source.receiver, key).copies.single().refreshFailed)
        assertEquals(page(1, 0), store.readPage(source, 0, key))
    }

    @Test
    fun revocationDuringDownloadNeverPublishesPartialData() {
        val store = PhoneOfflineStore(temporary.newFolder())
        val result = downloadPhoneOffline(store, source, key, 1_000_000, 10, true, { true }, {}) { cursor, _, _ ->
            if (cursor == 0L) PhoneControlResult.Value(page(2, 2)) else PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        }
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.REVOKED), result)
        assertTrue(store.list(source.receiver, key).copies.isEmpty())
    }
}
