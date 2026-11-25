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
class PhoneRunLiveClientTest {
    @Test
    fun signsAndFencesLiveRunAndExactLinkedSessionReads() {
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
        val controller = "33".repeat(16)
        val subject = "44".repeat(16)
        val owner = "55".repeat(16)
        val live = PhoneRunLiveView(controller, owner, PhoneRunOwnerStatus.ACTIVE, PhoneRunPhase.WORKING,
            1, 2, "b-1", listOf(PhoneRunLiveSession(subject, "codex", "ready")), false)
        val linked = PhoneRunSessionTarget(runId, 3, owner, PhoneControlTarget(controller, subject))
        var mismatch = ""
        var mayDeliver = true
        val transport: PhoneControlTransport = { _, _, body, _ ->
            val wire = body.getJSONObject("request")
            val operation = wire.getString("operation_id")
            val action = wire.getInt("action")
            val request = PhoneControlRequest(grant.grantId, grant.revision, period.periodId, grant.receiverIdentity,
                wire.getString("controller_id"), wire.getString("subject_id"), phoneControlHex(wire.getString("payload")),
                operation, wire.getLong("expected_revision"), wire.getInt("interaction") == 1, action)
            assertTrue(Signature.getInstance("SHA256withECDSA").run {
                initVerify(working.public); update(request.transcript()); verify(phoneControlHex(wire.getString("signature")))
            })
            if (mismatch == "late") mayDeliver = false
            if (mismatch == "grant") PhoneControlStore(context).saveGrant(grant.copy(grantId = "99".repeat(16)))
            if (action == 56) {
                assertEquals("00".repeat(16), request.controllerId)
                assertEquals(phoneControlHex(phoneControlRunIdBytes(runId)), request.subjectId)
                PhoneControlReply.RunLive(if (mismatch == "operation") "66".repeat(16) else operation,
                    if (mismatch == "run") "22222222-2222-4222-8222-222222222222" else runId,
                    if (mismatch == "revision") 4 else 3, PhoneRunController.CONNECTED, live)
            } else {
                assertEquals(57, action)
                assertEquals(controller, request.controllerId)
                assertEquals(subject, request.subjectId)
                assertEquals(phoneControlHex(phoneControlRunSessionPayload(runId, owner, 3, 0, 50)), wire.getString("payload"))
                PhoneControlReply.SessionPage(controller, if (mismatch == "subject") "77".repeat(16) else subject,
                    operation, 1, "ready", emptyList(), 0, true,
                    PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false))
            }
        }
        val client = PhoneControlClient(context, keys, transport)
        assertTrue(client.readRunLive(period, runId, 3, 1) { mayDeliver } is PhoneControlResult.Value)
        assertTrue(client.readRunSession(period, linked, 1) { mayDeliver } is PhoneControlResult.Value)
        for (kind in listOf("operation", "run", "revision")) {
            mismatch = kind
            assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.readRunLive(period, runId, 3, 1) { mayDeliver })
        }
        mismatch = "subject"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.readRunSession(period, linked, 1) { mayDeliver })
        mismatch = "grant"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.REVOKED), client.readRunLive(period, runId, 3, 1) { mayDeliver })
        PhoneControlStore(context).saveGrant(grant)
        mismatch = "late"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE), client.readRunSession(period, linked, 1) { mayDeliver })
    }
}
