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
class PhoneRunClientTest {
    @Test
    fun signsAndCorrelatesRunReadsAndRejectsLateOrRevokedReplies() {
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
        val runId = "11111111-1111-4111-8111-111111111111"
        val summary = PhoneRunSummary(runId, 3, PhoneRunState.ACTIVE)
        val view = PhoneRunView(summary, true, PhoneRunLoad.UNSUPPORTED, PhoneRunWork(5, 2, 1, 2), 1, 0, false, false, 0)
        var mismatch = ""
        var mayDeliver = true
        val transport: PhoneControlTransport = { _, _, body, _ ->
            val wire = body.getJSONObject("request")
            val action = wire.getInt("action")
            val operation = wire.getString("operation_id")
            val request = PhoneControlRequest(grant.grantId, grant.revision, period.periodId, grant.receiverIdentity,
                wire.getString("controller_id"), wire.getString("subject_id"), phoneControlHex(wire.getString("payload")),
                operation, wire.getLong("expected_revision"), wire.getInt("interaction") == 1, action)
            assertTrue(Signature.getInstance("SHA256withECDSA").run {
                initVerify(working.public); update(request.transcript()); verify(phoneControlHex(wire.getString("signature")))
            })
            assertEquals("00".repeat(16), request.controllerId)
            if (mismatch == "late") mayDeliver = false
            if (mismatch == "grant") PhoneControlStore(context).saveGrant(grant.copy(grantId = "99".repeat(16)))
            if (mismatch == "unsupported") PhoneControlReply.Failure(PhoneControlOutcome.UNSUPPORTED)
            else if (action == 54) {
                val end = wire.getString("payload").startsWith("01") && mismatch != "cursor"
                PhoneControlReply.RunPage(if (mismatch == "operation") "44".repeat(16) else operation,
                    if (end) emptyList() else listOf(summary), if (end) "" else runId)
            } else {
                assertEquals(phoneControlHex(phoneControlRunIdBytes(runId)), request.subjectId)
                val changed = when (mismatch) {
                    "run" -> summary.copy(runId = "22222222-2222-4222-8222-222222222222")
                    "revision" -> summary.copy(revision = 4)
                    else -> summary
                }
                PhoneControlReply.RunDetail(if (mismatch == "operation") "44".repeat(16) else operation,
                    view.copy(summary = changed))
            }
        }
        val client = PhoneControlClient(context, keys, transport)
        assertTrue(client.listRuns(period, 1) { mayDeliver } is PhoneControlResult.Value)
        val end = client.listRuns(period, 1, after = runId) { mayDeliver } as PhoneControlResult.Value
        assertTrue(end.value.items.isEmpty())
        assertTrue(client.readRun(period, runId, 3, 1) { mayDeliver } is PhoneControlResult.Value)
        assertTrue(client.readRun(period, runId, 0, 1) { mayDeliver } is PhoneControlResult.Value)
        for (kind in listOf("operation", "run", "revision")) {
            mismatch = kind
            assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.readRun(period, runId, 3, 1) { mayDeliver })
        }
        mismatch = "cursor"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.listRuns(period, 1, after = runId) { mayDeliver })
        mismatch = "unsupported"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNSUPPORTED), client.listRuns(period, 1) { mayDeliver })
        mismatch = "grant"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.REVOKED), client.readRun(period, runId, 3, 1) { mayDeliver })
        PhoneControlStore(context).saveGrant(grant)
        mismatch = "late"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE), client.readRun(period, runId, 3, 1) { mayDeliver })
        period.lock()
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED), client.listRuns(period, 1) { true })
    }
}
