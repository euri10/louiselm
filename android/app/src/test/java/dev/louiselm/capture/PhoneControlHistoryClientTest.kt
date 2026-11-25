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
class PhoneControlHistoryClientTest {
    @Test
    fun signedHistoryIsCorrelatedAndNeverRenewsTheWorkingPeriod() {
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
        var mismatch = false
        var allowed = true
        var calls = 0
        val transport: PhoneControlTransport = { _, path, body, maySend ->
            calls++
            assertTrue(maySend())
            assertEquals("request", path)
            val wire = body.getJSONObject("request")
            assertEquals(2, wire.getInt("action"))
            assertEquals(0, wire.getInt("interaction"))
            assertEquals("00".repeat(16), wire.getString("controller_id"))
            assertEquals("00".repeat(16), wire.getString("subject_id"))
            val request = PhoneControlRequest(grant.grantId, grant.revision, period.periodId, grant.receiverIdentity,
                wire.getString("controller_id"), wire.getString("subject_id"), phoneControlHex(wire.getString("payload")),
                wire.getString("operation_id"), wire.getLong("expected_revision"), false, action = 2)
            assertTrue(Signature.getInstance("SHA256withECDSA").run {
                initVerify(working.public); update(request.transcript()); verify(phoneControlHex(wire.getString("signature")))
            })
            PhoneControlReply.ActionHistoryPage(if (mismatch) "ff".repeat(16) else request.operationId, emptyList(), "")
        }
        val client = PhoneControlClient(context, keys, transport)
        val remaining = period.remainingMs(100)
        assertTrue(client.readActionHistoryPage(period, 100) { allowed } is PhoneControlResult.Value)
        assertEquals(remaining, period.remainingMs(100))
        mismatch = true
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.readActionHistoryPage(period, 100) { allowed })
        allowed = false
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE), client.readActionHistoryPage(period, 100) { allowed })
        assertEquals(2, calls)
        period.lock()
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED), client.readActionHistoryPage(period, 100) { true })
    }
}
