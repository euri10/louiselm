package dev.louiselm.capture

import android.Manifest
import android.app.KeyguardManager
import android.content.Intent
import android.os.CancellationSignal
import android.os.Looper
import android.os.PowerManager
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.TextView
import java.security.Signature
import java.time.Duration
import java.util.concurrent.ExecutorService
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertSame
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.android.controller.ActivityController
import org.robolectric.android.util.concurrent.PausedExecutorService
import org.robolectric.annotation.Config
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements
import org.robolectric.annotation.LooperMode
import org.robolectric.util.ReflectionHelpers

private object PhoneScreenFixture {
    val grant = PhoneControlGrant("11".repeat(16), 1, "aa".repeat(32), "22".repeat(32), "33".repeat(32))
    val key = phoneControlWorkingKey()
    var status: PhoneControlStatus = PhoneControlStatus.Disabled
    var prompt: ((Signature?) -> Unit)? = null
    var error: ((Boolean) -> Unit)? = null
    var sends = 0
    var prompts = 0
    var loseEnrollmentReply = false
    var lastPeriod: PhoneControlPeriod? = null
    var reads = 0
    var revision = 1L
    var latest = listOf<PhoneControlEntry>(PhoneControlEntry.Message(1, "assistant", "private session text", false))
    var older = emptyList<PhoneControlEntry>()
    var nextBefore = 0L
    var staleOlder = false
    val readRequests = mutableListOf<Triple<Long, Long, Boolean>>()
    var pairingBinding: String? = "fixture"
    fun reset() {
        status = PhoneControlStatus.Disabled; prompt = null; error = null; sends = 0; prompts = 0
        loseEnrollmentReply = false; lastPeriod = null; reads = 0; pairingBinding = "fixture"
        revision = 1; latest = listOf(PhoneControlEntry.Message(1, "assistant", "private session text", false))
        older = emptyList(); nextBefore = 0; staleOlder = false; readRequests.clear()
    }
}

@Implements(PairingStore::class, isInAndroidSdk = false)
class PhoneScreenPairingFixture {
    @Implementation
    fun binding(): String? = PhoneScreenFixture.pairingBinding
}

@Implements(PhoneControlClient::class, isInAndroidSdk = false)
class PhoneScreenClientFixture {
    @Implementation(methodName = "status")
    internal fun status(): PhoneControlResult<PhoneControlStatus> = PhoneControlResult.Value(PhoneScreenFixture.status)

    @Implementation(methodName = "enroll")
    internal fun enroll(maySend: () -> Boolean): PhoneControlResult<PhoneControlPending> {
        if (!maySend()) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        val pending = PhoneControlPending("44".repeat(16), "00123456", "aa".repeat(32), "22".repeat(32), "33".repeat(32))
        PhoneScreenFixture.status = PhoneControlStatus.Pending(pending)
        if (PhoneScreenFixture.loseEnrollmentReply) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        return PhoneControlResult.Value(pending)
    }

    @Implementation(methodName = "checkEnrollment")
    internal fun checkEnrollment(maySend: () -> Boolean): PhoneControlResult<PhoneControlGrant?> {
        if (!maySend()) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        return PhoneControlResult.Value(PhoneScreenFixture.grant)
    }

    @Implementation(methodName = "prepareUnlock")
    internal fun prepareUnlock(maySend: () -> Boolean): PhoneControlResult<PreparedPhoneControlUnlock> {
        if (!maySend()) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        val grant = PhoneScreenFixture.grant
        val proof = PhoneControlProof(grant.grantId, 1, grant.keyHash, grant.receiverIdentity,
            "55".repeat(16), "66".repeat(32), phoneControlHex(phoneControlPublicKey(PhoneScreenFixture.key.public)))
        val signature = AuthGatedSignature().apply { initSign(PhoneScreenFixture.key.private) }
        return PhoneControlResult.Value(PreparedPhoneControlUnlock(grant, proof, PhoneScreenFixture.key, signature))
    }

    @Implementation(methodName = "finishUnlock")
    internal fun finishUnlock(
        prepared: PreparedPhoneControlUnlock, signature: ByteArray, startedAtMs: Long, maySend: () -> Boolean,
    ): PhoneControlResult<PhoneControlPeriod> {
        if (!maySend()) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        assertTrue(Signature.getInstance("SHA256withECDSA").run {
            initVerify(PhoneScreenFixture.key.public)
            update(prepared.proof.transcript())
            verify(signature)
        })
        PhoneScreenFixture.sends++
        val key = prepared.takeWorkingKey() ?: return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        val period = PhoneControlPeriod(prepared.grant, "77".repeat(16), key, startedAtMs)
        PhoneScreenFixture.lastPeriod = period
        return PhoneControlResult.Value(period)
    }

    @Implementation(methodName = "readSession")
    internal fun readSession(period: PhoneControlPeriod, target: PhoneControlTarget, now: Long, before: Long,
        limit: Int, expectedRevision: Long, interaction: Boolean, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.SessionPage> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        PhoneScreenFixture.reads++
        PhoneScreenFixture.readRequests += Triple(before, expectedRevision, interaction)
        if (before != 0L && PhoneScreenFixture.staleOlder) return PhoneControlResult.Failure(PhoneControlOutcome.STALE_SUBJECT)
        return PhoneControlResult.Value(PhoneControlReply.SessionPage(target.controllerId, target.subjectId, "88".repeat(16),
            PhoneScreenFixture.revision, "ready", if (before == 0L) PhoneScreenFixture.latest else PhoneScreenFixture.older,
            if (before == 0L) PhoneScreenFixture.nextBefore else 0, true))
    }
}

@Implements(PhoneControlAuthenticator::class, isInAndroidSdk = false)
class PhoneScreenAuthenticatorFixture {
    @Implementation
    fun start(signing: Signature, success: (Signature?) -> Unit, error: (Boolean) -> Unit): CancellationSignal {
        PhoneScreenFixture.prompts++
        PhoneScreenFixture.prompt = { (signing as AuthGatedSignature).authenticated = true; success(signing) }
        PhoneScreenFixture.error = error
        return CancellationSignal()
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30, 34], shadows = [PhoneScreenClientFixture::class, PhoneScreenAuthenticatorFixture::class, PhoneScreenPairingFixture::class],
    instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneControlActivityTest {
    private lateinit var controller: ActivityController<PhoneControlActivity>
    private lateinit var activity: PhoneControlActivity
    private lateinit var executor: PausedExecutorService

    @Before
    fun setup() {
        PhoneScreenFixture.reset()
        // The merged manifest declares this signature permission; Robolectric 30 does not grant it.
        val application = RuntimeEnvironment.getApplication()
        shadowOf(application).grantPermissions("${application.packageName}.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION")
        shadowOf(RuntimeEnvironment.getApplication().getSystemService(KeyguardManager::class.java)).setIsDeviceLocked(false)
        shadowOf(RuntimeEnvironment.getApplication().getSystemService(PowerManager::class.java)).turnScreenOn(true)
        create()
    }

    private fun create(intent: Intent? = null) {
        controller = if (intent == null) Robolectric.buildActivity(PhoneControlActivity::class.java)
            else Robolectric.buildActivity(PhoneControlActivity::class.java, intent)
        activity = controller.get()
        ReflectionHelpers.getField<ExecutorService>(activity, "networkExecutor").shutdown()
        executor = PausedExecutorService()
        ReflectionHelpers.setField(activity, "networkExecutor", executor)
        controller.setup()
        drain()
    }

    @After
    fun close() {
        if (!activity.isDestroyed) controller.close()
        executor.runAll()
        shadowOf(Looper.getMainLooper()).idle()
        executor.shutdownNow()
    }

    private fun drain() { executor.runAll(); shadowOf(Looper.getMainLooper()).idle() }
    private fun screenOff() {
        shadowOf(activity.getSystemService(PowerManager::class.java)).turnScreenOn(false)
        shadowOf(Looper.getMainLooper()).idle()
    }
    private fun views(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(views(view.getChildAt(index)))
    }
    private fun button(id: Int) = views(activity.window.decorView).filterIsInstance<Button>()
        .single { it.text.toString() == activity.getString(id) }
    private fun buttonText(text: String) = views(activity.window.decorView).filterIsInstance<Button>()
        .single { it.text.toString() == text }
    private fun hasText(id: Int) = views(activity.window.decorView).filterIsInstance<TextView>()
        .any { it.text.toString() == activity.getString(id) }

    @Test
    fun attentionTargetUnlocksAndDisplaysCurrentReadOnlySession() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        val intent = Intent(activity, PhoneControlActivity::class.java)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, "aa".repeat(16))
            .putExtra(PhoneControlActivity.SUBJECT_ID, "bb".repeat(16))
        controller.close()
        create(intent)
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        drain()
        drain()
        assertEquals(1, PhoneScreenFixture.reads)
        assertTrue(views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains("private session text") })
        screenOff()
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains("private session text") })
    }

    @Test
    fun historyPagingAndPassiveRefreshKeepOneOrderedSnapshot() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        PhoneScreenFixture.latest = listOf(PhoneControlEntry.Message(3, "user", "third", false),
            PhoneControlEntry.Message(4, "assistant", "fourth", false))
        PhoneScreenFixture.older = listOf(PhoneControlEntry.Message(1, "user", "first", false),
            PhoneControlEntry.Message(2, "assistant", "second", false))
        PhoneScreenFixture.nextBefore = 3
        val intent = Intent(activity, PhoneControlActivity::class.java)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, "aa".repeat(16))
            .putExtra(PhoneControlActivity.SUBJECT_ID, "bb".repeat(16))
        controller.close()
        create(intent)
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        drain()
        drain()
        buttonText("Load older messages").performClick()
        drain()
        val displayed = views(activity.window.decorView).filterIsInstance<TextView>().map { it.text.toString() }.toList()
        assertTrue(displayed.indexOfFirst { it.contains("first") } < displayed.indexOfFirst { it.contains("second") })
        assertTrue(displayed.indexOfFirst { it.contains("second") } < displayed.indexOfFirst { it.contains("third") })
        assertEquals(Triple(3L, 1L, true), PhoneScreenFixture.readRequests.last())

        val period = requireNotNull(PhoneScreenFixture.lastPeriod)
        val remaining = period.remainingMs(android.os.SystemClock.elapsedRealtime())
        val olderView = views(activity.window.decorView).filterIsInstance<TextView>().single { it.text.toString().contains("second") }
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(6))
        drain()
        assertSame(olderView, views(activity.window.decorView).filterIsInstance<TextView>()
            .single { it.text.toString().contains("second") })
        PhoneScreenFixture.revision = 2
        PhoneScreenFixture.latest = listOf(PhoneControlEntry.Message(4, "assistant", "updated fourth", false))
        PhoneScreenFixture.nextBefore = 0
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(6))
        drain()
        assertEquals(Triple(0L, 0L, false), PhoneScreenFixture.readRequests.last())
        assertTrue(period.remainingMs(android.os.SystemClock.elapsedRealtime()) <= remaining - 5000)
        val refreshed = views(activity.window.decorView).filterIsInstance<TextView>().map { it.text.toString() }.toList()
        assertTrue(refreshed.any { it.contains("updated fourth") })
        assertFalse(refreshed.any { it.contains("first") })
        assertTrue(refreshed.any { it.contains("updated") && it.contains("older") })
    }

    @Test
    fun anOverlappingUpdateRestartsTheSnapshotInsteadOfJoiningDifferentRevisions() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        PhoneScreenFixture.latest = listOf(PhoneControlEntry.Message(3, "assistant", "old snapshot", false))
        PhoneScreenFixture.nextBefore = 3
        val intent = Intent(activity, PhoneControlActivity::class.java)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, "aa".repeat(16))
            .putExtra(PhoneControlActivity.SUBJECT_ID, "bb".repeat(16))
        controller.close()
        create(intent)
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        drain()
        drain()
        PhoneScreenFixture.staleOlder = true
        PhoneScreenFixture.revision = 2
        PhoneScreenFixture.latest = listOf(PhoneControlEntry.Message(4, "assistant", "fresh snapshot", false))
        PhoneScreenFixture.nextBefore = 0
        buttonText("Load older messages").performClick()
        drain()
        drain()
        assertEquals(listOf(Triple(3L, 1L, true), Triple(0L, 0L, false)), PhoneScreenFixture.readRequests.takeLast(2))
        val displayed = views(activity.window.decorView).filterIsInstance<TextView>().map { it.text.toString() }.toList()
        assertTrue(displayed.any { it.contains("fresh snapshot") })
        assertFalse(displayed.any { it.contains("old snapshot") })
        assertTrue(displayed.any { it.contains("updated") && it.contains("older") })
    }

    @Test
    fun destroyingTheActivityFencesACompletedReadBeforeItsUiCallback() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        val intent = Intent(activity, PhoneControlActivity::class.java)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, "aa".repeat(16))
            .putExtra(PhoneControlActivity.SUBJECT_ID, "bb".repeat(16))
        controller.close()
        create(intent)
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        drain() // The unlock callback schedules the read on the executor.
        executor.runAll() // The protected response is queued to the main loop.
        assertEquals(1, PhoneScreenFixture.reads)
        controller.close()
        shadowOf(Looper.getMainLooper()).idle()
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains("private session text") })
    }

    @Test
    fun screenLockFencesACompletedReadBeforeItsUiCallback() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        val intent = Intent(activity, PhoneControlActivity::class.java)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, "aa".repeat(16))
            .putExtra(PhoneControlActivity.SUBJECT_ID, "bb".repeat(16))
        controller.close()
        create(intent)
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        drain()
        executor.runAll()
        assertEquals(1, PhoneScreenFixture.reads)
        screenOff()
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains("private session text") })
    }

    @Test
    fun pairingChangeFencesACompletedReadBeforeItsUiCallback() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        val intent = Intent(activity, PhoneControlActivity::class.java)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, "aa".repeat(16))
            .putExtra(PhoneControlActivity.SUBJECT_ID, "bb".repeat(16))
        controller.close()
        create(intent)
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        drain()
        executor.runAll()
        assertEquals(1, PhoneScreenFixture.reads)
        PhoneScreenFixture.pairingBinding = "replacement"
        shadowOf(Looper.getMainLooper()).idle()
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains("private session text") })
    }

    @Test
    fun enrollmentAndUnlockSurviveAppSwitchButNotScreenLockOrRecreation() {
        button(R.string.phone_enable).performClick()
        drain()
        val labels = views(activity.window.decorView).filterIsInstance<TextView>().map { it.text.toString() }.toList()
        assertTrue(labels.toString(), labels.any { it.contains("00123456") })
        button(R.string.phone_check).performClick()
        drain()
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        drain()
        assertTrue(hasText(R.string.phone_unlocked))
        assertEquals(1, PhoneScreenFixture.sends)
        controller.pause().stop().restart().start().resume()
        drain() // Revalidate the pairing before showing protected content on return.
        assertTrue(hasText(R.string.phone_unlocked))
        screenOff()
        assertTrue(hasText(R.string.phone_locked))
        controller.close()
        shadowOf(RuntimeEnvironment.getApplication().getSystemService(PowerManager::class.java)).turnScreenOn(true)
        create()
        assertFalse(hasText(R.string.phone_unlocked))
    }

    @Test
    fun aLostEnrollmentReplyLeavesTheStoredApprovalCheckReachable() {
        PhoneScreenFixture.loseEnrollmentReply = true
        button(R.string.phone_enable).performClick()
        drain()
        drain()
        assertTrue(button(R.string.phone_check).isEnabled)
        assertEquals(View.VISIBLE, button(R.string.phone_check).visibility)
        assertTrue(hasText(R.string.phone_unavailable))
    }

    @Test
    fun cancellationAndLateCallbacksCannotSendOrUnlock() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        controller.close()
        create()
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.error).invoke(true)
        assertTrue(hasText(R.string.phone_auth_cancelled))
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.error).invoke(false)
        assertTrue(hasText(R.string.phone_auth_denied))
        button(R.string.phone_unlock).performClick()
        drain()
        val late = requireNotNull(PhoneScreenFixture.prompt)
        screenOff()
        late.invoke(null)
        drain()
        assertEquals(0, PhoneScreenFixture.sends)
        assertFalse(hasText(R.string.phone_unlocked))
    }

    @Test
    fun screenLockStopsAuthenticatedWorkAlreadyQueuedOnTheExecutor() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        controller.close()
        create()
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null) // Signing and the unlock POST are queued.
        screenOff()
        drain()
        assertEquals(0, PhoneScreenFixture.sends)
        assertFalse(hasText(R.string.phone_unlocked))
    }

    @Test
    fun screenLockDiscardsAnUnlockResponseQueuedForTheMainThread() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        controller.close()
        create()
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        executor.runAll() // Server replied, but the main-thread delivery has not run.
        assertEquals(1, PhoneScreenFixture.sends)
        screenOff()
        assertFalse(hasText(R.string.phone_unlocked))
        assertFalse(requireNotNull(PhoneScreenFixture.lastPeriod).isActive(android.os.SystemClock.elapsedRealtime()))
    }

    @Test
    fun destroyedActivityDiscardsQueuedChallengeResult() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        controller.close()
        create()
        button(R.string.phone_unlock).performClick()
        executor.runAll() // Challenge completed off-main, but its callback is still queued.
        controller.close()
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals(0, PhoneScreenFixture.prompts)
        assertEquals(0, PhoneScreenFixture.sends)
    }

    @Test
    @Config(sdk = [37])
    fun deniedOrLateNetworkPermissionCannotStartEnrollment() {
        shadowOf(activity).denyPermissions(Manifest.permission.ACCESS_LOCAL_NETWORK)
        button(R.string.phone_enable).performClick()
        val request = requireNotNull(shadowOf(activity).lastRequestedPermission)
        assertEquals(listOf(Manifest.permission.ACCESS_LOCAL_NETWORK), request.requestedPermissions.toList())
        activity.onRequestPermissionsResult(request.requestCode, request.requestedPermissions, intArrayOf(-1))
        assertTrue(hasText(R.string.local_network_denied))
        button(R.string.phone_enable).performClick()
        val status = views(activity.window.decorView).filterIsInstance<TextView>()
            .single { it.text.toString() == activity.getString(R.string.local_network_denied) }
        status.text = "unchanged after destruction"
        val before = status.text.toString()
        controller.close()
        activity.onRequestPermissionsResult(request.requestCode, request.requestedPermissions, intArrayOf(-1))
        assertEquals(before, status.text.toString())
        assertEquals(PhoneControlStatus.Disabled, PhoneScreenFixture.status)
    }
}
