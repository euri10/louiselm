package dev.louiselm.capture

import android.content.Context
import java.io.File
import java.security.MessageDigest
import java.security.Signature
import javax.crypto.spec.SecretKeySpec
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], shadows = [PhoneControlPairingFixture::class], instrumentedPackages = ["dev.louiselm.capture"])
class PhoneSendAvailabilityTest {
    @Test
    fun unlockedReplyDistinguishesUnavailableUnknownAndMalformedSupport() {
        val base = """{"v":1,"type":"unlocked","grant_id":"${"11".repeat(16)}","grant_revision":1,"period_id":"${"22".repeat(16)}","idle_expires_at_ms":1000,"absolute_expires_at_ms":2000}"""
        fun parse(value: String) = parsePhoneControlReply(value.toByteArray(Charsets.UTF_8))
        assertEquals(null, (parse(base) as PhoneControlReply.Unlocked).sendSupported)
        assertEquals(false, (parse(base.dropLast(1) + ",\"send_supported\":false}") as PhoneControlReply.Unlocked).sendSupported)
        assertEquals(true, (parse(base.dropLast(1) + ",\"send_supported\":true}") as PhoneControlReply.Unlocked).sendSupported)
        for (invalid in listOf("null", "1", "\"true\"", "{}")) {
            assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED),
                parse(base.dropLast(1) + ",\"send_supported\":$invalid}"))
        }
        assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED),
            parse(base.dropLast(1) + ",\"send_supported\":false,\"send_supported\":true}"))
    }

    @Test
    fun missingAvailabilityRefusesBeforeTouchingThePendingStoreOrTransport() {
        val context = RuntimeEnvironment.getApplication()
        val snapshot = File(context.noBackupFilesDir, "phone-action-pending-v1")
        assertTrue(!snapshot.exists() || snapshot.delete())
        context.getSharedPreferences("phone-control-v1", Context.MODE_PRIVATE).edit().clear().commit()
        val key = phoneControlWorkingKey()
        val keys = object : PhoneControlKeyAccess {
            override fun enrollmentPublicKey() = phoneControlPublicKey(key.public)
            override fun publicKey() = phoneControlPublicKey(key.public)
            override fun signingOperation() = Signature.getInstance("SHA256withECDSA").apply { initSign(key.private) }
        }
        val config = requireNotNull(PairingStore(context).load())
        val grant = PhoneControlGrant("11".repeat(16), 1, config.receiverIdentitySha256,
            phoneControlPairingBinding(config), phoneControlHex(MessageDigest.getInstance("SHA-256").digest(keys.publicKey())))
        PhoneControlStore(context).saveGrant(grant)
        val period = PhoneControlPeriod(grant, "22".repeat(16), key.private, 0)
        var keyReads = 0
        var keyCreated = false
        val pending = PendingPhoneActionStore(context, { create ->
            keyReads++
            if (create) keyCreated = true
            if (keyCreated) SecretKeySpec(ByteArray(32), "AES") else null
        })
        var calls = 0
        val client = PhoneControlClient(context, keys) { _, _, _, _ ->
            calls++
            PhoneControlReply.Failure(PhoneControlOutcome.UNSUPPORTED)
        }

        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE),
            client.sendPrompt(period, PhoneControlTarget("33".repeat(16), "44".repeat(16)),
                7, "full reviewed draft", pending, 100) { true })
        assertEquals(0, keyReads)
        assertEquals(0, calls)
        assertFalse(snapshot.exists())
    }

    @Test
    fun authenticatedUnlockRefusalLeavesExistingIdentityUntouchedAcrossStoreRestart() {
        val context = RuntimeEnvironment.getApplication()
        val snapshot = File(context.noBackupFilesDir, "phone-action-pending-v1")
        assertTrue(!snapshot.exists() || snapshot.delete())
        context.getSharedPreferences("phone-control-v1", Context.MODE_PRIVATE).edit().clear().commit()
        val enrolled = phoneControlWorkingKey()
        val keys = object : PhoneControlKeyAccess {
            override fun enrollmentPublicKey() = phoneControlPublicKey(enrolled.public)
            override fun publicKey() = phoneControlPublicKey(enrolled.public)
            override fun signingOperation() = Signature.getInstance("SHA256withECDSA").apply { initSign(enrolled.private) }
        }
        val config = requireNotNull(PairingStore(context).load())
        val grant = PhoneControlGrant("11".repeat(16), 1, config.receiverIdentitySha256,
            phoneControlPairingBinding(config), phoneControlHex(MessageDigest.getInstance("SHA-256").digest(keys.publicKey())))
        PhoneControlStore(context).saveGrant(grant)
        var actionCalls = 0
        var advertiseSupport = true
        val client = PhoneControlClient(context, keys) { _, path, _, _ ->
            when (path) {
                "challenge" -> PhoneControlReply.Challenge(grant.grantId, grant.revision, grant.receiverIdentity,
                    "33".repeat(16), "44".repeat(32), 1000)
                "unlock" -> {
                    val support = if (advertiseSupport) ",\"send_supported\":false" else ""
                    parsePhoneControlReply("""{"v":1,"type":"unlocked","grant_id":"${grant.grantId}","grant_revision":1,"period_id":"${"22".repeat(16)}","idle_expires_at_ms":1000,"absolute_expires_at_ms":2000$support}""".toByteArray())
                }
                else -> { actionCalls++; PhoneControlReply.Failure(PhoneControlOutcome.UNSUPPORTED) }
            }
        }
        val prepared = (client.prepareUnlock { true } as PhoneControlResult.Value).value
        prepared.signing.update(prepared.proof.transcript())
        val period = (client.finishUnlock(prepared, prepared.signing.sign(), 0) { true } as PhoneControlResult.Value).value
        val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        var keyCreated = false
        var keyReads = 0
        val keyAccess: (Boolean) -> javax.crypto.SecretKey? = { create ->
            keyReads++
            if (create) keyCreated = true
            if (keyCreated) secret else null
        }
        val store = PendingPhoneActionStore(context, keyAccess)
        val target = PhoneControlTarget("55".repeat(16), "66".repeat(16))
        val old = PhoneControlRequest(grant.grantId, grant.revision, period.periodId, grant.receiverIdentity,
            target.controllerId, target.subjectId, phoneControlSendPayload("old unknown"), "77".repeat(16), 7, true, 3)
        assertEquals(PendingReservation.NEW, store.reserve(old))
        val original = store.pending()
        val bytes = snapshot.readBytes().toList()
        val readsBeforeSend = keyReads
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNSUPPORTED),
            client.sendPrompt(period, target, 7, "full reviewed draft", store, 100) { true })
        assertEquals(readsBeforeSend, keyReads)
        assertEquals(0, actionCalls)
        assertEquals(bytes, snapshot.readBytes().toList())
        assertEquals(original, PendingPhoneActionStore(context, keyAccess).pending())
        assertEquals(299_899L, period.remainingMs(101))
        advertiseSupport = false
        val missing = (client.prepareUnlock { true } as PhoneControlResult.Value).value
        missing.signing.update(missing.proof.transcript())
        val missingPeriod = (client.finishUnlock(missing, missing.signing.sign(), 0) { true } as PhoneControlResult.Value).value
        val beforeMissing = keyReads
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE),
            client.sendPrompt(missingPeriod, target, 7, "full reviewed draft", store, 100) { true })
        assertEquals(beforeMissing, keyReads)
        assertEquals(bytes, snapshot.readBytes().toList())
        val supported = PhoneControlPeriod(grant, "99".repeat(16), enrolled.private, 0, sendSupported = true)
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE),
            client.sendPrompt(supported, target, 7, "full reviewed draft", store, 100) { false })
        supported.lock()
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED),
            client.sendPrompt(supported, target, 7, "full reviewed draft", store, 100) { true })
        assertEquals(0, actionCalls)
        assertEquals(original, PendingPhoneActionStore(context, keyAccess).pending())
    }
}
