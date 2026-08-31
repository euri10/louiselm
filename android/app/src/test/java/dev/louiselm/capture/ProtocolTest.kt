package dev.louiselm.capture

import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Assert.assertThrows
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

    @Test
    fun pairingTransitionSeparatesFirstPairEndpointUpdateAndReceiverMigration() {
        assertEquals(PairingTransition.FIRST_PAIR, pairingTransition(null, "aa".repeat(32)))
        assertEquals(
            PairingTransition.ENDPOINT_UPDATE,
            pairingTransition("aa".repeat(32), "aa".repeat(32)),
        )
        assertEquals(
            PairingTransition.RECEIVER_MIGRATION,
            pairingTransition("aa".repeat(32), "bb".repeat(32)),
        )
    }

    @Test
    fun uploadEligibilityRequiresAnExplicitMatchingQueueOwner() {
        assertFalse(queueBelongsToReceiver(null, "aa".repeat(32)))
        assertTrue(queueBelongsToReceiver("aa".repeat(32), "aa".repeat(32)))
        assertFalse(queueBelongsToReceiver("bb".repeat(32), "aa".repeat(32)))
    }

    @Test
    fun attentionSnapshotAcceptsOnlyBoundedTypedFields() {
        val snapshot = AttentionSnapshot.parse(
            """
            {
              "generation": 4,
              "items": [{
                "subject_kind": "session",
                "subject_id": "agent/session-1",
                "kind": "permission_required",
                "source_operation_id": "11111111-2222-4333-8444-555555555555",
                "created_at_ms": 123,
                "eligible": true,
                "reason": "Permission is required",
                "linked_run_id": "11111111-2222-4333-8444-555555555555",
                "stage": "review/execute"
              }]
            }
            """.trimIndent(),
        )

        assertEquals(4, snapshot.generation)
        assertEquals(AttentionKind.PERMISSION_REQUIRED, snapshot.items.single().kind)
        assertEquals("Permission is required", snapshot.items.single().kind.reason)
        assertEquals("review/execute", snapshot.items.single().stage)
    }

    @Test
    fun attentionSnapshotRejectsUnknownFieldsAndMalformedIdentifiers() {
        val unknown = """
            {"generation":0,"items":[],"message":"do something"}
        """.trimIndent()
        assertThrows(IllegalArgumentException::class.java) { AttentionSnapshot.parse(unknown) }

        val malformed = """
            {
              "generation": 1,
              "items": [{
                "subject_kind": "run",
                "subject_id": "not-a-uuid",
                "kind": "run_parked",
                "source_operation_id": "11111111-2222-4333-8444-555555555555",
                "created_at_ms": 123,
                "eligible": false,
                "reason": "Run is Parked",
                "linked_run_id": null,
                "stage": null
              }]
            }
        """.trimIndent()
        assertThrows(IllegalArgumentException::class.java) { AttentionSnapshot.parse(malformed) }
    }
}
