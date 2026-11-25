package dev.louiselm.capture

import android.content.Context
import java.io.File
import java.io.IOException
import java.security.MessageDigest
import java.security.Signature
import javax.crypto.spec.SecretKeySpec
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], shadows = [PhoneControlPairingFixture::class], instrumentedPackages = ["dev.louiselm.capture"])
class PhoneControlSendClientTest {
    @Test
    fun sendPersistsSignedIdentityBeforeTransportAndKeepsUnsupportedUncertain() {
        val context = RuntimeEnvironment.getApplication()
        val snapshot = File(context.noBackupFilesDir, "phone-action-pending-v1")
        assertTrue(!snapshot.exists() || snapshot.delete())
        context.getSharedPreferences("phone-control-v1", Context.MODE_PRIVATE).edit().clear().commit()
        context.getSharedPreferences("control-pairing-fixture", Context.MODE_PRIVATE).edit().clear().commit()
        val enrolled = phoneControlWorkingKey()
        val working = phoneControlWorkingKey()
        val keys = object : PhoneControlKeyAccess {
            override fun enrollmentPublicKey() = phoneControlPublicKey(enrolled.public)
            override fun publicKey() = phoneControlPublicKey(enrolled.public)
            override fun signingOperation() = Signature.getInstance("SHA256withECDSA").apply { initSign(enrolled.private) }
        }
        val config = requireNotNull(PairingStore(context).load())
        val grant = PhoneControlGrant("11".repeat(16), 1, "aa".repeat(32), phoneControlPairingBinding(config),
            phoneControlHex(MessageDigest.getInstance("SHA-256").digest(keys.publicKey())))
        PhoneControlStore(context).saveGrant(grant)
        val period = PhoneControlPeriod(grant, "22".repeat(16), working.private, 0)
        val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        var initialized = false
        val store = PendingPhoneActionStore(context, { create ->
            if (create) initialized = true
            if (initialized) secret else null
        })
        val target = PhoneControlTarget("33".repeat(16), "44".repeat(16))
        var calls = 0
        val client = PhoneControlClient(context, keys) { _, path, body, maySend ->
            calls++
            assertTrue(maySend())
            assertEquals("request", path)
            val wire = body.getJSONObject("request")
            assertEquals(3, wire.getInt("action"))
            assertEquals(1, wire.getInt("interaction"))
            assertEquals(7L, wire.getLong("expected_revision"))
            assertEquals(phoneControlHex(phoneControlSendPayload("reviewed text")), wire.getString("payload"))
            val retained = store.pending().single()
            assertEquals(wire.getString("operation_id"), retained.operationId)
            assertEquals(wire.getString("payload_hash"), retained.payloadHash)
            val request = PhoneControlRequest(grant.grantId, grant.revision, period.periodId, grant.receiverIdentity,
                target.controllerId, target.subjectId, phoneControlHex(wire.getString("payload")), retained.operationId, 7, true, 3)
            assertTrue(Signature.getInstance("SHA256withECDSA").run {
                initVerify(working.public); update(request.transcript()); verify(phoneControlHex(wire.getString("signature")))
            })
            parsePhoneControlReply("""{"v":1,"type":"error","code":"unsupported"}""".toByteArray(Charsets.UTF_8))
        }
        val attempt = client.sendPrompt(period, target, 7, "reviewed text", store, 100) { true }
        assertTrue(attempt is PhoneControlResult.Value)
        val sent = (attempt as PhoneControlResult.Value).value
        assertEquals(PhoneControlOutcome.UNSUPPORTED, sent.outcome)
        assertEquals(store.pending().single(), sent.pending)
        assertEquals(1, calls)
        period.lock()
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED),
            client.sendPrompt(period, target, 7, "reviewed text", store, 100) { true })
        assertEquals(1, calls)
    }

    @Test
    fun failedReservationNeverTransmits() {
        val context = RuntimeEnvironment.getApplication()
        val snapshot = File(context.noBackupFilesDir, "phone-action-pending-v1")
        assertTrue(!snapshot.exists() || snapshot.delete())
        context.getSharedPreferences("phone-control-v1", Context.MODE_PRIVATE).edit().clear().commit()
        context.getSharedPreferences("control-pairing-fixture", Context.MODE_PRIVATE).edit().clear().commit()
        val key = phoneControlWorkingKey()
        val keys = object : PhoneControlKeyAccess {
            override fun enrollmentPublicKey() = phoneControlPublicKey(key.public)
            override fun publicKey() = phoneControlPublicKey(key.public)
            override fun signingOperation() = Signature.getInstance("SHA256withECDSA").apply { initSign(key.private) }
        }
        val config = requireNotNull(PairingStore(context).load())
        val grant = PhoneControlGrant("11".repeat(16), 1, "aa".repeat(32), phoneControlPairingBinding(config),
            phoneControlHex(MessageDigest.getInstance("SHA-256").digest(keys.publicKey())))
        PhoneControlStore(context).saveGrant(grant)
        val period = PhoneControlPeriod(grant, "22".repeat(16), key.private, 0)
        val store = PendingPhoneActionStore(context, { SecretKeySpec(ByteArray(32), "AES") },
            syncFile = { throw IOException("failed") })
        var calls = 0
        val client = PhoneControlClient(context, keys) { _, _, _, _ -> calls++; PhoneControlReply.Failure(PhoneControlOutcome.UNAVAILABLE) }
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE),
            client.sendPrompt(period, PhoneControlTarget("33".repeat(16), "44".repeat(16)), 7, "text", store, 100) { true })
        assertEquals(0, calls)
    }

    @Test
    fun lockDuringReservationKeepsIdentityAndPreventsTransmission() {
        val context = RuntimeEnvironment.getApplication()
        val snapshot = File(context.noBackupFilesDir, "phone-action-pending-v1")
        assertTrue(!snapshot.exists() || snapshot.delete())
        context.getSharedPreferences("phone-control-v1", Context.MODE_PRIVATE).edit().clear().commit()
        context.getSharedPreferences("control-pairing-fixture", Context.MODE_PRIVATE).edit().clear().commit()
        val key = phoneControlWorkingKey()
        val keys = object : PhoneControlKeyAccess {
            override fun enrollmentPublicKey() = phoneControlPublicKey(key.public)
            override fun publicKey() = phoneControlPublicKey(key.public)
            override fun signingOperation() = Signature.getInstance("SHA256withECDSA").apply { initSign(key.private) }
        }
        val config = requireNotNull(PairingStore(context).load())
        val grant = PhoneControlGrant("11".repeat(16), 1, "aa".repeat(32), phoneControlPairingBinding(config),
            phoneControlHex(MessageDigest.getInstance("SHA-256").digest(keys.publicKey())))
        PhoneControlStore(context).saveGrant(grant)
        val period = PhoneControlPeriod(grant, "22".repeat(16), key.private, 0)
        val secret = SecretKeySpec(ByteArray(32), "AES")
        var initialized = false
        val store = PendingPhoneActionStore(context, { create ->
            if (create) initialized = true
            if (initialized) secret else null
        }, syncFile = { descriptor -> descriptor.sync(); period.lock() })
        var calls = 0
        val client = PhoneControlClient(context, keys) { _, _, _, _ -> calls++; PhoneControlReply.Failure(PhoneControlOutcome.UNAVAILABLE) }
        val result = client.sendPrompt(period, PhoneControlTarget("33".repeat(16), "44".repeat(16)),
            7, "text", store, 100) { true }
        assertTrue(result is PhoneControlResult.Value)
        assertEquals(PhoneControlOutcome.UNAVAILABLE, (result as PhoneControlResult.Value).value.outcome)
        assertEquals(1, store.pending().size)
        assertEquals(0, calls)
    }
}
