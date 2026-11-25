package dev.louiselm.capture

import android.content.Context
import java.io.File
import java.io.IOException
import java.nio.file.Files
import java.security.MessageDigest
import java.util.concurrent.Executors
import javax.crypto.Cipher
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
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
    private val staging get() = File(context.noBackupFilesDir, ".phone-action-pending-v1.tmp")
    private val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
    private var created = false
    private val keys: (Boolean) -> SecretKey? = { create ->
        if (create) created = true
        if (created) secret else null
    }

    @Before
    fun clear() {
        assertTrue(!snapshot.exists() || snapshot.delete())
        Files.deleteIfExists(staging.toPath())
        context.getSharedPreferences("phone-control-v1", Context.MODE_PRIVATE).edit().clear().commit()
        created = false
    }

    private fun action(id: Int) = PendingPhoneAction(
        operationId = "%02x".format(id).repeat(16), receiverIdentity = "02".repeat(32),
        controllerId = "03".repeat(16), subjectId = "04".repeat(16), action = 3,
        payloadHash = phoneControlHex(MessageDigest.getInstance("SHA-256").digest(phoneControlSendPayload("test instruction"))),
        grantId = "06".repeat(16), grantRevision = 1,
        periodId = "07".repeat(16), expectedRevision = 2,
    )

    private fun request(
        id: Int,
        text: String = "test instruction",
        receiver: String = "02".repeat(32),
        controller: String = "03".repeat(16),
        subject: String = "04".repeat(16),
        grant: String = "06".repeat(16),
        grantRevision: Long = 1,
        period: String = "07".repeat(16),
        expectedRevision: Long = 2,
    ) = PhoneControlRequest(
        grantId = grant, grantRevision = grantRevision, periodId = period,
        receiverIdentity = receiver, controllerId = controller, subjectId = subject,
        payload = phoneControlSendPayload(text), operationId = "%02x".format(id).repeat(16),
        expectedRevision = expectedRevision, interaction = true, action = 3,
    )

    @Test
    fun requestReservationUsesExactBindingsWithoutRetainingPayload() {
        val payload = phoneControlSendPayload("sensitive test instruction")
        val expectedHash = phoneControlHex(MessageDigest.getInstance("SHA-256").digest(payload))
        val request = PhoneControlRequest(
            grantId = "06".repeat(16), grantRevision = 1, periodId = "07".repeat(16),
            receiverIdentity = "02".repeat(32), controllerId = "03".repeat(16), subjectId = "04".repeat(16),
            payload = payload, operationId = "01".repeat(16), expectedRevision = 2, interaction = true, action = 3,
        )
        payload.fill(0)
        val store = PendingPhoneActionStore(context, keys)
        assertEquals(PendingReservation.NEW, store.reserve(request))
        assertEquals(action(1).copy(payloadHash = expectedHash), store.pending().single())
        assertEquals(PendingReservation.EXISTING, store.reserve(request))

        val encrypted = snapshot.readBytes()
        val cipher = Cipher.getInstance("AES/GCM/NoPadding").apply {
            init(Cipher.DECRYPT_MODE, secret, GCMParameterSpec(128, encrypted.copyOfRange(0, 12)))
            updateAAD("louiselm-phone-action-pending-v1".toByteArray(Charsets.UTF_8))
        }
        val plaintext = cipher.doFinal(encrypted, 12, encrypted.size - 12).toString(Charsets.UTF_8)
        assertFalse(plaintext.contains("sensitive test instruction"))
        assertFalse(plaintext.contains(phoneControlHex(phoneControlSendPayload("sensitive test instruction"))))
    }

    @Test
    fun encryptedIdentitySurvivesRestartAndGrantClearing() {
        val first = PendingPhoneActionStore(context, keys)
        assertEquals(PendingReservation.NEW, first.reserve(request(1)))
        assertEquals(PendingReservation.EXISTING, first.reserve(request(1)))
        val conflicting = listOf(
            request(1, text = "different instruction"), request(1, receiver = "08".repeat(32)),
            request(1, controller = "08".repeat(16)), request(1, subject = "08".repeat(16)),
            request(1, grant = "08".repeat(16)), request(1, grantRevision = 2),
            request(1, period = "08".repeat(16)), request(1, expectedRevision = 3),
        )
        for (attempt in conflicting) {
            assertThrows(PendingPhoneActionStoreException::class.java) { first.reserve(attempt) }
                .also { assertEquals(PendingPhoneActionFailure.CONFLICT, it.reason) }
        }
        PhoneControlStore(context).clear()
        val reopened = PendingPhoneActionStore(context, keys)
        assertEquals(listOf(action(1)), reopened.pending())
        val saved = snapshot.readBytes().toString(Charsets.ISO_8859_1)
        assertFalse(saved.contains(action(1).operationId))
        assertFalse(saved.contains(action(1).payloadHash))
    }

    @Test
    fun readRequestsCannotBecomePendingWrites() {
        val read = PhoneControlRequest(
            grantId = "06".repeat(16), grantRevision = 1, periodId = "07".repeat(16),
            receiverIdentity = "02".repeat(32), controllerId = "03".repeat(16), subjectId = "04".repeat(16),
            payload = phoneControlReadPayload(0, 50), operationId = "01".repeat(16),
            expectedRevision = 0, interaction = true,
        )
        assertThrows(PendingPhoneActionStoreException::class.java) { PendingPhoneActionStore(context, keys).reserve(read) }
            .also { assertEquals(PendingPhoneActionFailure.MALFORMED, it.reason) }
        assertFalse(snapshot.exists())
    }

    @Test
    fun damagedOrMissingEvidenceNeverBecomesAnEmptyStore() {
        val store = PendingPhoneActionStore(context, keys)
        store.reserve(request(1))
        snapshot.writeText("damaged")
        assertThrows(PendingPhoneActionStoreException::class.java) { store.pending() }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
        assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(request(2)) }
        assertEquals("damaged", snapshot.readText())
        assertTrue(snapshot.delete())
        assertThrows(PendingPhoneActionStoreException::class.java) { PendingPhoneActionStore(context, keys).reserve(request(2)) }
        assertFalse(snapshot.exists())
    }

    @Test
    fun concurrentStoreInstancesPreserveEveryReservation() {
        val pool = Executors.newFixedThreadPool(4)
        try {
            val writes = (1..12).map { id -> pool.submit<PendingReservation> { PendingPhoneActionStore(context, keys).reserve(request(id)) } }
            assertTrue(writes.all { it.get() == PendingReservation.NEW })
            assertEquals((1..12).map(::action).toSet(), PendingPhoneActionStore(context, keys).pending().toSet())
        } finally {
            pool.shutdownNow()
        }
    }

    @Test
    fun concurrentReservationsOfOneRequestHaveOnlyOneNewResult() {
        val pool = Executors.newFixedThreadPool(4)
        try {
            val results = (1..12).map {
                pool.submit<PendingReservation> { PendingPhoneActionStore(context, keys).reserve(request(1)) }
            }.map { it.get() }
            assertEquals(1, results.count { it == PendingReservation.NEW })
            assertEquals(11, results.count { it == PendingReservation.EXISTING })
            assertEquals(listOf(action(1)), PendingPhoneActionStore(context, keys).pending())
        } finally {
            pool.shutdownNow()
        }
    }

    @Test
    fun failedPublicationNeverReturnsAReservation() {
        PendingPhoneActionStore(context, keys).reserve(request(1))
        val prior = snapshot.readBytes().toList()
        val store = PendingPhoneActionStore(context, keys, syncFile = { throw IOException("injected sync failure") })
        assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(request(2)) }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
        assertEquals(prior, snapshot.readBytes().toList())
        assertEquals(listOf(action(1)), PendingPhoneActionStore(context, keys).pending())
    }

    @Test
    fun abandonedStagingIsReusedWithoutLosingPublishedEvidence() {
        val store = PendingPhoneActionStore(context, keys)
        store.reserve(request(1))
        staging.writeText("incomplete encrypted snapshot")
        assertEquals(PendingReservation.NEW, store.reserve(request(2)))
        assertFalse(staging.exists())
        assertEquals(listOf(action(1), action(2)), PendingPhoneActionStore(context, keys).pending())
    }

    @Test
    fun unexpectedStagingSymlinkIsRefusedWithoutChangingPublishedEvidence() {
        val store = PendingPhoneActionStore(context, keys)
        store.reserve(request(1))
        val prior = snapshot.readBytes().toList()
        Files.createSymbolicLink(staging.toPath(), snapshot.toPath())
        try {
            assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(request(2)) }
                .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
            assertEquals(prior, snapshot.readBytes().toList())
            assertEquals(listOf(action(1)), PendingPhoneActionStore(context, keys).pending())
        } finally {
            Files.deleteIfExists(staging.toPath())
        }
    }

    @Test
    fun failedFirstPublicationDoesNotBecomeAnEmptyInitializedStore() {
        val store = PendingPhoneActionStore(context, keys, syncFile = { throw IOException("injected sync failure") })
        assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(request(1)) }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
        assertFalse(snapshot.exists())
        assertThrows(PendingPhoneActionStoreException::class.java) { PendingPhoneActionStore(context, keys).reserve(request(2)) }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
    }

    @Test
    fun failedPublicationIsUnavailableToOtherInstancesUntilConfirmed() {
        PendingPhoneActionStore(context, keys).reserve(request(1))
        var calls = 0
        var failAfterRename = true
        val syncDirectory: (File) -> Unit = { directory ->
            calls += 1
            if (failAfterRename && calls >= 2) throw IOException("injected directory sync failure")
            syncPendingDirectory(directory)
        }
        assertThrows(PendingPhoneActionStoreException::class.java) {
            PendingPhoneActionStore(context, keys, syncDirectory = syncDirectory).reserve(request(2))
        }.also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
        val other = PendingPhoneActionStore(context, keys, syncDirectory = syncDirectory)
        assertThrows(PendingPhoneActionStoreException::class.java) { other.pending() }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }
        assertThrows(PendingPhoneActionStoreException::class.java) { other.reserve(request(2)) }
            .also { assertEquals(PendingPhoneActionFailure.UNAVAILABLE, it.reason) }

        failAfterRename = false
        assertEquals(listOf(action(1), action(2)), PendingPhoneActionStore(context, keys, syncDirectory = syncDirectory).pending())
        assertEquals(PendingReservation.EXISTING, PendingPhoneActionStore(context, keys).reserve(request(2)))
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
        assertThrows(PendingPhoneActionStoreException::class.java) { store.reserve(request(254)) }
            .also { assertEquals(PendingPhoneActionFailure.LIMIT, it.reason) }
        assertEquals(encrypted.toList(), snapshot.readBytes().toList())
        assertEquals(4096, store.pending().size)
    }
}
