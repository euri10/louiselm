package dev.louiselm.capture

import java.security.KeyFactory
import java.security.Signature
import java.security.spec.X509EncodedKeySpec
import org.json.JSONObject
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
import org.junit.Test

class PhoneControlSendWireTest {
    @Test
    fun sendUsesTheSharedSignedPayloadAndExactRevision() {
        val original = JSONObject(requireNotNull(javaClass.getResourceAsStream("/phone-control-v1.json")).bufferedReader().use { it.readText() })
        val fixture = JSONObject(requireNotNull(javaClass.getResourceAsStream("/phone-control-send-v1.json")).bufferedReader().use { it.readText() })
        val fields = original.getJSONObject("request")
        val payload = phoneControlSendPayload("Reply from phone\n")
        val request = PhoneControlRequest(fields.getString("grant_id"), fields.getLong("grant_revision"), fields.getString("period_id"),
            fields.getString("receiver_identity"), fields.getString("controller_id"), fields.getString("subject_id"),
            payload, fields.getString("operation_id"), 1, true, action = 3)
        assertArrayEquals(phoneControlHex(fixture.getString("payload")), payload)
        assertArrayEquals(phoneControlHex(fixture.getString("transcript")), request.transcript())
        val key = original.getJSONObject("keys").getJSONObject("working")
        val publicKey = KeyFactory.getInstance("EC").generatePublic(X509EncodedKeySpec(phoneControlHex(key.getString("spki"))))
        assertTrue(Signature.getInstance("SHA256withECDSA").run {
            initVerify(publicKey); update(request.transcript()); verify(phoneControlHex(fixture.getString("signature_der")))
        })
        assertEquals(fixture.getString("payload_hash"), request.json(phoneControlHex(fixture.getString("signature_der"))).getString("payload_hash"))
    }

    @Test
    fun invalidTextAndUnreviewedSendAreRejected() {
        for (text in listOf("", "bad\u0000text", "\ud800", "x".repeat(4096))) {
            assertThrows(IllegalArgumentException::class.java) { phoneControlSendPayload(text) }
        }
        for ((revision, interaction) in listOf(0L to true, 1L to false)) {
            assertThrows(IllegalArgumentException::class.java) {
                PhoneControlRequest("01".repeat(16), 1, "02".repeat(16), "03".repeat(32),
                    "04".repeat(16), "05".repeat(16), phoneControlSendPayload("hello"), "06".repeat(16), revision, interaction, action = 3)
            }
        }
    }
}
