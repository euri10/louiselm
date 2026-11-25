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
class PhoneSavedReviewClientTest {
    @Test
    fun signsAndCorrelatesPagesAndRejectsLateOrMismatchedReplies() {
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
        val run = "11111111-1111-4111-8111-111111111111"
        val revision = "33".repeat(32)
        val digest = "sha256:" + "44".repeat(32)
        val cursor = PhoneSavedCursor(0, PhoneSavedSide.AFTER, 0)
        val review = PhoneSavedReview(run, "result", revision, "codex", listOf("codex"), "/checkout", "a".repeat(40),
            digest, digest, digest, digest, 1, PhoneSavedVerification.MATCHING, PhoneSavedProvenance.UNKNOWN,
            PhoneSavedFile("file", PhoneSavedChange.MODIFIED, PhoneSavedSide.BEFORE, 0, "text", false, false), cursor)
        var mismatch = ""
        var mayDeliver = true
        val transport: PhoneControlTransport = { _, _, body, _ ->
            val wire = body.getJSONObject("request")
            val operation = wire.getString("operation_id")
            val request = PhoneControlRequest(grant.grantId, grant.revision, period.periodId, grant.receiverIdentity,
                wire.getString("controller_id"), wire.getString("subject_id"), phoneControlHex(wire.getString("payload")),
                operation, wire.getLong("expected_revision"), wire.getInt("interaction") == 1, wire.getInt("action"))
            assertEquals(80, request.action)
            assertEquals(phoneControlHex(phoneControlRunIdBytes(run)), request.subjectId)
            assertTrue(Signature.getInstance("SHA256withECDSA").run {
                initVerify(working.public); update(request.transcript()); verify(phoneControlHex(wire.getString("signature")))
            })
            if (mismatch == "late") mayDeliver = false
            val complete = review.copy(next = null)
            val changed = when (mismatch) {
                "run" -> complete.copy(runId = "22222222-2222-4222-8222-222222222222")
                "result" -> complete.copy(resultId = "other")
                "revision" -> complete.copy(revision = "55".repeat(32))
                "cursor" -> review.copy(next = cursor)
                else -> complete
            }
            PhoneControlReply.SavedReview(if (mismatch == "operation") "66".repeat(16) else operation, changed)
        }
        val client = PhoneControlClient(context, keys, transport)
        assertTrue(client.readSavedReview(period, run, "result", null, null, 1) { mayDeliver } is PhoneControlResult.Value)
        for (kind in listOf("operation", "run", "result", "revision", "cursor")) {
            mismatch = kind
            assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED),
                client.readSavedReview(period, run, "result", revision, cursor, 1) { mayDeliver })
        }
        mismatch = "late"
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE),
            client.readSavedReview(period, run, "result", null, null, 1) { mayDeliver })
        period.lock()
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED),
            client.readSavedReview(period, run, "result", null, null, 1) { true })
    }
}
