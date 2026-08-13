package dev.louiselm.capture

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class ProtocolTest {
    @Test
    fun uploadStatusDistinguishesRetriesFromOwnerAttention() {
        assertEquals(UploadDisposition.SUCCESS, uploadDisposition(201))
        assertEquals(UploadDisposition.RETRY, uploadDisposition(429))
        assertEquals(UploadDisposition.RETRY, uploadDisposition(503))
        assertEquals(UploadDisposition.OPERATOR_ACTION, uploadDisposition(401))
        assertEquals(UploadDisposition.OPERATOR_ACTION, uploadDisposition(400))
    }

    @Test
    fun pairingInputsRequireExactFingerprintAndHttpsAuthority() {
        assertArrayEquals(ByteArray(32) { 0xaa.toByte() }, decodeSha256("aa".repeat(32)))
        assertNull(decodeSha256("aa"))
        assertNull(decodeSha256("z".repeat(64)))
        assertTrue(validReceiverUrl("https://192.0.2.1:7391"))
        assertFalse(validReceiverUrl("http://192.0.2.1:7391"))
        assertFalse(validReceiverUrl("https://user@192.0.2.1:7391"))
    }
}
