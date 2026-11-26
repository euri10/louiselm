package dev.louiselm.capture

import java.io.File
import java.security.MessageDigest
import java.security.Signature
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.json.JSONObject

@RunWith(RobolectricTestRunner::class)
class PhoneVoiceJobClientTest {
    @Test fun signsTheRetainedDestinationAndRejectsLateOrUnrelatedResponses() {
        val config = PairingConfig("https://127.0.0.1:1", "aa".repeat(32), "device", "fixture")
        val grant = PhoneControlGrant("11".repeat(16), 1, config.receiverIdentitySha256, phoneControlPairingBinding(config), "44".repeat(32))
        val key = phoneControlWorkingKey()
        val period = PhoneControlPeriod(grant, "22".repeat(16), key.private, 0)
        val source = PhoneOfflineSource(grant.receiverIdentity, PhoneControlTarget("33".repeat(16), "55".repeat(16)), "session", "codex", "/project")
        val bytes = "dummy audio".toByteArray()
        val file = File.createTempFile("dummy-voice", ".m4a").apply { writeBytes(bytes) }
        val audio = CaptureRecord("01010101-0101-0101-0101-010101010101", 100, 50, "audio/mp4", bytes.size.toLong(),
            phoneControlHex(MessageDigest.getInstance("SHA-256").digest(bytes)), file)
        val record = PhoneVoiceRecord(audio.id, source, "66".repeat(32), "77".repeat(32), PhoneVoiceIntent.REQUESTED, audio)
        var late = false
        var revoked = false
        var mismatched = false
        var appliedRefusals = 0
        val client = PhoneVoiceJobClient({ if (revoked) null else config to grant }, { _, body, streamed, _ ->
            assertEquals(audio, streamed)
            val wire = body.getJSONObject("request")
            val request = PhoneControlRequest(wire.getString("grant_id"), wire.getLong("grant_revision"), wire.getString("period_id"),
                wire.getString("receiver_identity"), wire.getString("controller_id"), wire.getString("subject_id"),
                phoneControlHex(wire.getString("payload")), wire.getString("operation_id"), wire.getLong("expected_revision"),
                wire.getInt("interaction") == 1, wire.getInt("action"))
            assertEquals(83, request.action)
            assertEquals(source.target.controllerId, request.controllerId)
            assertEquals(source.target.subjectId, request.subjectId)
            assertTrue(Signature.getInstance("SHA256withECDSA").run {
                initVerify(key.public); update(request.transcript()); verify(phoneControlHex(wire.getString("signature")))
            })
            if (late) period.lock()
            if (late) PhoneVoiceJobResponse.Failure(PhoneControlOutcome.REVOKED)
            else PhoneVoiceJobResponse.Accepted(if (mismatched) "99".repeat(16) else request.operationId, audio.id, "pending")
        }, { 1L }, { appliedRefusals++ })
        assertTrue(client.submit(period, record) { true } is PhoneControlResult.Value)
        mismatched = true
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.submit(period, record) { true })
        mismatched = false
        revoked = true
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.REVOKED), client.submit(period, record) { true })
        revoked = false
        late = true
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE), client.submit(period, record) { true })
        assertEquals(0, appliedRefusals)
    }
}
