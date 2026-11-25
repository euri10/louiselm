package dev.louiselm.capture

import android.content.Context
import android.content.ContextWrapper
import android.content.SharedPreferences
import android.util.Base64
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
    private val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
    private var created = false
    private val keys: (Boolean) -> SecretKey? = { create ->
        if (create) created = true
        if (created) secret else null
    }

    @Before
    fun clear() {
        context.getSharedPreferences("phone-action-pending-v1", Context.MODE_PRIVATE).edit().clear().commit()
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
        val saved = context.getSharedPreferences("phone-action-pending-v1", Context.MODE_PRIVATE).all.toString()
        assertFalse(saved.contains(action(1).operationId))
        assertFalse(saved.contains(action(1).payloadHash))
    }

    @Test
    fun damagedOrMissingEvidenceNeverBecomesAnEmptyStore() {
        val store = PendingPhoneActionStore(context, keys)
        store.reserve(action(1))
        val preferences = context.getSharedPreferences("phone-action-pending-v1", Context.MODE_PRIVATE)
        preferences.edit().putString("snapshot", "damaged").commit()
        assertThrows(PendingPhoneActionStoreException::class.java) { store.pending() }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
        assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(action(2)) }
        assertEquals("damaged", preferences.getString("snapshot", null))
        preferences.edit().remove("snapshot").commit()
        assertThrows(PendingPhoneActionStoreException::class.java) { PendingPhoneActionStore(context, keys).reserve(action(2)) }
        assertFalse(preferences.contains("snapshot"))
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
        val delegate = context.getSharedPreferences("phone-action-pending-v1", Context.MODE_PRIVATE)
        val failing = object : SharedPreferences by delegate {
            override fun edit(): SharedPreferences.Editor {
                val editor = delegate.edit()
                return object : SharedPreferences.Editor by editor {
                    override fun putString(key: String?, value: String?): SharedPreferences.Editor {
                        editor.putString(key, value)
                        return this
                    }
                    override fun commit(): Boolean = false
                }
            }
        }
        val wrapped = object : ContextWrapper(context) {
            override fun getApplicationContext(): Context = this
            override fun getSharedPreferences(name: String, mode: Int): SharedPreferences = failing
        }
        val store = PendingPhoneActionStore(wrapped, keys)
        assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(action(1)) }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
        assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(action(2)) }
        assertFalse(delegate.contains("snapshot"))
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
        val snapshot = Base64.encodeToString(cipher.iv + cipher.doFinal(plain), Base64.NO_WRAP)
        val preferences = context.getSharedPreferences("phone-action-pending-v1", Context.MODE_PRIVATE)
        preferences.edit().putString("snapshot", snapshot).commit()
        created = true
        val store = PendingPhoneActionStore(context, keys)
        assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(action(254)) }
            .also { assertEquals(PendingPhoneActionFailure.LIMIT, it.reason) }
        assertEquals(snapshot, preferences.getString("snapshot", null))
        assertEquals(4096, store.pending().size)
    }
}
