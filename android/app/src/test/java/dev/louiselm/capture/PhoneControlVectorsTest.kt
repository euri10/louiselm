package dev.louiselm.capture

import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.security.KeyFactory
import java.security.MessageDigest
import java.security.Signature
import java.security.SignatureException
import java.security.spec.PKCS8EncodedKeySpec
import java.security.spec.X509EncodedKeySpec
import org.json.JSONObject
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class PhoneControlVectorsTest {
    private fun hex(text: String): ByteArray {
        require(text.length % 2 == 0)
        return text.chunked(2).map { it.toInt(16).toByte() }.toByteArray()
    }

    private fun bytes(obj: JSONObject, name: String, size: Int): ByteArray =
        hex(obj.getString(name)).also { assertEquals(name, size, it.size) }

    private fun transcript(message: JSONObject, kind: String): ByteArray {
        val out = ByteArrayOutputStream()
        fun field(name: String, size: Int) = out.write(bytes(message, name, size))
        fun long(name: String) = out.write(ByteBuffer.allocate(8).putLong(message.getLong(name)).array())
        out.write("louiselm-phone-control\u0000".toByteArray(Charsets.US_ASCII))
        out.write(byteArrayOf(1, if (kind == "proof") 1 else 2))
        field("grant_id", 16)
        long("grant_revision")
        if (kind == "proof") {
            field("enrolled_key_hash", 32)
            field("receiver_identity", 32)
            field("challenge_id", 16)
            field("challenge", 32)
            field("working_key", 65)
        } else {
            field("period_id", 16)
            field("receiver_identity", 32)
            field("controller_id", 16)
            field("subject_id", 16)
            out.write(message.getInt("action"))
            field("payload_hash", 32)
            field("operation_id", 16)
            long("expected_revision")
            out.write(message.getInt("interaction"))
        }
        return out.toByteArray()
    }

    private fun verify(key: JSONObject, message: ByteArray, signature: ByteArray): Boolean {
        val public = KeyFactory.getInstance("EC").generatePublic(X509EncodedKeySpec(hex(key.getString("spki"))))
        return try {
            Signature.getInstance("SHA256withECDSA").run {
                initVerify(public)
                update(message)
                verify(signature)
            }
        } catch (_: SignatureException) {
            false
        }
    }

    // This models fixture expectations only; it is not the production authority check.
    private fun outcome(fixture: JSONObject, case: JSONObject): String {
        val kind = case.getString("kind")
        val message = fixture.getJSONObject(kind)
        val key = fixture.getJSONObject("keys").getJSONObject(case.getString("key"))
        if (!verify(key, transcript(message, kind), hex(message.getString("signature_der")))) return "unauthenticated"
        if (kind == "proof") {
            if (case.optString("current_challenge_id", message.getString("challenge_id")) != message.getString("challenge_id")) return "expired"
        } else {
            val payload = hex(case.optString("payload", message.getString("payload")))
            if (!MessageDigest.getInstance("SHA-256").digest(payload).contentEquals(bytes(message, "payload_hash", 32))) return "unauthenticated"
            if (case.optString("current_subject_id", message.getString("subject_id")) != message.getString("subject_id")) return "stale_subject"
        }
        val identity = message.getString(if (kind == "proof") "challenge_id" else "operation_id")
        if (case.optString("consumed_id", "") == identity) return "unauthenticated"
        return "ok"
    }

    @Test
    fun sharedVectorsBindTheExactBytesAndRefuseInvalidContexts() {
        val stream = requireNotNull(javaClass.getResourceAsStream("/phone-control-v1.json"))
        val fixture = JSONObject(stream.bufferedReader().use { it.readText() })
        val keys = fixture.getJSONObject("keys")
        val proof = fixture.getJSONObject("proof")
        assertEquals(1, fixture.getInt("version"))
        assertArrayEquals(MessageDigest.getInstance("SHA-256").digest(bytes(keys.getJSONObject("enrolled"), "sec1", 65)), bytes(proof, "enrolled_key_hash", 32))
        assertArrayEquals(bytes(keys.getJSONObject("working"), "sec1", 65), bytes(proof, "working_key", 65))
        for (field in listOf("grant_id", "grant_revision", "receiver_identity")) {
            assertEquals(proof.get(field), fixture.getJSONObject("request").get(field))
        }
        val request = fixture.getJSONObject("request")
        val payload = ByteBuffer.allocate(10).putLong(request.getLong("before")).putShort(request.getInt("limit").toShort()).array()
        assertArrayEquals(hex(request.getString("payload")), payload)
        for (kind in listOf("proof", "request")) {
            val message = fixture.getJSONObject(kind)
            val encoded = transcript(message, kind)
            val key = keys.getJSONObject(if (kind == "proof") "enrolled" else "working")
            val signature = hex(message.getString("signature_der"))
            assertArrayEquals(hex(message.getString("transcript")), encoded)
            assertTrue(verify(key, encoded, signature))
            assertFalse(verify(key, encoded, bytes(message, "signature_raw", 64)))
            assertFalse(verify(key, encoded, signature + byteArrayOf(0)))
            for (index in encoded.indices) {
                val changed = encoded.clone().also { it[index] = (it[index].toInt() xor 1).toByte() }
                assertFalse("$kind byte $index", verify(key, changed, signature))
            }
            val privateKey = KeyFactory.getInstance("EC").generatePrivate(PKCS8EncodedKeySpec(hex(key.getString("pkcs8"))))
            val freshSignature = Signature.getInstance("SHA256withECDSA").run {
                initSign(privateKey)
                update(encoded)
                sign()
            }
            assertTrue(verify(key, encoded, freshSignature))
        }
        val cases = fixture.getJSONArray("cases")
        for (index in 0 until cases.length()) {
            val case = cases.getJSONObject(index)
            assertEquals(case.getString("name"), case.getString("outcome"), outcome(fixture, case))
        }
    }
}
