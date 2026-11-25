package dev.louiselm.capture

import java.io.IOException
import java.nio.charset.StandardCharsets
import java.security.GeneralSecurityException
import javax.crypto.spec.SecretKeySpec
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneOfflineStoreTest {
    @get:Rule val temporary = TemporaryFolder()
    private val key = SecretKeySpec(ByteArray(32) { it.toByte() }, "AES")
    private val receiver = "aa".repeat(32)
    private val target = PhoneControlTarget("11".repeat(16), "22".repeat(16))

    private fun page(text: String, subject: PhoneControlTarget = target) = PhoneControlReply.SessionPage(subject.controllerId, subject.subjectId, "33".repeat(16),
        4, "ready", listOf(PhoneControlEntry.Message(1, "assistant", text, false)), 0, true,
        PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false))

    @Test
    fun encryptedCopiesPreserveProvenanceAndOnlyOpeningChangesTheAge() {
        val root = temporary.newFolder()
        val store = PhoneOfflineStore(root)
        val source = PhoneOfflineSource(receiver, target, "private name", "agent", "project")
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(page("private content")), false, 100, 100, 1_000_000, key) { true })
        val first = store.list(receiver, key).copies.single()
        assertEquals(100, first.openedAtMs)
        assertEquals(page("private content"), store.readPage(source, 0, key))
        assertTrue(root.walkTopDown().filter { it.isFile }.none {
            String(it.readBytes(), StandardCharsets.ISO_8859_1).contains("private")
        })
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(page("refreshed")), true, null, 200, 1_000_000, key) { true })
        val refreshed = store.list(receiver, key).copies.single()
        assertEquals(100, refreshed.openedAtMs)
        assertEquals(200, refreshed.refreshedAtMs)
        assertTrue(refreshed.pinned)
        store.markOpened(source, 300, key)
        assertEquals(300, store.list(receiver, key).copies.single().openedAtMs)
    }

    @Test
    fun failedOrOverBudgetRefreshPreservesThePreviousPinnedCopy() {
        val store = PhoneOfflineStore(temporary.newFolder())
        val source = PhoneOfflineSource(receiver, target, "name", "agent", "project")
        val original = page("original")
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(original), true, 100, 100, 1_000_000, key) { true })
        val bytes = store.list(receiver, key).copies.single().bytes
        assertEquals(PhoneOfflineSave.BYTE_LIMIT,
            store.save(source, sequenceOf(page("longer replacement")), true, null, 200, bytes, key) { true })
        assertThrows(IOException::class.java) {
            store.save(source, sequence { yield(page("partial")); throw IOException("fixture") },
                true, null, 200, 1_000_000, key) { true }
        }
        assertEquals(original, store.readPage(source, 0, key))
        assertEquals(100, store.list(receiver, key).copies.single().refreshedAtMs)
    }

    @Test
    fun wrongKeyAndCorruptionDoNotDeleteOtherCopies() {
        val root = temporary.newFolder()
        val store = PhoneOfflineStore(root)
        val source = PhoneOfflineSource(receiver, target, "name", "agent", "project")
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(page("content")), false, 1, 1, 1_000_000, key) { true })
        val wrongKey = SecretKeySpec(ByteArray(32) { 9 }, "AES")
        val unreadableId = store.list(receiver, wrongKey).damaged.single()
        store.removeDamaged(unreadableId, key)
        assertEquals(page("content"), store.readPage(source, 0, key))
        assertFalse(store.list(receiver, key).copies.isEmpty())
    }

    @Test
    fun explicitlyRemovingDamagedMetadataPreservesAnotherSource() {
        val root = temporary.newFolder()
        val store = PhoneOfflineStore(root)
        val source = PhoneOfflineSource(receiver, target, "one", "agent", "project")
        val otherTarget = PhoneControlTarget("11".repeat(16), "44".repeat(16))
        val other = PhoneOfflineSource(receiver, otherTarget, "two", "agent", "project")
        store.save(source, sequenceOf(page("one")), true, 1, 1, 1_000_000, key) { true }
        val metadata = root.walkTopDown().single { it.name.endsWith(".meta") }
        store.save(other, sequenceOf(page("two", otherTarget)), true, 1, 1, 1_000_000, key) { true }
        val bytes = metadata.readBytes()
        bytes[0] = (bytes[0].toInt() xor 1).toByte()
        metadata.writeBytes(bytes)
        val damaged = store.listAll(key).damaged.single()
        store.removeDamaged(damaged, key)
        assertTrue(store.listAll(key).damaged.isEmpty())
        assertEquals(page("two", otherTarget), store.readPage(other, 0, key))
        assertEquals(PhoneOfflineSave.REVOKED, store.save(source, sequenceOf(page("late")), true, 2, 2, 1_000_000, key) { true })
    }

    @Test
    fun damagedPageIsIsolatedAndActualOpenControlsExpiry() {
        val root = temporary.newFolder()
        val store = PhoneOfflineStore(root)
        val source = PhoneOfflineSource(receiver, target, "one", "agent", "project")
        val otherTarget = PhoneControlTarget("11".repeat(16), "44".repeat(16))
        val other = PhoneOfflineSource(receiver, otherTarget, "two", "agent", "project")
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(page("one")), false, 1, 1, 1_000_000, key) { true })
        val firstData = root.walkTopDown().single { it.name.endsWith(".data") }
        assertEquals(PhoneOfflineSave.SAVED, store.save(other, sequenceOf(page("two", otherTarget)), true, 1, 1, 1_000_000, key) { true })
        val bytes = firstData.readBytes()
        bytes[bytes.lastIndex] = (bytes.last().toInt() xor 1).toByte()
        firstData.writeBytes(bytes)
        assertThrows(GeneralSecurityException::class.java) { store.readPage(source, 0, key) }
        assertEquals(page("two", otherTarget), store.readPage(other, 0, key))
        assertEquals(PhoneOfflineSave.SAVED, store.maintain(receiver, 1_000_000, 1 + PHONE_OFFLINE_EXPIRY_MS, key))
        assertEquals(listOf(other), store.list(receiver, key).copies.map { it.source })
    }

    @Test
    fun recentBackgroundRefreshCannotReplaceAPinnedDownload() {
        val store = PhoneOfflineStore(temporary.newFolder())
        val source = PhoneOfflineSource(receiver, target, "name", "agent", "project")
        val original = page("saved")
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(original), true, 1, 1, 1_000_000, key) { true })
        assertEquals(PhoneOfflineSave.KEEP_EXISTING,
            store.save(source, sequenceOf(page("recent")), false, null, 2, 1_000_000, key) { true })
        assertEquals(original, store.readPage(source, 0, key))
    }

    @Test
    fun observedRevocationFencesLateSavesAndLeavesOtherSourcesAndDraftsAlone() {
        val parent = temporary.newFolder()
        val root = java.io.File(parent, "downloads")
        val drafts = java.io.File(parent, "drafts").apply { writeText("detached") }
        val store = PhoneOfflineStore(root)
        val source = PhoneOfflineSource(receiver, target, "name", "agent", "project")
        val other = PhoneOfflineSource("bb".repeat(32), target, "other", "agent", "project")
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(page("one")), true, 1, 1, 1_000_000, key) { true })
        assertEquals(PhoneOfflineSave.SAVED, store.save(other, sequenceOf(page("two")), true, 1, 1, 1_000_000, key) { true })
        store.markRevoked(receiver)
        assertEquals(listOf(other), store.listAll(key).copies.map { it.source })
        assertEquals(PhoneOfflineSave.REVOKED,
            store.save(source, sequenceOf(page("late")), false, 2, 2, 1_000_000, key) { true })
        assertEquals("detached", drafts.readText())
        store.allowReceiver(receiver)
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(page("new grant")), false, 3, 3, 1_000_000, key) { true })
    }

    @Test
    fun losingReadScopeFencesOnlyTheAffectedSession() {
        val store = PhoneOfflineStore(temporary.newFolder())
        val source = PhoneOfflineSource(receiver, target, "one", "agent", "project")
        val otherTarget = PhoneControlTarget("11".repeat(16), "44".repeat(16))
        val other = PhoneOfflineSource(receiver, otherTarget, "two", "agent", "project")
        store.save(source, sequenceOf(page("one")), true, 1, 1, 1_000_000, key) { true }
        store.save(other, sequenceOf(page("two", otherTarget)), true, 1, 1, 1_000_000, key) { true }
        store.denySource(source)
        val oldFence = store.sourceFence(source)
        assertEquals(listOf(other), store.listAll(key).copies.map { it.source })
        assertEquals(PhoneOfflineSave.REVOKED, store.save(source, sequenceOf(page("late")), false, 2, 2, 1_000_000, key) { true })
        store.denySource(source)
        assertFalse(store.allowSource(source, oldFence))
        assertEquals(PhoneOfflineSave.REVOKED, store.save(source, sequenceOf(page("late")), false, 2, 2, 1_000_000, key) { true })
        assertTrue(store.allowSource(source, store.sourceFence(source)))
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(page("authorized")), false, 3, 3, 1_000_000, key) { true })
    }
}
