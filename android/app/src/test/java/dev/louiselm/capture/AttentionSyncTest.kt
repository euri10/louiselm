package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28, 37])
class AttentionSyncTest {
    @Test
    fun lateRegistrationAndFetchCannotPublishIntoReplacementPairing() {
        var binding = "register-old"
        val state = AttentionState(RuntimeEnvironment.getApplication()) { binding }
        state.enable()
        syncAttention(state, binding, { "old-token" }, {
            binding = "register-new"
            UploadAttempt.Success
        }, { error("obsolete fetch after late registration") })
        assertNull(state.readyOwner())
        syncAttention(state, binding, { "new-token" }, { UploadAttempt.Success }, {
            binding = "fetch-new"
            AttentionFetch.Success(AttentionSnapshot(9, emptyList()))
        })
        assertNull(state.cached(binding))
        assertNull(state.readyOwner())
    }

    @Test
    fun offlineFetchRetriesWithoutRotatingAgainAndCachesOnlyAuthoritativeSnapshot() {
        val state = AttentionState(RuntimeEnvironment.getApplication()) { "pair" }
        state.enable()
        val rotations = mutableListOf<Boolean>()
        var attempts = 0
        fun sync() = syncAttention(state, "pair", { reset -> rotations += reset; "current-token" }, {
            assertEquals("current-token", it)
            UploadAttempt.Success
        }, {
            attempts++
            if (attempts == 1) AttentionFetch.Retry("offline") else AttentionFetch.Success(AttentionSnapshot(8, emptyList()))
        })
        assertTrue(sync() is UploadAttempt.Retry)
        assertEquals("pair", state.readyOwner())
        assertEquals(UploadAttempt.Success, sync())
        assertEquals(listOf(true, false), rotations)
        assertEquals(8L, state.cached("pair")?.generation)
        assertTrue(state.deliver("pair", 8) { true }) // A background fetch never marks the inbox seen.
    }

    @Test
    fun replacementDuringTokenLookupCannotRegisterOrFetchWithOldWork() {
        var binding = "old"
        val state = AttentionState(RuntimeEnvironment.getApplication()) { binding }
        state.enable()
        assertEquals(UploadAttempt.Success, syncAttention(state, "old", {
            binding = "new"
            "obsolete-token"
        }, { error("obsolete registration") }, { error("obsolete fetch") }))
        assertNull(state.readyOwner())
        assertTrue(state.needsInstallationReset("new"))
    }

    @Test
    fun revokedRegistrationBlocksRepeatedCallbacksAndNeverFetches() {
        val state = AttentionState(RuntimeEnvironment.getApplication()) { "pair" }
        state.enable()
        assertTrue(syncAttention(state, "pair", { "token" }, {
            UploadAttempt.OperatorAction("revoked")
        }, { error("revoked fetch") }) is UploadAttempt.OperatorAction)
        assertNull(state.owner())
        assertEquals(UploadAttempt.Success, syncAttention(state, "pair", {
            error("revoked token lookup")
        }, { error("revoked registration") }, { error("revoked fetch") }))
    }
}
