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
class PhonePreparedClientTest {
    @Test
    fun signsExactPreparedReadsAndRejectsStaleOrLateReplies() {
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
        val registration = "33".repeat(16)
        val summary = PhonePreparedSummary(registration, "11111111-1111-4111-8111-111111111111", 3)
        val digest = "sha256:" + "01".repeat(32)
        val draft = PhonePreparedDraft("codex", listOf(PhonePreparedBead("a", "first"),
            PhonePreparedBead("b", "second"), PhonePreparedBead("c", "third")),
            PhonePreparedExecution("duration", 1_800_000), 6, digest,
            PhonePreparedBeadScope(digest, listOf("a", "b", "c"), 3),
            PhonePreparedProvider(digest, "openai", "https://api.openai.com/v1/responses",
                listOf("127.0.0.1"), listOf("gpt-5.6-luna"), "low", 3),
            PhonePreparedSkillScope(false, emptyList(), false, false), PhonePreparedSkills("off", digest, null), "denied")
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
            else if (action == 52) {
                val isEnd = wire.getString("payload") == registration && mismatch != "cursor"
                PhoneControlReply.PreparedPage(if (mismatch == "operation") "44".repeat(16) else operation,
                    if (isEnd) emptyList() else listOf(summary), if (isEnd) "" else summary.registrationId)
            }
            else PhoneControlReply.PreparedDetail(if (mismatch == "operation") "44".repeat(16) else operation,
                if (mismatch == "registration") "55".repeat(16) else registration,
                if (mismatch == "run") "22222222-2222-4222-8222-222222222222" else summary.runId,
                if (mismatch == "revision") 4 else summary.revision, draft)
        }
        val client = PhoneControlClient(context, keys, transport)
        assertTrue(client.listPrepared(period, 1) { mayDeliver } is PhoneControlResult.Value)
        val end = client.listPrepared(period, 1, after = registration) { mayDeliver } as PhoneControlResult.Value
        assertTrue(end.value.items.isEmpty())
        assertTrue(client.readPrepared(period, summary, 1) { mayDeliver } is PhoneControlResult.Value)
        for (kind in listOf("operation", "registration", "run", "revision")) {
            mismatch = kind
            assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.readPrepared(period, summary, 1) { mayDeliver })
        }
        mismatch = "operation"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.listPrepared(period, 1) { mayDeliver })
        mismatch = "cursor"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.listPrepared(period, 1, after = registration) { mayDeliver })
        mismatch = "unsupported"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNSUPPORTED), client.listPrepared(period, 1) { mayDeliver })
        mismatch = "grant"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.REVOKED), client.readPrepared(period, summary, 1) { mayDeliver })
        PhoneControlStore(context).saveGrant(grant)
        mismatch = "late"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE), client.readPrepared(period, summary, 1) { mayDeliver })
        period.lock()
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED), client.listPrepared(period, 1) { true })
    }
}
