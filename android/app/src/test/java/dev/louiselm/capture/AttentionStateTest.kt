package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28, 34])
class AttentionStateTest {
    @Test
    fun tokenModeStateCannotAuthorizeInstallationModeNotifications() {
        val context = RuntimeEnvironment.getApplication()
        context.getSharedPreferences("attention-push", android.content.Context.MODE_PRIVATE).edit()
            .putString("owner", "pair").putBoolean("enabled", true)
            .putBoolean("prepared", true).putBoolean("registered", true).commit()
        val state = AttentionState(context) { "pair" }
        assertTrue(state.needsInstallationReset("pair"))
        assertNull(state.readyOwner())
    }

    @Test
    fun viewingInboxBeforeOptInStillSuppressesAlreadySeenGeneration() {
        val state = AttentionState(RuntimeEnvironment.getApplication()) { "pair" }
        assertTrue(state.seen("pair", 6))
        assertTrue(state.enable())
        assertFalse(state.deliver("pair", 6) { error("already seen before enabling") })
        assertTrue(state.deliver("pair", 7) { true })
    }

    @Test
    fun unpairedAndUnsetNeverNotifyAndDeniedDeliveryDoesNotConsumeGeneration() {
        var binding: String? = null
        val state = AttentionState(RuntimeEnvironment.getApplication()) { binding }
        assertNull(state.owner())
        assertFalse(state.enable())
        binding = "first-pairing"
        assertNull(state.owner())
        assertTrue(state.enable())
        assertFalse(state.deliver("first-pairing", 1) { false })
        assertTrue(state.deliver("first-pairing", 1) { true })
    }

    @Test
    fun duplicatesOlderAndSeenGenerationsStaySuppressedAcrossOwnersAndRestart() {
        var binding: String? = "first"
        val context = RuntimeEnvironment.getApplication()
        val state = AttentionState(context) { binding }
        assertTrue(state.enable())
        assertTrue(state.deliver("first", 3) { true })
        val restarted = AttentionState(context) { binding }
        assertFalse(restarted.deliver("first", 3) { error("duplicate") })
        assertFalse(restarted.deliver("first", 2) { error("older") })
        assertTrue(restarted.seen("first", 5))
        assertFalse(restarted.deliver("first", 4) { error("already seen") })
        assertTrue(restarted.deliver("first", 6) { true })
        binding = "replacement"
        assertFalse(restarted.deliver("first", 7) { error("old pairing") })
        assertEquals("replacement", restarted.owner())
        assertTrue(restarted.deliver("replacement", 1) { true })
        assertFalse(restarted.installationPrepared("first"))
        assertTrue(restarted.installationPrepared("replacement"))
        assertFalse(restarted.needsInstallationReset("replacement"))
        assertTrue(restarted.block("replacement"))
        assertNull(restarted.owner())
        assertFalse(restarted.deliver("replacement", 2) { error("revoked pairing") })
        binding = null
        assertNull(restarted.owner())
    }

    @Test
    fun cachedSnapshotIsValidatedAndCannotCrossPairingBoundaries() {
        var binding: String? = "first"
        val state = AttentionState(RuntimeEnvironment.getApplication()) { binding }
        assertTrue(state.enable())
        val snapshot = AttentionSnapshot(3, emptyList())
        assertTrue(state.cache("first", snapshot))
        assertEquals(snapshot, state.cached("first"))
        binding = "second"
        assertFalse(state.cache("first", AttentionSnapshot(4, emptyList())))
        assertNull(state.cached("second"))
    }
}
