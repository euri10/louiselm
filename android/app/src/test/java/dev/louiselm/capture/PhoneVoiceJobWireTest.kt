package dev.louiselm.capture

import java.io.File
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertThrows
import org.junit.Test

class PhoneVoiceJobWireTest {
    private val record = CaptureRecord("01010101-0101-0101-0101-010101010101", 100, 50, "audio/mp4",
        20L * 1024 * 1024, "02".repeat(32), File("unused-dummy-audio"))
    private val source = PhoneOfflineSource("aa".repeat(32), PhoneControlTarget("11".repeat(16), "22".repeat(16)), "session", "fixture", "/project")

    @Test fun admissionBindsRecordingDigestLengthAndDraftRevision() {
        val expected = byteArrayOf(1) + ByteArray(16) { 1 } + ByteArray(32) { 2 } +
            java.nio.ByteBuffer.allocate(24).putLong(100).putLong(50).putLong(20L * 1024 * 1024).array() +
            ByteArray(32) { 3 } + ByteArray(32) { 4 } + phoneControlHex("9aeebed9be9be18077fc10a85110e46761e00c7657bd0e5cdea5ea57ace9013d")
        assertArrayEquals(expected, phoneVoiceAdmissionPayload(record, source, "03".repeat(32), "04".repeat(32)))
        assertArrayEquals(ByteArray(16) { 1 }, phoneVoiceMutationPayload(record.id))
        assertArrayEquals(ByteArray(16) { 1 } + ByteArray(16) { 5 }, phoneVoiceRetryPayload(record.id, "05".repeat(16)))
    }

    @Test fun malformedAndOversizedAudioCannotBecomeAJob() {
        for (invalid in listOf(record.copy(bytes = 0), record.copy(bytes = 20L * 1024 * 1024 + 1),
            record.copy(mimeType = "audio/ogg"), record.copy(sha256 = "x"), record.copy(recordedAtMs = 0))) {
            assertThrows(IllegalArgumentException::class.java) { phoneVoiceAdmissionPayload(invalid, source, "03".repeat(32), "04".repeat(32)) }
        }
        assertThrows(IllegalArgumentException::class.java) { phoneVoiceAdmissionPayload(record, source, "03".repeat(32), "x") }
        assertThrows(IllegalArgumentException::class.java) { phoneVoiceMutationPayload("invalid") }
    }

    @Test fun voiceRequestsRequireTheExactDestinationAndDoNotRenewOnUpload() {
        fun request(action: Int, payload: ByteArray, interaction: Boolean = false, subject: String = "22".repeat(16)) =
            PhoneControlRequest("11".repeat(16), 1, "22".repeat(16), source.receiver,
                source.target.controllerId, subject, payload, "33".repeat(16), 0, interaction, action)
        val admission = phoneVoiceAdmissionPayload(record, source, "03".repeat(32), "04".repeat(32))
        request(83, admission).transcript()
        request(84, phoneVoiceMutationPayload(record.id), true).transcript()
        request(85, phoneVoiceRetryPayload(record.id, "05".repeat(16)), true).transcript()
        assertThrows(IllegalArgumentException::class.java) { request(83, admission, true) }
        assertThrows(IllegalArgumentException::class.java) { request(84, byteArrayOf(1)) }
        assertThrows(IllegalArgumentException::class.java) { request(85, phoneVoiceRetryPayload(record.id, "05".repeat(16)), subject = "00".repeat(16)) }
    }
}
