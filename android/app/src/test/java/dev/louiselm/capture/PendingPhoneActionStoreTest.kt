package dev.louiselm.capture

import android.content.Context
import java.io.File
import java.io.IOException
import java.util.concurrent.Executors
import javax.crypto.Cipher
import javax.crypto.SecretKey
import javax.crypto.spec.SecretKeySpec
import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PendingPhoneActionStoreTest {
    private val context = RuntimeEnvironment.getApplication()
    private val snapshot get() = File(context.noBackupFilesDir, "phone-action-pending-v1")
    private val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
    private var created = false
    private val keys: (Boolean) -> SecretKey? = { create ->
        if (create) created = true
        if (created) secret else null
    }

    @Before
    fun clear() {
        assertTrue(!snapshot.exists() || snapshot.delete())
        context.getSharedPreferences("phone-control-v1", Context.MODE_PRIVATE).edit().clear().commit()
        created = false
    }

    private fun action(id: Int) = PendingPhoneAction(
        operationId = "%02x".format(id).repeat(16), receiverIdentity = "02".repeat(32),
        controllerId = "03".repeat(16), subjectId = "04".repeat(16), action = 3,
        payloadHash = "05".repeat(32), grantId = "06".repeat(16), grantRevision = 1,
        periodId = "07".repeat(16), expectedRevision = 2,
    )

    @Test
    fun encryptedIdentitySurvivesRestartAndGrantClearing() {
        val first = PendingPhoneActionStore(context, keys)
        assertEquals(PendingReservation.NEW, first.reserve(action(1)))
        assertEquals(PendingReservation.EXISTING, first.reserve(action(1)))
        assertThrows(PendingPhoneActionStoreException::class.java) {
            first.reserve(action(1).copy(payloadHash = "09".repeat(32)))
        }.also { assertEquals(PendingPhoneActionFailure.CONFLICT, it.reason) }
        PhoneControlStore(context).clear()
        val reopened = PendingPhoneActionStore(context, keys)
        assertEquals(listOf(action(1)), reopened.pending())
        val saved = snapshot.readBytes().toString(Charsets.ISO_8859_1)
        assertFalse(saved.contains(action(1).operationId))
        assertFalse(saved.contains(action(1).payloadHash))
    }

    @Test
    fun damagedOrMissingEvidenceNeverBecomesAnEmptyStore() {
        val store = PendingPhoneActionStore(context, keys)
        store.reserve(action(1))
        snapshot.writeText("damaged")
        assertThrows(PendingPhoneActionStoreException::class.java) { store.pending() }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
        assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(action(2)) }
        assertEquals("damaged", snapshot.readText())
        assertTrue(snapshot.delete())
        assertThrows(PendingPhoneActionStoreException::class.java) { PendingPhoneActionStore(context, keys).reserve(action(2)) }
        assertFalse(snapshot.exists())
    }

    @Test
    fun concurrentStoreInstancesPreserveEveryReservation() {
        val pool = Executors.newFixedThreadPool(4)
        try {
            val writes = (1..12).map { id -> pool.submit<PendingReservation> { PendingPhoneActionStore(context, keys).reserve(action(id)) } }
            assertTrue(writes.all { it.get() == PendingReservation.NEW })
            assertEquals((1..12).map(::action).toSet(), PendingPhoneActionStore(context, keys).pending().toSet())
        } finally {
            pool.shutdownNow()
        }
    }

    @Test
    fun failedPublicationNeverReturnsAReservation() {
        PendingPhoneActionStore(context, keys).reserve(action(1))
        val prior = snapshot.readBytes().toList()
        val store = PendingPhoneActionStore(context, keys, syncFile = { throw IOException("injected sync failure") })
        assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(action(2)) }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
        assertEquals(prior, snapshot.readBytes().toList())
        assertEquals(listOf(action(1)), PendingPhoneActionStore(context, keys).pending())
    }

    @Test
    fun failedFirstPublicationDoesNotBecomeAnEmptyInitializedStore() {
        val store = PendingPhoneActionStore(context, keys, syncFile = { throw IOException("injected sync failure") })
        assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(action(1)) }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
        assertFalse(snapshot.exists())
        assertThrows(PendingPhoneActionStoreException::class.java) { PendingPhoneActionStore(context, keys).reserve(action(2)) }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
    }

    @Test
    fun failedPublicationIsUnavailableToOtherInstancesUntilConfirmed() {
        PendingPhoneActionStore(context, keys).reserve(action(1))
        var calls = 0
        var failAfterRename = true
        val syncDirectory: (File) -> Unit = { directory ->
            calls += 1
            if (failAfterRename && calls >= 2) throw IOException("injected directory sync failure")
            syncPendingDirectory(directory)
        }
        assertThrows(PendingPhoneActionStoreException::class.java) {
            PendingPhoneActionStore(context, keys, syncDirectory = syncDirectory).reserve(action(2))
        }.also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
        val other = PendingPhoneActionStore(context, keys, syncDirectory = syncDirectory)
        assertThrows(PendingPhoneActionStoreException::class.java) { other.pending() }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
        assertThrows(PendingPhoneActionStoreException::class.java) { other.reserve(action(2)) }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }

        failAfterRename = false
        assertEquals(listOf(action(1), action(2)), PendingPhoneActionStore(context, keys, syncDirectory = syncDirectory).pending())
        assertEquals(PendingReservation.EXISTING, PendingPhoneActionStore(context, keys).reserve(action(2)))
    }

    @Test
    fun fullStoreRefusesAnotherActionWithoutEviction() {
        val records = JSONArray()
        for (id in 0 until 4096) {
            records.put(JSONObject().put("operation_id", "%032x".format(id)).put("receiver_identity", "02".repeat(32))
                .put("controller_id", "03".repeat(16)).put("subject_id", "04".repeat(16)).put("action", 3)
                .put("payload_hash", "05".repeat(32)).put("grant_id", "06".repeat(16)).put("grant_revision", 1)
                .put("period_id", "07".repeat(16)).put("expected_revision", 2))
        }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply {
            init(Cipher.ENCRYPT_MODE, secret)
            updateAAD("louiselm-phone-action-pending-v1".toByteArray(Charsets.UTF_8))
        }
        val plain = JSONObject().put("v", 1).put("actions", records).toString().toByteArray(Charsets.UTF_8)
        val encrypted = cipher.iv + cipher.doFinal(plain)
        snapshot.writeBytes(encrypted)
        created = true
        val store = PendingPhoneActionStore(context, keys)
        assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(action(254)) }
            .also { assertEquals(PendingPhoneActionFailure.LIMIT, it.reason) }
        assertEquals(encrypted.toList(), snapshot.readBytes().toList())
        assertEquals(4096, store.pending().size)
    }
}
