package dev.louiselm.capture

import android.content.Context
import java.security.MessageDigest
import java.security.Signature
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], shadows = [PhoneControlPairingFixture::class], instrumentedPackages = ["dev.louiselm.capture"])
class PhoneControlPendingHistoryTest {
    @Test
    fun exactHistoryUsesOriginalSubjectWithoutRenewingAndRejectsOtherEvidence() {
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
        val grant = PhoneControlGrant("11".repeat(16), 1, "aa".repeat(32), phoneControlPairingBinding(config),
            phoneControlHex(MessageDigest.getInstance("SHA-256").digest(keys.publicKey())))
        PhoneControlStore(context).saveGrant(grant)
        val period = PhoneControlPeriod(grant, "22".repeat(16), working.private, 0)
        val pending = PendingPhoneAction("33".repeat(16), grant.receiverIdentity, "44".repeat(16), "55".repeat(16),
            3, "66".repeat(32), grant.grantId, grant.revision, period.periodId, 9)
        var record = PhoneActionRecord(pending.operationId, pending.receiverIdentity, pending.controllerId,
            pending.subjectId, pending.action, pending.payloadHash, pending.grantId, pending.grantRevision,
            pending.expectedRevision, 100, 101, PhoneActionState.FENCED_NO_EFFECT, "77".repeat(32), "", "", "")
        var permitted = true
        var calls = 0
        val client = PhoneControlClient(context, keys) { _, path, body, maySend ->
            calls++
            assertTrue(maySend())
            assertEquals("request", path)
            val wire = body.getJSONObject("request")
            assertEquals(2, wire.getInt("action"))
            assertEquals(0, wire.getInt("interaction"))
            assertEquals(pending.controllerId, wire.getString("controller_id"))
            assertEquals(pending.subjectId, wire.getString("subject_id"))
            assertEquals(phoneControlHex(phoneControlHistoryRecordPayload(pending.operationId)), wire.getString("payload"))
            val request = PhoneControlRequest(grant.grantId, grant.revision, period.periodId, grant.receiverIdentity,
                wire.getString("controller_id"), wire.getString("subject_id"), phoneControlHex(wire.getString("payload")),
                wire.getString("operation_id"), 0, false, action = 2)
            assertTrue(Signature.getInstance("SHA256withECDSA").run {
                initVerify(working.public); update(request.transcript()); verify(phoneControlHex(wire.getString("signature")))
            })
            PhoneControlReply.ActionHistory(request.operationId, record)
        }
        val remaining = period.remainingMs(100)
        assertEquals(PhoneControlResult.Value(record), client.readActionHistory(period, pending, 100) { permitted })
        assertEquals(remaining, period.remainingMs(100))
        record = record.copy(payloadHash = "88".repeat(32))
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.readActionHistory(period, pending, 100) { permitted })
        permitted = false
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE), client.readActionHistory(period, pending, 100) { permitted })
        assertEquals(2, calls)
        period.lock()
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED), client.readActionHistory(period, pending, 100) { true })
    }
}
