package dev.louiselm.capture

import java.io.File
import java.security.GeneralSecurityException
import android.security.keystore.KeyPermanentlyInvalidatedException
import javax.crypto.spec.SecretKeySpec
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneOfflineUnlockTest {
    @Test
    fun localDeadlinesRequireInteractionAndAreIndependentOfOnlineAuthority() {
        val period = PhoneOfflinePeriod(ByteArray(32) { 1 }, 1_000)
        assertNotNull(period.key(300_999))
        assertNull(period.key(301_000))
        assertNull(period.key(1_001))
        val active = PhoneOfflinePeriod(ByteArray(32) { 2 }, 0)
        repeat(13) { assertTrue(active.confirmInteraction(it * 299_999L)) }
        assertNotNull(active.key(3_599_999))
        assertFalse(active.confirmInteraction(3_600_000))
        assertNull(active.key(1))
        val locked = PhoneOfflinePeriod(ByteArray(32) { 3 }, 0)
        locked.lock()
        assertNull(locked.key(1))
    }

    @Test
    fun wrappedDownloadKeyCanBeUnlockedOfflineAndWrongKeysDoNotResetIt() {
        val context = RuntimeEnvironment.getApplication()
        val wrapping = SecretKeySpec(ByteArray(32) { 4 }, "AES")
        val wrong = SecretKeySpec(ByteArray(32) { 5 }, "AES")
        val directory = File(context.filesDir, "offline-key-test")
        val store = PhoneOfflineUnlockStore(context, directory) { wrapping }
        val creation = store.prepare()
        val created = store.finish(creation, creation.operation, 0)
        val original = requireNotNull(created.key(1)).encoded
        created.lock()
        val broken = PhoneOfflineUnlockStore(context, directory) { wrong }
        val failed = broken.prepare()
        assertThrows(GeneralSecurityException::class.java) { broken.finish(failed, failed.operation, 0) }
        val retry = store.prepare()
        assertTrue(original.contentEquals(requireNotNull(store.finish(retry, retry.operation, 2).key(3)).encoded))
    }

    @Test
    fun cancellationDoesNotCreateAUsableUnlock() {
        val context = RuntimeEnvironment.getApplication()
        val wrapping = SecretKeySpec(ByteArray(32) { 8 }, "AES")
        val store = PhoneOfflineUnlockStore(context, File(context.filesDir, "offline-cancel-test"), "offline-cancel-test") { wrapping }
        val prepared = store.prepare()
        prepared.discard()
        assertThrows(IllegalStateException::class.java) { store.finish(prepared, prepared.operation, 0) }
        assertEquals(false, store.hasWrappedKey())
    }
    @Test
    fun draftKeyIsIndependentOfDownloadKey() {
        val context = RuntimeEnvironment.getApplication()
        val downloads = PhoneOfflineUnlockStore(context, File(context.filesDir, "separate-downloads"), "separate-downloads") {
            SecretKeySpec(ByteArray(32) { 4 }, "AES")
        }
        val drafts = PhoneOfflineUnlockStore(context, File(context.filesDir, "separate-drafts"), "separate-drafts", draft = true) {
            SecretKeySpec(ByteArray(32) { 5 }, "AES")
        }
        val download = downloads.prepare().let { downloads.finish(it, it.operation, 0).key(1)!!.encoded }
        val draft = drafts.prepare().let { drafts.finish(it, it.operation, 0).key(1)!!.encoded }
        assertFalse(download.contentEquals(draft))
        assertTrue(downloads.hasWrappedKey())
        assertTrue(drafts.hasWrappedKey())
        val reopen = drafts.prepare()
        assertTrue(draft.contentEquals(drafts.finish(reopen, reopen.operation, 2).key(3)!!.encoded))
    }

    @Test
    fun provenDraftKeyLossRequiresExplicitResetAndFencesOldKeys() {
        val context = RuntimeEnvironment.getApplication()
        val root = File(context.filesDir, "draft-loss-test")
        val wrapping = SecretKeySpec(ByteArray(32) { 9 }, "AES")
        val original = PhoneOfflineUnlockStore(context, root, "draft-loss-test", draft = true) { wrapping }
        val created = original.prepare()
        val oldKey = requireNotNull(original.finish(created, created.operation, 0).key(1))
        val source = PhoneOfflineSource("aa".repeat(32), PhoneControlTarget("11".repeat(16), "22".repeat(16)), "session", "agent", "project")
        val store = PhoneDraftStore(root)
        assertEquals(PhoneDraftSave.SAVED, store.save(PhoneDraft(source, "unrecoverable", emptyList()), oldKey) { true })
        var removed = false
        val lost = PhoneOfflineUnlockStore(context, root, "draft-loss-test", draft = true,
            removeWrappingKey = { removed = true }) { throw KeyPermanentlyInvalidatedException() }
        assertThrows(IllegalStateException::class.java) { lost.resetLostDraftKey() }
        assertThrows(KeyPermanentlyInvalidatedException::class.java) { lost.prepare() }
        assertTrue(store.list(oldKey).drafts.isNotEmpty())
        lost.resetLostDraftKey()
        assertTrue(removed)
        assertFalse(lost.hasWrappedKey())
        assertEquals(PhoneDraftSave.DAMAGED, store.save(PhoneDraft(source, "late stale edit", emptyList()), oldKey) { true })
        assertTrue(store.list(oldKey).drafts.isEmpty())
        val replacement = original.prepare()
        val newKey = requireNotNull(original.finish(replacement, replacement.operation, 2).key(3))
        assertEquals(PhoneDraftSave.SAVED, store.save(PhoneDraft(source, "new work", emptyList()), newKey) { true })
    }

    @Test
    fun temporaryDraftKeyFailurePreservesDataForRetry() {
        val context = RuntimeEnvironment.getApplication()
        val root = File(context.filesDir, "draft-retry-test")
        val wrapping = SecretKeySpec(ByteArray(32) { 6 }, "AES")
        val store = PhoneOfflineUnlockStore(context, root, "draft-retry-test", draft = true) { wrapping }
        val prepared = store.prepare()
        val original = requireNotNull(store.finish(prepared, prepared.operation, 0).key(1)).encoded
        val temporary = PhoneOfflineUnlockStore(context, root, "draft-retry-test", draft = true) {
            throw GeneralSecurityException("temporary")
        }
        assertThrows(GeneralSecurityException::class.java) { temporary.prepare() }
        assertFalse(temporary.hasProvenDraftKeyLoss())
        assertThrows(IllegalStateException::class.java) { temporary.resetLostDraftKey() }
        val retry = store.prepare()
        assertTrue(original.contentEquals(requireNotNull(store.finish(retry, retry.operation, 2).key(3)).encoded))
    }

}
