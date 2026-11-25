package dev.louiselm.capture

import java.security.KeyFactory
import java.security.MessageDigest
import java.security.Signature
import java.security.spec.PKCS8EncodedKeySpec
import java.security.spec.X509EncodedKeySpec
import org.json.JSONObject
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
import org.junit.Test

class PhoneControlWireTest {
    private val fixture = JSONObject(requireNotNull(javaClass.getResourceAsStream("/phone-control-v1.json"))
        .bufferedReader().use { it.readText() })

    @Test
    fun productionTranscriptsAndSignaturesMatchTheSharedVectors() {
        val proof = fixture.getJSONObject("proof")
        val message = PhoneControlProof(
            proof.getString("grant_id"), proof.getLong("grant_revision"), proof.getString("enrolled_key_hash"),
            proof.getString("receiver_identity"), proof.getString("challenge_id"), proof.getString("challenge"),
            proof.getString("working_key"),
        )
        assertArrayEquals(phoneControlHex(proof.getString("transcript"), 226), message.transcript())
        val request = fixture.getJSONObject("request")
        val payload = phoneControlReadPayload(request.getLong("before"), request.getInt("limit"))
        val envelope = PhoneControlRequest(
            request.getString("grant_id"), request.getLong("grant_revision"), request.getString("period_id"),
            request.getString("receiver_identity"), request.getString("controller_id"), request.getString("subject_id"),
            payload, request.getString("operation_id"), request.getLong("expected_revision"), true,
        )
        assertArrayEquals(phoneControlHex(request.getString("payload"), 10), payload)
        assertArrayEquals(phoneControlHex(request.getString("transcript"), 187), envelope.transcript())
        assertEquals(request.getString("payload_hash"), phoneControlHex(MessageDigest.getInstance("SHA-256").digest(payload)))
        for ((kind, transcript) in listOf("enrolled" to message.transcript(), "working" to envelope.transcript())) {
            val key = fixture.getJSONObject("keys").getJSONObject(kind)
            val factory = KeyFactory.getInstance("EC")
            val privateKey = factory.generatePrivate(PKCS8EncodedKeySpec(phoneControlHex(key.getString("pkcs8"))))
            val publicKey = factory.generatePublic(X509EncodedKeySpec(phoneControlHex(key.getString("spki"))))
            val vector = if (kind == "enrolled") proof else request
            assertTrue(Signature.getInstance("SHA256withECDSA").run {
                initVerify(publicKey)
                update(transcript)
                verify(phoneControlHex(vector.getString("signature_der")))
            })
            val signature = Signature.getInstance("SHA256withECDSA").run { initSign(privateKey); update(transcript); sign() }
            assertTrue(Signature.getInstance("SHA256withECDSA").run { initVerify(publicKey); update(transcript); verify(signature) })
            val changed = transcript.clone().also { it[it.lastIndex] = (it.last().toInt() xor 1).toByte() }
            assertFalse(Signature.getInstance("SHA256withECDSA").run { initVerify(publicKey); update(changed); verify(signature) })
        }
    }

    @Test
    fun preparedDiscoveryAndDetailBindTheirCursorSubjectAndRevision() {
        val existing = fixture.getJSONObject("request")
        val zero = "00".repeat(16)
        val registration = "ab".repeat(16)
        fun request(action: Int, subject: String, payload: ByteArray, revision: Long) = PhoneControlRequest(
            existing.getString("grant_id"), existing.getLong("grant_revision"), existing.getString("period_id"),
            existing.getString("receiver_identity"), zero, subject, payload,
            existing.getString("operation_id"), revision, true, action = action,
        )
        val page = request(52, zero, phoneControlHex(registration, 16), 0)
        assertEquals(52, page.json(ByteArray(8)).getInt("action"))
        assertEquals(registration, page.json(ByteArray(8)).getString("payload"))
        val detail = request(53, registration, ByteArray(0), 7)
        assertEquals(registration, detail.json(ByteArray(8)).getString("subject_id"))
        assertEquals(7L, detail.json(ByteArray(8)).getLong("expected_revision"))
        assertFalse(page.transcript().contentEquals(detail.transcript()))
        assertThrows(IllegalArgumentException::class.java) { request(52, registration, ByteArray(16), 0) }
        assertThrows(IllegalArgumentException::class.java) { request(52, zero, ByteArray(15), 0) }
        assertThrows(IllegalArgumentException::class.java) { request(53, zero, ByteArray(0), 1) }
        assertThrows(IllegalArgumentException::class.java) { request(53, registration, ByteArray(1), 1) }
        assertThrows(IllegalArgumentException::class.java) { request(53, registration, ByteArray(0), 0) }
    }

    @Test
    fun durableRunReadsBindTheirUuidCursorAndRevision() {
        val existing = fixture.getJSONObject("request")
        val zero = "00".repeat(16)
        val run = "11111111111141118111111111111111"
        val uuid = "11111111-1111-4111-8111-111111111111"
        fun request(action: Int, subject: String, payload: ByteArray, revision: Long) = PhoneControlRequest(
            existing.getString("grant_id"), existing.getLong("grant_revision"), existing.getString("period_id"),
            existing.getString("receiver_identity"), zero, subject, payload,
            existing.getString("operation_id"), revision, true, action = action,
        )
        val first = request(54, zero, ByteArray(17), 0)
        val next = request(54, zero, byteArrayOf(1) + phoneControlHex(run, 16), 0)
        val detail = request(55, run, ByteArray(0), 3)
        assertEquals(54, first.json(ByteArray(8)).getInt("action"))
        assertEquals("01$run", next.json(ByteArray(8)).getString("payload"))
        assertArrayEquals(ByteArray(17), phoneControlRunCursor(null))
        assertArrayEquals(byteArrayOf(1) + phoneControlHex(run, 16), phoneControlRunCursor(uuid))
        assertThrows(IllegalArgumentException::class.java) { phoneControlRunIdBytes("AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE") }
        assertEquals(run, detail.json(ByteArray(8)).getString("subject_id"))
        assertEquals(3L, detail.json(ByteArray(8)).getLong("expected_revision"))
        assertFalse(first.transcript().contentEquals(next.transcript()))
        assertThrows(IllegalArgumentException::class.java) { request(54, zero, byteArrayOf(2) + ByteArray(16), 0) }
        assertThrows(IllegalArgumentException::class.java) { request(54, zero, byteArrayOf(0) + phoneControlHex(run, 16), 0) }
        assertThrows(IllegalArgumentException::class.java) { request(54, run, ByteArray(17), 0) }
        assertThrows(IllegalArgumentException::class.java) { request(55, run, ByteArray(1), 0) }
    }

    @Test
    fun liveRunAndLinkedSessionBindExactOwners() {
        val existing = fixture.getJSONObject("request")
        val zero = "00".repeat(16)
        val run = "11111111111141118111111111111111"
        val controller = "12".repeat(16)
        val subject = "34".repeat(16)
        val owner = "56".repeat(16)
        fun request(action: Int, targetController: String, target: String, payload: ByteArray, revision: Long) = PhoneControlRequest(
            existing.getString("grant_id"), existing.getLong("grant_revision"), existing.getString("period_id"),
            existing.getString("receiver_identity"), targetController, target, payload,
            existing.getString("operation_id"), revision, true, action = action,
        )
        val live = request(56, zero, run, ByteArray(0), 7)
        assertEquals(56, live.json(ByteArray(8)).getInt("action"))
        val payload = byteArrayOf(1) + phoneControlHex(run, 16) + phoneControlHex(owner, 16) +
            java.nio.ByteBuffer.allocate(8).putLong(7).array() + phoneControlReadPayload(0, 20)
        assertArrayEquals(payload, phoneControlRunSessionPayload("11111111-1111-4111-8111-111111111111", owner, 7, 0, 20))
        val linked = request(57, controller, subject, payload, 0)
        assertEquals(57, linked.json(ByteArray(8)).getInt("action"))
        assertEquals(phoneControlHex(payload), linked.json(ByteArray(8)).getString("payload"))
        assertFalse(live.transcript().contentEquals(linked.transcript()))
        assertThrows(IllegalArgumentException::class.java) { request(57, zero, subject, payload, 0) }
        assertThrows(IllegalArgumentException::class.java) { request(57, controller, subject, payload.dropLast(1).toByteArray(), 0) }
        assertThrows(IllegalArgumentException::class.java) { request(56, controller, run, ByteArray(0), 0) }
        assertThrows(IllegalArgumentException::class.java) { request(57, controller, zero, payload, 0) }
        assertThrows(IllegalArgumentException::class.java) { request(57, controller, subject, payload.clone().also { it[0] = 2 }, 0) }
        assertThrows(IllegalArgumentException::class.java) { phoneControlRunSessionPayload("11111111-1111-4111-8111-111111111111", zero, 7, 0, 20) }
        assertThrows(IllegalArgumentException::class.java) { phoneControlRunSessionPayload("11111111-1111-4111-8111-111111111111", owner, 0, 0, 20) }
    }

    @Test
    fun invalidReadRangesAndNoncanonicalIdentifiersAreRejected() {
        for ((before, limit) in listOf(-1L to 1, 0L to 0, 0L to 51, 9_007_199_254_740_992L to 1)) {
            try {
                phoneControlReadPayload(before, limit)
                throw AssertionError("invalid range accepted: $before/$limit")
            } catch (_: IllegalArgumentException) {
                // Expected.
            }
        }
        try {
            phoneControlHex("AA", 1)
            throw AssertionError("uppercase hex accepted")
        } catch (_: IllegalArgumentException) {
            // Expected.
        }
    }
}
