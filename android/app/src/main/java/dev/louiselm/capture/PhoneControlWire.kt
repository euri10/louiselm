package dev.louiselm.capture

import java.io.ByteArrayOutputStream
import java.nio.ByteBuffer
import java.security.MessageDigest
import org.json.JSONObject

private const val MAX_WIRE_INTEGER = 9_007_199_254_740_991L
private val TRANSCRIPT_PREFIX = "louiselm-phone-control\u0000".toByteArray(Charsets.US_ASCII)

internal fun phoneControlHex(text: String, size: Int = -1): ByteArray {
    require(text.length % 2 == 0 && (size < 0 || text.length == size * 2)) { "invalid hex length" }
    require(text.all { it in '0'..'9' || it in 'a'..'f' }) { "invalid hex encoding" }
    return ByteArray(text.length / 2) { index -> text.substring(index * 2, index * 2 + 2).toInt(16).toByte() }
}

internal fun phoneControlHex(bytes: ByteArray): String = bytes.joinToString("") { "%02x".format(it) }

internal fun phoneControlReadPayload(before: Long, limit: Int): ByteArray {
    require(before in 0..MAX_WIRE_INTEGER && limit in 1..50) { "invalid read range" }
    return ByteBuffer.allocate(10).putLong(before).putShort(limit.toShort()).array()
}

private fun ByteArrayOutputStream.field(value: String, size: Int) = write(phoneControlHex(value, size))
private fun ByteArrayOutputStream.number(value: Long) {
    require(value in 0..MAX_WIRE_INTEGER) { "invalid wire integer" }
    write(ByteBuffer.allocate(8).putLong(value).array())
}

internal class PhoneControlProof(
    val grantId: String,
    val grantRevision: Long,
    val enrolledKeyHash: String,
    val receiverIdentity: String,
    val challengeId: String,
    val challenge: String,
    val workingKey: String,
) {
    fun transcript(): ByteArray = ByteArrayOutputStream().apply {
        write(TRANSCRIPT_PREFIX)
        write(byteArrayOf(1, 1))
        field(grantId, 16)
        number(grantRevision)
        field(enrolledKeyHash, 32)
        field(receiverIdentity, 32)
        field(challengeId, 16)
        field(challenge, 32)
        require(workingKey.startsWith("04")) { "invalid working key" }
        field(workingKey, 65)
    }.toByteArray()

    fun json(signature: ByteArray): JSONObject {
        transcript() // Validate all signed fields before constructing the request.
        require(signature.size in 8..72) { "invalid signature" }
        return JSONObject().put("grant_id", grantId).put("grant_revision", grantRevision)
            .put("enrolled_key_hash", enrolledKeyHash).put("receiver_identity", receiverIdentity)
            .put("challenge_id", challengeId).put("challenge", challenge).put("working_key", workingKey)
            .put("signature", phoneControlHex(signature))
    }
}

internal class PhoneControlRequest(
    val grantId: String,
    val grantRevision: Long,
    val periodId: String,
    val receiverIdentity: String,
    val controllerId: String,
    val subjectId: String,
    payload: ByteArray,
    val operationId: String,
    val expectedRevision: Long,
    val interaction: Boolean,
) {
    private val payload = payload.copyOf()
    private val payloadHash = phoneControlHex(MessageDigest.getInstance("SHA-256").digest(this.payload))

    init {
        require(this.payload.size == 10) { "invalid read payload" }
        val decoded = ByteBuffer.wrap(this.payload)
        phoneControlReadPayload(decoded.long, decoded.short.toInt() and 0xffff)
    }

    fun transcript(): ByteArray = ByteArrayOutputStream().apply {
        write(TRANSCRIPT_PREFIX)
        write(byteArrayOf(1, 2))
        field(grantId, 16)
        number(grantRevision)
        field(periodId, 16)
        field(receiverIdentity, 32)
        field(controllerId, 16)
        field(subjectId, 16)
        write(1)
        field(payloadHash, 32)
        field(operationId, 16)
        number(expectedRevision)
        write(if (interaction) 1 else 0)
    }.toByteArray()

    fun json(signature: ByteArray): JSONObject {
        transcript()
        require(signature.size in 8..72) { "invalid signature" }
        return JSONObject().put("grant_id", grantId).put("grant_revision", grantRevision)
            .put("period_id", periodId).put("receiver_identity", receiverIdentity)
            .put("controller_id", controllerId).put("subject_id", subjectId).put("action", 1)
            .put("payload", phoneControlHex(payload)).put("payload_hash", payloadHash)
            .put("operation_id", operationId).put("expected_revision", expectedRevision)
            .put("interaction", if (interaction) 1 else 0).put("signature", phoneControlHex(signature))
    }
}
