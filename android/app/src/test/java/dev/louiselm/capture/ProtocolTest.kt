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
    fun pairingInputsRequireReceiverIdentityAndHttpsAuthority() {
        assertArrayEquals(ByteArray(32) { 0xaa.toByte() }, decodeSha256("aa".repeat(32)))
        assertNull(decodeSha256("aa"))
        assertNull(decodeSha256("z".repeat(64)))
        assertTrue(validReceiverUrl("https://192.0.2.1:7391"))
        assertFalse(validReceiverUrl("http://192.0.2.1:7391"))
        assertFalse(validReceiverUrl("https://user@192.0.2.1:7391"))
    }

    @Test
    fun receiverIdentityPinsThePublicKeyRatherThanCertificateBytes() {
        val publicKey = byteArrayOf(1, 2, 3)
        val identity = decodeSha256("039058c6f2c0cb492c533b0a4d14ef77cc0f78abccced5287d84a1a2011cfb81")

        assertTrue(matchesReceiverIdentity(identity!!, publicKey))
        assertFalse(matchesReceiverIdentity(identity, byteArrayOf(3, 2, 1)))
    }
}
