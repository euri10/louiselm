package dev.louiselm.capture

import java.nio.ByteBuffer
import java.security.MessageDigest

/** Bind a voice job to its immutable original and the saved draft version. */
internal fun phoneVoiceAdmissionPayload(record: CaptureRecord, source: PhoneOfflineSource, draftId: String, revision: String): ByteArray {
    require(record.mimeType == "audio/mp4" && record.bytes in 1..20L * 1024 * 1024)
    require(record.recordedAtMs in 1..9_007_199_254_740_991L && record.durationMs in 1..9_007_199_254_740_991L)
    val project = source.project.toByteArray(Charsets.UTF_8)
    val agent = source.agent.toByteArray(Charsets.UTF_8)
    require(project.size in 1..512 && agent.size in 1..512 && !source.project.contains('\u0000') && !source.agent.contains('\u0000'))
    val scope = "louiselm-voice-source\u0000".toByteArray(Charsets.US_ASCII) +
        ByteBuffer.allocate(2).putShort(project.size.toShort()).array() + project +
        ByteBuffer.allocate(2).putShort(agent.size.toShort()).array() + agent
    return byteArrayOf(1) + phoneControlRunIdBytes(record.id) + phoneControlHex(record.sha256, 32) +
        ByteBuffer.allocate(24).putLong(record.recordedAtMs).putLong(record.durationMs).putLong(record.bytes).array() +
        phoneControlHex(draftId, 32) + phoneControlHex(revision, 32) + MessageDigest.getInstance("SHA-256").digest(scope)
}

/** Bind retry or cancellation to the original recording UUID. */
internal fun phoneVoiceMutationPayload(id: String): ByteArray = phoneControlRunIdBytes(id)

/** The retry identity survives a lost transport response; each signed request still has a fresh nonce. */
internal fun phoneVoiceRetryPayload(id: String, retryId: String): ByteArray {
    val marker = phoneControlHex(retryId, 16)
    require(marker.any { it != 0.toByte() })
    return phoneControlRunIdBytes(id) + marker
}
