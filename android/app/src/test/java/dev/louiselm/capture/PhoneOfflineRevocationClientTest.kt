package dev.louiselm.capture

import android.content.Context
import android.os.SystemClock
import java.io.File
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
class PhoneOfflineRevocationClientTest {
    @Test
    fun receiverChangeRemovesOldCopiesButSameIdentityEndpointMoveDoesNot() {
        val context = RuntimeEnvironment.getApplication()
        context.getSharedPreferences("phone-control-v1", Context.MODE_PRIVATE).edit().clear().commit()
        val pairing = context.getSharedPreferences("control-pairing-fixture", Context.MODE_PRIVATE)
        pairing.edit().clear().commit()
        val enrolled = phoneControlWorkingKey()
        val keys = object : PhoneControlKeyAccess {
            override fun enrollmentPublicKey() = phoneControlPublicKey(enrolled.public)
            override fun publicKey() = phoneControlPublicKey(enrolled.public)
            override fun signingOperation() = Signature.getInstance("SHA256withECDSA").apply { initSign(enrolled.private) }
        }
        val config = requireNotNull(PairingStore(context).load())
        val grant = PhoneControlGrant("11".repeat(16), 1, config.receiverIdentitySha256, phoneControlPairingBinding(config),
            phoneControlHex(MessageDigest.getInstance("SHA-256").digest(keys.publicKey())))
        PhoneControlStore(context).saveGrant(grant)
        val target = PhoneControlTarget("22".repeat(16), "33".repeat(16))
        val source = PhoneOfflineSource(grant.receiverIdentity, target, "name", "agent", "project")
        val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        val store = PhoneOfflineStore(File(context.filesDir, "phone-offline-v1"))
        val page = PhoneControlReply.SessionPage(target.controllerId, target.subjectId, "44".repeat(16), 1, "ready",
            listOf(PhoneControlEntry.Message(1, "assistant", "protected", false)), 0, true,
            PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false))
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(page), true, 1, 1, 1_000_000, key) { true })
        val client = PhoneControlClient(context, keys) { _, _, _, _ -> PhoneControlReply.Failure(PhoneControlOutcome.UNAVAILABLE) }
        pairing.edit().putString("url", "https://192.0.2.2").commit()
        assertTrue((client.status() as PhoneControlResult.Value).value is PhoneControlStatus.Granted)
        assertEquals(page, store.readPage(source, 0, key))
        pairing.edit().putString("receiver", "bb".repeat(32)).commit()
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.REVOKED), client.status())
        assertTrue(store.listAll(key).copies.isEmpty())
        assertEquals(PhoneOfflineSave.REVOKED, store.save(source, sequenceOf(page), true, 2, 2, 1_000_000, key) { true })
    }

    @Test
    fun lostReadScopeFencesOnlyThatSessionUntilAnExplicitAuthorizedRead() {
        val context = RuntimeEnvironment.getApplication()
        context.getSharedPreferences("phone-control-v1", Context.MODE_PRIVATE).edit().clear().commit()
        context.getSharedPreferences("control-pairing-fixture", Context.MODE_PRIVATE).edit().clear().commit()
        val enrolled = phoneControlWorkingKey()
        val keys = object : PhoneControlKeyAccess {
            override fun enrollmentPublicKey() = phoneControlPublicKey(enrolled.public)
            override fun publicKey() = phoneControlPublicKey(enrolled.public)
            override fun signingOperation() = Signature.getInstance("SHA256withECDSA").apply { initSign(enrolled.private) }
        }
        val config = requireNotNull(PairingStore(context).load())
        val grant = PhoneControlGrant("11".repeat(16), 1, config.receiverIdentitySha256, phoneControlPairingBinding(config),
            phoneControlHex(MessageDigest.getInstance("SHA-256").digest(keys.publicKey())))
        PhoneControlStore(context).saveGrant(grant)
        val target = PhoneControlTarget("22".repeat(16), "33".repeat(16))
        val other = PhoneControlTarget(target.controllerId, "44".repeat(16))
        val source = PhoneOfflineSource(grant.receiverIdentity, target, "name", "agent", "project")
        val otherSource = source.copy(target = other)
        val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        val store = PhoneOfflineStore(File(context.filesDir, "phone-offline-v1"))
        fun page(forTarget: PhoneControlTarget, operation: String) = PhoneControlReply.SessionPage(
            forTarget.controllerId, forTarget.subjectId, operation, 1, "ready",
            listOf(PhoneControlEntry.Message(1, "assistant", "protected", false)), 0, true,
            PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false))
        val original = page(target, "44".repeat(16))
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(original), true, 1, 1, 1_000_000, key) { true })
        assertEquals(PhoneOfflineSave.SAVED, store.save(otherSource, sequenceOf(page(other, "44".repeat(16))), true, 1, 1, 1_000_000, key) { true })
        var denied = true
        val client = PhoneControlClient(context, keys) { _, _, body, _ ->
            if (denied) PhoneControlReply.Failure(PhoneControlOutcome.OUT_OF_SCOPE)
            else page(target, body.getJSONObject("request").getString("operation_id"))
        }
        val started = SystemClock.elapsedRealtime()
        val period = PhoneControlPeriod(grant, "55".repeat(16), phoneControlWorkingKey().private, started)
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.OUT_OF_SCOPE), client.readSession(period, target, started) { true })
        assertEquals(listOf(otherSource), store.listAll(key).copies.map { it.source })
        assertEquals(PhoneOfflineSave.REVOKED, store.save(source, sequenceOf(original), true, 2, 2, 1_000_000, key) { true })
        denied = false
        assertTrue(client.readSession(period, target, started, interaction = false) { true } is PhoneControlResult.Value)
        assertEquals(PhoneOfflineSave.REVOKED, store.save(source, sequenceOf(original), true, 2, 2, 1_000_000, key) { true })
        assertTrue(client.readSession(period, target, started) { true } is PhoneControlResult.Value)
        assertEquals(PhoneOfflineSave.REVOKED, store.save(source, sequenceOf(original), true, 2, 2, 1_000_000, key) { true })
        val saved = downloadPhoneOffline(store, source, key, 1_000_000, 2, true, { true }, {}) { cursor, revision, interaction ->
            client.readSession(period, target, started, before = cursor, expectedRevision = revision, interaction = interaction) { true }
        }
        assertEquals(PhoneControlResult.Value(PhoneOfflineDownload(PhoneOfflineSave.SAVED, null)), saved)
    }

    @Test
    fun authenticatedRevocationRemovesPinsButNetworkFailureDoesNot() {
        val context = RuntimeEnvironment.getApplication()
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
        val grant = PhoneControlGrant("11".repeat(16), 1, config.receiverIdentitySha256, phoneControlPairingBinding(config),
            phoneControlHex(MessageDigest.getInstance("SHA-256").digest(keys.publicKey())))
        PhoneControlStore(context).saveGrant(grant)
        val target = PhoneControlTarget("22".repeat(16), "33".repeat(16))
        val source = PhoneOfflineSource(grant.receiverIdentity, target, "name", "agent", "project")
        val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        val store = PhoneOfflineStore(File(context.filesDir, "phone-offline-v1"))
        val page = PhoneControlReply.SessionPage(target.controllerId, target.subjectId, "44".repeat(16), 1, "ready",
            listOf(PhoneControlEntry.Message(1, "assistant", "protected", false)), 0, true,
            PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false))
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(page), true, 1, 1, 1_000_000, key) { true })
        val draft = File(context.filesDir, "detached-draft-fixture").apply { writeText("draft") }
        var outcome = PhoneControlOutcome.UNAVAILABLE
        val client = PhoneControlClient(context, keys) { _, _, _, _ -> PhoneControlReply.Failure(outcome) }
        val started = SystemClock.elapsedRealtime()
        val period = PhoneControlPeriod(grant, "55".repeat(16), working.private, started)
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE), client.readSession(period, target, started) { true })
        assertEquals(page, store.readPage(source, 0, key))
        outcome = PhoneControlOutcome.REVOKED
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.REVOKED), client.readSession(period, target, started) { true })
        assertTrue(store.listAll(key).copies.isEmpty())
        assertEquals(PhoneOfflineSave.REVOKED, store.save(source, sequenceOf(page), false, 2, 2, 1_000_000, key) { true })
        assertEquals("draft", draft.readText())
    }
}
