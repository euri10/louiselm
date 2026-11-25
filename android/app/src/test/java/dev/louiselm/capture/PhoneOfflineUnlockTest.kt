package dev.louiselm.capture

import java.io.File
import java.security.GeneralSecurityException
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
}
