package dev.louiselm.capture

import java.security.Signature
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PhoneControlHistoryWireTest {
    @Test
    fun signedHistoryPageUsesBackgroundActionAndExactCursor() {
        val cursor = "12".repeat(16)
        assertArrayEquals(byteArrayOf(1, 50), phoneControlHistoryPagePayload(null, 50))
        val payload = phoneControlHistoryPagePayload(cursor, 7)
        assertArrayEquals(byteArrayOf(2) + phoneControlHex(cursor, 16) + byteArrayOf(7), payload)
        val zeros = "00".repeat(16)
        val request = PhoneControlRequest("01".repeat(16), 1, "02".repeat(16), "03".repeat(32),
            zeros, zeros, payload, "04".repeat(16), 0, false, action = 2)
        val key = phoneControlWorkingKey()
        val signature = Signature.getInstance("SHA256withECDSA").run { initSign(key.private); update(request.transcript()); sign() }
        assertTrue(Signature.getInstance("SHA256withECDSA").run { initVerify(key.public); update(request.transcript()); verify(signature) })
        assertEquals(2, request.json(signature).getInt("action"))
        assertEquals(0, request.json(signature).getInt("interaction"))
        assertEquals(phoneControlHex(payload), request.json(signature).getString("payload"))
    }

    @Test
    fun historyRequestRejectsInteractionAndWrongPageScope() {
        val zeros = "00".repeat(16)
        for ((subject, revision, interaction) in listOf(
            Triple("01".repeat(16), 0L, false), Triple(zeros, 1L, false), Triple(zeros, 0L, true),
        )) {
            try {
                PhoneControlRequest("01".repeat(16), 1, "02".repeat(16), "03".repeat(32),
                    zeros, subject, phoneControlHistoryPagePayload(null, 50), "04".repeat(16), revision, interaction, action = 2)
                throw AssertionError("invalid history request accepted")
            } catch (_: IllegalArgumentException) {
                // Expected.
            }
        }
    }
}
