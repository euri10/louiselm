package dev.louiselm.capture

import android.content.Context
import java.nio.ByteBuffer
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
class PhoneVoiceArchiveClientTest {
    @Test fun signsAndCorrelatesEveryPageAndRejectsChangedOrLateResults() {
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
        val row = PhoneVoiceRow("11111111-1111-4111-8111-111111111111", PhoneControlTarget("33".repeat(16), "44".repeat(16)),
            "/project", "codex", true)
        var mismatch = ""
        var mayDeliver = true
        val transport: PhoneControlTransport = { _, _, body, _ ->
            val wire = body.getJSONObject("request")
            val payload = phoneControlHex(wire.getString("payload"))
            val operation = wire.getString("operation_id")
            val request = PhoneControlRequest(grant.grantId, grant.revision, period.periodId, grant.receiverIdentity,
                wire.getString("controller_id"), wire.getString("subject_id"), payload, operation,
                wire.getLong("expected_revision"), wire.getInt("interaction") == 1, wire.getInt("action"))
            assertTrue(Signature.getInstance("SHA256withECDSA").run {
                initVerify(working.public); update(request.transcript()); verify(phoneControlHex(wire.getString("signature")))
            })
            if (request.action == 81) PhoneControlReply.VoiceHistory(operation, listOf(row), "")
            else {
                assertEquals(82, request.action)
                val offset = ByteBuffer.wrap(payload, 16, 4).int
                if (mismatch == "late") mayDeliver = false
                PhoneControlReply.VoiceResult(if (mismatch == "operation") "99".repeat(16) else operation,
                    row.id, if (mismatch == "receiver") "ff".repeat(32) else grant.receiverIdentity,
                    if (mismatch == "target") row.target.copy(subjectId = "55".repeat(16)) else row.target,
                    row.project, row.agent, "66".repeat(32), if (offset > 0 && mismatch == "revision") "88".repeat(32) else "77".repeat(32),
                    offset, if (offset == 0) 2 else 0, if (offset == 0) "é" else "done")
            }
        }
        val client = PhoneControlClient(context, keys, transport)
        assertEquals(listOf(row), (client.voiceHistory(period, 1) { true } as PhoneControlResult.Value).value.records)
        val result = client.voiceResult(period, row, 1) { mayDeliver } as PhoneControlResult.Value
        assertEquals("édone", result.value.text)
        for (kind in listOf("operation", "receiver", "target", "revision")) {
            mismatch = kind
            assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.voiceResult(period, row, 1) { true })
        }
        mismatch = "late"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE), client.voiceResult(period, row, 1) { mayDeliver })
        period.lock()
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED), client.voiceResult(period, row, 1) { true })
    }
}
