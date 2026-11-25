package dev.louiselm.capture

import android.Manifest
import android.app.KeyguardManager
import android.content.Intent
import android.os.CancellationSignal
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import java.io.File
import java.security.Signature
import java.time.Duration
import java.util.concurrent.ExecutorService
import javax.crypto.Cipher
import javax.crypto.spec.SecretKeySpec
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
    val offlineMaterial = ByteArray(32) { 7 }
    var status: PhoneControlStatus = PhoneControlStatus.Disabled
    var prompt: ((Signature?) -> Unit)? = null
    var error: ((Boolean) -> Unit)? = null
    var sends = 0
    var prompts = 0
    var loseEnrollmentReply = false
    var lastPeriod: PhoneControlPeriod? = null
    var reads = 0
    var lastTarget: PhoneControlTarget? = null
    var sessionRows = emptyList<PhoneControlSessionSummary>()
    var staleRead = false
    var actionHistoryReads = 0
    var moreActionHistory = false
    val actionHistoryCursors = mutableListOf<String?>()
    var revision = 1L
    var latest = listOf<PhoneControlEntry>(PhoneControlEntry.Message(1, "assistant", "private session text", false))
    var older = emptyList<PhoneControlEntry>()
    var latestSource = PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false)
    var olderSource = latestSource
    var nextBefore = 0L
    var olderNextBefore = 0L
    var staleOlder = false
    var olderOutcome: PhoneControlOutcome? = null
    val readRequests = mutableListOf<Triple<Long, Long, Boolean>>()
    var pairingBinding: String? = "fixture"
    var preUnlockOffline = true
    var sendCalls = 0
    var sendOutcome = PhoneControlOutcome.UNSUPPORTED
    var sendFailure: PhoneControlOutcome? = null
    var evidenceState: PhoneActionState? = null
    var evidenceReads = 0
    var evidenceReadOnMain = false
    val evidenceOperationIds = mutableListOf<String>()
    val preparedSummary = PhonePreparedSummary("33".repeat(16), "11111111-1111-4111-8111-111111111111", 3)
    val preparedDigest = "sha256:" + "01".repeat(32)
    val preparedDraft = PhonePreparedDraft("codex", listOf(PhonePreparedBead("a", "private prepared instruction"),
        PhonePreparedBead("b", "second"), PhonePreparedBead("c", "third")),
        PhonePreparedExecution("duration", 1_800_000), 6, preparedDigest,
        PhonePreparedBeadScope(preparedDigest, listOf("a", "b", "c"), 3),
        PhonePreparedProvider(preparedDigest, "openai", "https://api.openai.com/v1/responses",
            listOf("127.0.0.1"), listOf("gpt-5.6-luna"), "low", 3),
        PhonePreparedSkillScope(false, emptyList(), false, false), PhonePreparedSkills("off", preparedDigest, null), "denied")
    var preparedReads = 0
    var preparedFailure: PhoneControlOutcome? = null
    val preparedInteractions = mutableListOf<Boolean>()
    fun reset() {
        status = PhoneControlStatus.Disabled; prompt = null; error = null; sends = 0; prompts = 0
        loseEnrollmentReply = false; lastPeriod = null; reads = 0; lastTarget = null; sessionRows = emptyList(); staleRead = false; pairingBinding = "fixture"
        preUnlockOffline = true
        sendCalls = 0; sendOutcome = PhoneControlOutcome.UNSUPPORTED; sendFailure = null; evidenceState = null; evidenceReads = 0; evidenceReadOnMain = false
        evidenceOperationIds.clear()
        preparedReads = 0; preparedFailure = null; preparedInteractions.clear()
        actionHistoryReads = 0; moreActionHistory = false; actionHistoryCursors.clear()
        revision = 1; latest = listOf(PhoneControlEntry.Message(1, "assistant", "private session text", false))
        older = emptyList(); nextBefore = 0; olderNextBefore = 0; staleOlder = false; olderOutcome = null; readRequests.clear()
        latestSource = PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false); olderSource = latestSource
    }
}

@Implements(PairingStore::class, isInAndroidSdk = false)
class PhoneScreenPairingFixture {
    @Implementation
    fun binding(): String? = PhoneScreenFixture.pairingBinding

    @Implementation(methodName = "load")
    internal fun load() = PairingConfig("https://example.invalid", PhoneScreenFixture.grant.receiverIdentity, "device", "credential")
}

@Implements(PhoneControlClient::class, isInAndroidSdk = false)
class PhoneScreenClientFixture {
    @Implementation(methodName = "status")
    internal fun status(): PhoneControlResult<PhoneControlStatus> = PhoneControlResult.Value(PhoneScreenFixture.status)

    @Implementation(methodName = "listPrepared")
    internal fun listPrepared(period: PhoneControlPeriod, now: Long, after: String?, interaction: Boolean,
        maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.PreparedPage> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        PhoneScreenFixture.preparedFailure?.let { return PhoneControlResult.Failure(it) }
        PhoneScreenFixture.preparedInteractions += interaction
        val items = if (after == null) listOf(PhoneScreenFixture.preparedSummary) else emptyList()
        return PhoneControlResult.Value(PhoneControlReply.PreparedPage("88".repeat(16), items,
            items.singleOrNull()?.registrationId ?: ""))
    }

    @Implementation(methodName = "readPrepared")
    internal fun readPrepared(period: PhoneControlPeriod, target: PhonePreparedSummary, now: Long,
        interaction: Boolean, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.PreparedDetail> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        PhoneScreenFixture.preparedFailure?.let { return PhoneControlResult.Failure(it) }
        PhoneScreenFixture.preparedInteractions += interaction
        PhoneScreenFixture.preparedReads++
        return PhoneControlResult.Value(PhoneControlReply.PreparedDetail("88".repeat(16), target.registrationId,
            target.runId, target.revision, PhoneScreenFixture.preparedDraft))
    }

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
        PhoneScreenFixture.lastTarget = target
        if (PhoneScreenFixture.staleRead) return PhoneControlResult.Failure(PhoneControlOutcome.STALE_SUBJECT)
        PhoneScreenFixture.readRequests += Triple(before, expectedRevision, interaction)
        if (before != 0L && PhoneScreenFixture.staleOlder) return PhoneControlResult.Failure(PhoneControlOutcome.STALE_SUBJECT)
        if (before != 0L && PhoneScreenFixture.olderOutcome != null) return PhoneControlResult.Failure(requireNotNull(PhoneScreenFixture.olderOutcome))
        val source = if (before == 0L) PhoneScreenFixture.latestSource else PhoneScreenFixture.olderSource
        val complete = source.replay == "not_applicable" && source.redaction == "unknown" && !source.missing && !source.truncated
        return PhoneControlResult.Value(PhoneControlReply.SessionPage(target.controllerId, target.subjectId, "88".repeat(16),
            PhoneScreenFixture.revision, "ready", if (before == 0L) PhoneScreenFixture.latest else PhoneScreenFixture.older,
            if (before == 0L) PhoneScreenFixture.nextBefore else PhoneScreenFixture.olderNextBefore, complete, source))
    }

    @Implementation(methodName = "listControllers")
    internal fun listControllers(period: PhoneControlPeriod, now: Long, after: String?, limit: Int,
        interaction: Boolean, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.ControllerList> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        return PhoneControlResult.Value(PhoneControlReply.ControllerList("88".repeat(16), listOf("aa".repeat(16)), ""))
    }

    @Implementation(methodName = "listSessions")
    internal fun listSessions(period: PhoneControlPeriod, now: Long, controllerId: String, after: String?, limit: Int,
        interaction: Boolean, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.SessionList> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        val remaining = PhoneScreenFixture.sessionRows.filter { after == null || it.subjectId > after }
        val rows = remaining.take(limit)
        val next = if (remaining.size > limit) rows.last().subjectId else ""
        return PhoneControlResult.Value(PhoneControlReply.SessionList(controllerId, "88".repeat(16), rows, next))
    }

    @Implementation(methodName = "readActionHistoryPage")
    internal fun readActionHistoryPage(period: PhoneControlPeriod, now: Long, after: String?, limit: Int,
        maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.ActionHistoryPage> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        PhoneScreenFixture.actionHistoryReads++
        PhoneScreenFixture.actionHistoryCursors += after
        val record = PhoneActionRecord((if (after == null) "12" else "34").repeat(16), "aa".repeat(32), "ab".repeat(16), "cd".repeat(16),
            3, "23".repeat(32), "34".repeat(16), 1, 2, 100, 101, PhoneActionState.UNKNOWN, "", "", "", "")
        val next = if (after == null && PhoneScreenFixture.moreActionHistory) record.operationId else ""
        return PhoneControlResult.Value(PhoneControlReply.ActionHistoryPage("88".repeat(16), listOf(record), next))
    }

    @Implementation(methodName = "sendPrompt")
    internal fun sendPrompt(period: PhoneControlPeriod, target: PhoneControlTarget, revision: Long, text: String,
        store: PendingPhoneActionStore, now: Long, maySend: () -> Boolean): PhoneControlResult<PhoneSendAttempt> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        PhoneScreenFixture.sendFailure?.let { return PhoneControlResult.Failure(it) }
        val request = PhoneControlRequest(period.grant.grantId, period.grant.revision, period.periodId,
            period.grant.receiverIdentity, target.controllerId, target.subjectId, phoneControlSendPayload(text),
            "99".repeat(16), revision, true, 3)
        assertEquals(PendingReservation.NEW, store.reserve(request))
        PhoneScreenFixture.sendCalls++
        return PhoneControlResult.Value(PhoneSendAttempt(store.pending().single(), PhoneScreenFixture.sendOutcome))
    }

    @Implementation(methodName = "readActionHistory")
    internal fun readActionHistory(period: PhoneControlPeriod, pending: PendingPhoneAction, now: Long,
        maySend: () -> Boolean): PhoneControlResult<PhoneActionRecord> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        PhoneScreenFixture.evidenceReads++
        PhoneScreenFixture.evidenceReadOnMain = PhoneScreenFixture.evidenceReadOnMain || Looper.myLooper() == Looper.getMainLooper()
        PhoneScreenFixture.evidenceOperationIds.add(pending.operationId)
        val state = PhoneScreenFixture.evidenceState ?: return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        return PhoneControlResult.Value(PhoneActionRecord(pending.operationId, pending.receiverIdentity, pending.controllerId,
            pending.subjectId, pending.action, pending.payloadHash, pending.grantId, pending.grantRevision,
            pending.expectedRevision, 1, 2, state, if (state == PhoneActionState.UNKNOWN) "" else "66".repeat(32),
            if (state in setOf(PhoneActionState.ACCEPTED, PhoneActionState.DISPATCHED, PhoneActionState.COMPLETED)) "66".repeat(32) else "",
            if (state in setOf(PhoneActionState.DISPATCHED, PhoneActionState.COMPLETED)) "66".repeat(32) else "",
            if (state == PhoneActionState.COMPLETED) "66".repeat(32) else ""))
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

@Implements(PhoneOfflineUnlockStore::class, isInAndroidSdk = false)
class PhoneScreenOfflineUnlockFixture {
    @Implementation(methodName = "prepare")
    internal fun prepare(): PreparedPhoneOfflineUnlock {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(PhoneScreenFixture.offlineMaterial, "AES"))
        return PreparedPhoneOfflineUnlock(cipher, true, ByteArray(32))
    }

    @Implementation(methodName = "finish")
    internal fun finish(prepared: PreparedPhoneOfflineUnlock, cipher: Cipher, startedAtMs: Long): PhoneOfflinePeriod {
        check(prepared.operation === cipher)
        return PhoneOfflinePeriod(PhoneScreenFixture.offlineMaterial, startedAtMs)
    }
}

@Implements(PhoneOfflineAuthenticator::class, isInAndroidSdk = false)
class PhoneScreenOfflineAuthenticatorFixture {
    @Implementation(methodName = "start")
    internal fun start(cipher: Cipher, success: (Cipher?) -> Unit, error: (Boolean) -> Unit): CancellationSignal {
        success(cipher)
        return CancellationSignal()
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30, 34], shadows = [PhoneScreenClientFixture::class, PhoneScreenAuthenticatorFixture::class, PhoneScreenPairingFixture::class,
    PhoneScreenOfflineUnlockFixture::class, PhoneScreenOfflineAuthenticatorFixture::class],
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
        if (PhoneScreenFixture.preUnlockOffline) {
            // The offline-unlock screen has its own lifecycle tests; keep these cases focused on online callbacks.
            ReflectionHelpers.setField(activity, "offlinePeriod", PhoneOfflinePeriod(PhoneScreenFixture.offlineMaterial, SystemClock.elapsedRealtime()))
        }
    }

    @After
    fun close() {
        if (!activity.isDestroyed) controller.close()
        executor.runAll()
        shadowOf(Looper.getMainLooper()).idle()
        executor.shutdownNow()
    }

    private fun drain() { executor.runAll(); shadowOf(Looper.getMainLooper()).idle() }
    private fun drainBackground() {
        var failure: Throwable? = null
        val thread = Thread { try { executor.runAll() } catch (error: Throwable) { failure = error } }
        thread.start()
        thread.join()
        failure?.let { throw AssertionError(it) }
        shadowOf(Looper.getMainLooper()).idle()
    }
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
    fun preparedDraftIsExplicitlyOpenedAndClearedOnLock() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        controller.close()
        create()
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(5) { drain() }
        buttonText("Prepared Runs").performClick()
        repeat(2) { drain() }
        assertEquals(0, PhoneScreenFixture.preparedReads)
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>().any {
            it.text.toString().contains("private prepared instruction")
        })
        val row = views(activity.window.decorView).filterIsInstance<Button>().single {
            it.text.toString().contains(PhoneScreenFixture.preparedSummary.runId)
        }
        row.performClick()
        repeat(2) { drain() }
        assertEquals(1, PhoneScreenFixture.preparedReads)
        assertTrue(views(activity.window.decorView).filterIsInstance<TextView>().any {
            it.text.toString().contains("private prepared instruction")
        })
        assertTrue(views(activity.window.decorView).filterIsInstance<TextView>().any {
            it.text.toString().contains("does not start")
        })
        val displayed = views(activity.window.decorView).filterIsInstance<TextView>().joinToString("\n") { it.text.toString() }
        for (value in listOf("6", PhoneScreenFixture.preparedDigest, "127.0.0.1", "gpt-5.6-luna")) assertTrue(displayed.contains(value))
        assertTrue(displayed.replace(",", "").replace(" ", "").replace("\u00a0", "").contains("1800000"))
        assertEquals(0, PhoneScreenFixture.sendCalls)
        screenOff()
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>().any {
            it.text.toString().contains("private prepared instruction")
        })
    }

    @Test
    fun preparedResumeRechecksWithoutRenewingAndExpiryClearsContent() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        controller.close()
        create()
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(5) { drain() }
        buttonText("Prepared Runs").performClick()
        repeat(2) { drain() }
        views(activity.window.decorView).filterIsInstance<Button>().single {
            it.text.toString().contains(PhoneScreenFixture.preparedSummary.runId)
        }.performClick()
        repeat(2) { drain() }
        controller.pause()
        controller.resume()
        repeat(5) { drain() }
        assertEquals(false, PhoneScreenFixture.preparedInteractions.last())
        assertTrue(views(activity.window.decorView).filterIsInstance<TextView>().any {
            it.text.toString().contains("private prepared instruction")
        })
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMinutes(5))
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>().any {
            it.text.toString().contains("private prepared instruction")
        })
        assertTrue(hasText(R.string.phone_expired))
    }

    @Test
    fun unsupportedPreparedReadsAndLateDetailNeverExposeContent() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        controller.close()
        create()
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(5) { drain() }
        PhoneScreenFixture.preparedFailure = PhoneControlOutcome.UNSUPPORTED
        buttonText("Prepared Runs").performClick()
        repeat(2) { drain() }
        assertTrue(hasText(R.string.phone_unsupported))
        PhoneScreenFixture.preparedFailure = null
        buttonText("Prepared Runs").performClick()
        repeat(2) { drain() }
        views(activity.window.decorView).filterIsInstance<Button>().single {
            it.text.toString().contains(PhoneScreenFixture.preparedSummary.runId)
        }.performClick()
        screenOff()
        repeat(2) { drain() }
        assertEquals(0, PhoneScreenFixture.preparedReads)
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>().any {
            it.text.toString().contains("private prepared instruction")
        })
    }

    @Test
    fun explicitSendKeepsInputAndRequiresOwnerFenceBeforeRetry() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        val intent = Intent(activity, PhoneControlActivity::class.java)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, "aa".repeat(16))
            .putExtra(PhoneControlActivity.SUBJECT_ID, "bb".repeat(16))
        controller.close()
        create(intent)
        val snapshot = File(activity.noBackupFilesDir, "phone-action-pending-v1")
        assertTrue(!snapshot.exists() || snapshot.delete())
        val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        var initialized = false
        val store = PendingPhoneActionStore(activity, { create ->
            if (create) initialized = true
            if (initialized) secret else null
        })
        ReflectionHelpers.setField(activity, "pendingActionsStore", store)
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(5) { drain() }
        val input = views(activity.window.decorView).filterIsInstance<EditText>().single()
        input.setText("é".repeat(3000))
        button(R.string.phone_send).performClick()
        assertTrue(hasText(R.string.phone_send_invalid))
        assertEquals(0, PhoneScreenFixture.sendCalls)
        input.setText("retain this instruction")
        button(R.string.phone_send).performClick()
        drain()
        assertEquals(1, PhoneScreenFixture.sendCalls)
        assertEquals("retain this instruction", input.text.toString())
        assertTrue(input.visibility == View.VISIBLE)
        assertFalse(button(R.string.phone_send).isEnabled)
        assertTrue(hasText(R.string.phone_send_unsupported))
        button(R.string.phone_send).performClick()
        drain()
        assertEquals(1, PhoneScreenFixture.sendCalls)
        PhoneScreenFixture.evidenceState = PhoneActionState.FENCED_NO_EFFECT
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
        repeat(2) { drain() }
        assertFalse(button(R.string.phone_send).isEnabled)
        assertEquals("retain this instruction", input.text.toString())
        button(R.string.phone_refresh).performClick()
        repeat(2) { drain() }
        assertTrue(button(R.string.phone_send).isEnabled)
        screenOff()
        assertEquals(View.GONE, input.visibility)
        assertEquals("retain this instruction", input.text.toString())
    }

    @Test
    fun reopeningReloadsPendingIdentityBeforeAllowingAnotherSend() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        val intent = Intent(activity, PhoneControlActivity::class.java)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, "aa".repeat(16))
            .putExtra(PhoneControlActivity.SUBJECT_ID, "bb".repeat(16))
        controller.close()
        create(intent)
        val snapshot = File(activity.noBackupFilesDir, "phone-action-pending-v1")
        assertTrue(!snapshot.exists() || snapshot.delete())
        val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        var initialized = false
        val store = PendingPhoneActionStore(activity, { create ->
            if (create) initialized = true
            if (initialized) secret else null
        })
        ReflectionHelpers.setField(activity, "pendingActionsStore", store)
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(5) { drain() }
        val input = views(activity.window.decorView).filterIsInstance<EditText>().single()
        input.setText("still under review")
        assertTrue(button(R.string.phone_send).isEnabled)
        controller.pause()
        val current = requireNotNull(PhoneScreenFixture.lastPeriod)
        val request = PhoneControlRequest(current.grant.grantId, current.grant.revision, current.periodId,
            current.grant.receiverIdentity, "aa".repeat(16), "bb".repeat(16), phoneControlSendPayload("earlier instruction"),
            "98".repeat(16), 1, true, 3)
        assertEquals(PendingReservation.NEW, store.reserve(request))
        PhoneScreenFixture.evidenceState = PhoneActionState.UNKNOWN
        controller.resume()
        drain()
        drain()
        assertFalse(button(R.string.phone_send).isEnabled)
        drain()
        assertFalse(button(R.string.phone_send).isEnabled)
        assertEquals(0, PhoneScreenFixture.sendCalls)
        assertTrue(PhoneScreenFixture.evidenceReads > 0)
        val deadline = current.remainingMs(SystemClock.elapsedRealtime()) + SystemClock.elapsedRealtime()
        val reads = PhoneScreenFixture.evidenceReads
        controller.pause()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(20))
        drain()
        assertEquals(reads, PhoneScreenFixture.evidenceReads)
        assertEquals(deadline, current.remainingMs(SystemClock.elapsedRealtime()) + SystemClock.elapsedRealtime())
    }

    @Test
    fun expiredSendLocksWorkspaceAndPreservesInput() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        val intent = Intent(activity, PhoneControlActivity::class.java)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, "aa".repeat(16))
            .putExtra(PhoneControlActivity.SUBJECT_ID, "bb".repeat(16))
        controller.close()
        create(intent)
        val snapshot = File(activity.noBackupFilesDir, "phone-action-pending-v1")
        assertTrue(!snapshot.exists() || snapshot.delete())
        ReflectionHelpers.setField(activity, "pendingActionsStore", PendingPhoneActionStore(activity, { null }))
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(5) { drain() }
        val input = views(activity.window.decorView).filterIsInstance<EditText>().single()
        input.setText("do not lose this")
        PhoneScreenFixture.sendFailure = PhoneControlOutcome.EXPIRED
        button(R.string.phone_send).performClick()
        drain()
        assertEquals(View.GONE, input.visibility)
        assertEquals("do not lose this", input.text.toString())
        assertTrue(hasText(R.string.phone_expired))
        assertFalse(requireNotNull(PhoneScreenFixture.lastPeriod).isActive(SystemClock.elapsedRealtime()))
        assertEquals(0, PhoneScreenFixture.sendCalls)
    }

    @Test
    fun lostAcknowledgementReconcilesAfterRestartWithoutResending() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        PhoneScreenFixture.sendOutcome = PhoneControlOutcome.UNAVAILABLE
        val intent = Intent(activity, PhoneControlActivity::class.java)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, "aa".repeat(16))
            .putExtra(PhoneControlActivity.SUBJECT_ID, "bb".repeat(16))
        controller.close()
        create(intent)
        val snapshot = File(activity.noBackupFilesDir, "phone-action-pending-v1")
        assertTrue(!snapshot.exists() || snapshot.delete())
        val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        var initialized = false
        val store = PendingPhoneActionStore(activity, { create ->
            if (create) initialized = true
            if (initialized) secret else null
        })
        ReflectionHelpers.setField(activity, "pendingActionsStore", store)
        button(R.string.phone_unlock).performClick()
        drainBackground()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(5) { drainBackground() }
        val input = views(activity.window.decorView).filterIsInstance<EditText>().single()
        input.setText("deliver once")
        button(R.string.phone_send).performClick()
        repeat(3) { drainBackground() }
        assertEquals(1, PhoneScreenFixture.sendCalls)
        assertEquals("deliver once", input.text.toString())
        assertFalse(button(R.string.phone_send).isEnabled)
        PhoneScreenFixture.evidenceState = PhoneActionState.ACCEPTED
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
        repeat(4) { drainBackground() }
        assertTrue(hasText(R.string.phone_action_accepted))
        assertFalse(button(R.string.phone_send).isEnabled)
        assertFalse(PhoneScreenFixture.evidenceReadOnMain)
        PhoneScreenFixture.evidenceState = PhoneActionState.DISPATCHED
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
        repeat(4) { drainBackground() }
        assertTrue(hasText(R.string.phone_action_dispatched))
        assertFalse(button(R.string.phone_send).isEnabled)
        PhoneScreenFixture.evidenceState = PhoneActionState.COMPLETED
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
        repeat(4) { drainBackground() }
        assertTrue(hasText(R.string.phone_action_completed))
        assertFalse(button(R.string.phone_send).isEnabled)
        controller.close()
        create(intent)
        ReflectionHelpers.setField(activity, "pendingActionsStore", store)
        button(R.string.phone_unlock).performClick()
        drainBackground()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(7) { drainBackground() }
        val reopened = views(activity.window.decorView).filterIsInstance<EditText>().single()
        reopened.setText("deliver once")
        assertTrue(hasText(R.string.phone_action_completed))
        assertFalse(button(R.string.phone_send).isEnabled)
        assertEquals(1, PhoneScreenFixture.sendCalls)
    }

    @Test
    fun reconciliationRotatesThroughBoundedBatchesAndStopsOnLock() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        PhoneScreenFixture.evidenceState = PhoneActionState.UNKNOWN
        val intent = Intent(activity, PhoneControlActivity::class.java)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, "aa".repeat(16))
            .putExtra(PhoneControlActivity.SUBJECT_ID, "bb".repeat(16))
        controller.close()
        create(intent)
        val snapshot = File(activity.noBackupFilesDir, "phone-action-pending-v1")
        assertTrue(!snapshot.exists() || snapshot.delete())
        val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        var initialized = false
        val store = PendingPhoneActionStore(activity, { create ->
            if (create) initialized = true
            if (initialized) secret else null
        })
        val grant = PhoneScreenFixture.grant
        for (index in 1..7) {
            val request = PhoneControlRequest(grant.grantId, grant.revision, "77".repeat(16), grant.receiverIdentity,
                "aa".repeat(16), "bb".repeat(16), phoneControlSendPayload("instruction $index"),
                "%02x".format(index).repeat(16), 1, true, 3)
            assertEquals(PendingReservation.NEW, store.reserve(request))
        }
        ReflectionHelpers.setField(activity, "pendingActionsStore", store)
        button(R.string.phone_unlock).performClick()
        drainBackground()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(5) { drainBackground() }
        assertEquals(4, PhoneScreenFixture.evidenceReads)
        val current = requireNotNull(PhoneScreenFixture.lastPeriod)
        val deadline = current.remainingMs(SystemClock.elapsedRealtime()) + SystemClock.elapsedRealtime()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(5))
        repeat(4) { drainBackground() }
        assertEquals(8, PhoneScreenFixture.evidenceReads)
        assertEquals(7, PhoneScreenFixture.evidenceOperationIds.toSet().size)
        assertEquals(deadline, current.remainingMs(SystemClock.elapsedRealtime()) + SystemClock.elapsedRealtime())
        val reads = PhoneScreenFixture.evidenceReads
        screenOff()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(20))
        repeat(2) { drainBackground() }
        assertEquals(reads, PhoneScreenFixture.evidenceReads)
        assertEquals(0, PhoneScreenFixture.sendCalls)
        assertFalse(PhoneScreenFixture.evidenceReadOnMain)
    }

    @Test
    fun actionHistoryIsAvailableWithoutALiveSessionAndIsHiddenOnLock() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        PhoneScreenFixture.moreActionHistory = true
        controller.close()
        create()
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(3) { drain() }
        val period = requireNotNull(PhoneScreenFixture.lastPeriod)
        val now = SystemClock.elapsedRealtime()
        val remaining = period.remainingMs(now)
        button(R.string.phone_action_history).performClick()
        drain()
        assertEquals(1, PhoneScreenFixture.actionHistoryReads)
        assertEquals(remaining, period.remainingMs(now))
        assertTrue(views(activity.window.decorView).filterIsInstance<TextView>()
            .any { it.text.toString().contains("12".repeat(16)) && it.text.toString().contains(activity.getString(R.string.phone_action_unknown)) })
        button(R.string.phone_more_actions).performClick()
        drain()
        assertEquals(listOf(null, "12".repeat(16)), PhoneScreenFixture.actionHistoryCursors)
        assertTrue(views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains("34".repeat(16)) })
        screenOff()
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>()
            .any { it.text.toString().contains("34".repeat(16)) })
    }

    @Test
    fun screenLockFencesACompletedActionHistoryRead() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        controller.close()
        create()
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(3) { drain() }
        button(R.string.phone_action_history).performClick()
        executor.runAll()
        assertEquals(1, PhoneScreenFixture.actionHistoryReads)
        screenOff()
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains("12".repeat(16)) })
    }

    @Test
    fun browsesSessionsWithoutAttentionAndOpensTheExactIdenticalName() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        PhoneScreenFixture.sessionRows = listOf(
            PhoneControlSessionSummary("01".repeat(16), "same", "mock", "/work", "ready", false),
            PhoneControlSessionSummary("02".repeat(16), "same", "mock", "/work", "running", false),
        )
        controller.close()
        create()
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(3) { drain() }
        val rows = views(activity.window.decorView).filterIsInstance<Button>()
            .filter { it.text.toString().contains("same") }.toList()
        assertEquals(2, rows.size)
        rows.single { it.text.toString().contains("02".repeat(16)) }.performClick()
        drain()
        assertEquals(PhoneControlTarget("aa".repeat(16), "02".repeat(16)), PhoneScreenFixture.lastTarget)
        assertTrue(views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains("private session text") })
        screenOff()
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains("private session text") })
    }

    @Test
    fun staleSessionRowExplainsHowToRefreshTheSessionList() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        PhoneScreenFixture.sessionRows = listOf(
            PhoneControlSessionSummary("01".repeat(16), "stale row", "mock", "/work", "ready", false),
        )
        controller.close()
        create()
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(3) { drain() }
        PhoneScreenFixture.staleRead = true
        views(activity.window.decorView).filterIsInstance<Button>()
            .single { it.text.toString().contains("stale row") }.performClick()
        drain()
        assertTrue(views(activity.window.decorView).filterIsInstance<TextView>().any {
            it.text.toString() == "This Session is unavailable. Refresh Sessions or the Attention inbox."
        })
    }

    @Test
    fun emptyAndLargeListsStayBoundedAndPageWithoutLosingIdentity() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        controller.close()
        create()
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(3) { drain() }
        assertTrue(hasText(R.string.phone_no_sessions))
        PhoneScreenFixture.sessionRows = (1..52).map {
            PhoneControlSessionSummary("%032x".format(it), "same", "mock", "/work", "ready", false)
        }
        button(R.string.phone_refresh).performClick()
        drain()
        assertEquals(50, views(activity.window.decorView).filterIsInstance<Button>().count { it.text.toString().contains("same") })
        button(R.string.phone_more_sessions).performClick()
        drain()
        val rows = views(activity.window.decorView).filterIsInstance<Button>().filter { it.text.toString().contains("same") }.toList()
        assertEquals(2, rows.size)
        rows.last().performClick()
        drain()
        assertEquals(PhoneControlTarget("aa".repeat(16), "%032x".format(52)), PhoneScreenFixture.lastTarget)
    }

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
    fun openingAnAuthorizedSessionAutomaticallySavesAnOfflineCopy() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        PhoneScreenFixture.preUnlockOffline = false
        val intent = Intent(activity, PhoneControlActivity::class.java)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, "aa".repeat(16))
            .putExtra(PhoneControlActivity.SUBJECT_ID, "bb".repeat(16))
        controller.close()
        create(intent)
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(8) { drain() }
        val store = PhoneOfflineStore(File(activity.filesDir, "phone-offline-v1"))
        val key = SecretKeySpec(PhoneScreenFixture.offlineMaterial, "AES")
        val saved = store.list(PhoneScreenFixture.grant.receiverIdentity, key).copies.single()
        assertEquals(PhoneControlTarget("aa".repeat(16), "bb".repeat(16)), saved.source.target)
        assertEquals("private session text", (store.readPage(saved.source, 0, key).entries.single() as PhoneControlEntry.Message).text)
    }

    @Test
    fun unlockedWorkspaceRefreshesPinsWithoutExtendingEitherPeriod() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        controller.close()
        create()
        val target = PhoneControlTarget("aa".repeat(16), "bb".repeat(16))
        val source = PhoneOfflineSource(PhoneScreenFixture.grant.receiverIdentity, target, "saved", "agent", "project")
        val old = PhoneControlReply.SessionPage(target.controllerId, target.subjectId, "88".repeat(16), 1, "ready",
            listOf(PhoneControlEntry.Message(1, "assistant", "old", false)), 0, true,
            PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false))
        val store = PhoneOfflineStore(File(activity.filesDir, "phone-offline-v1"))
        val key = SecretKeySpec(PhoneScreenFixture.offlineMaterial, "AES")
        assertEquals(PhoneOfflineSave.SAVED, store.save(source, sequenceOf(old), true, 1, 1, 1_000_000, key) { true })
        button(R.string.phone_unlock).performClick()
        drain()
        requireNotNull(PhoneScreenFixture.prompt).invoke(null)
        repeat(3) { drain() }
        val online = requireNotNull(PhoneScreenFixture.lastPeriod)
        val local = ReflectionHelpers.getField<PhoneOfflinePeriod>(activity, "offlinePeriod")
        val onlineBefore = online.remainingMs(SystemClock.elapsedRealtime())
        val localBefore = local.remainingMs(SystemClock.elapsedRealtime())
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(11))
        repeat(3) { drain() }
        val refreshed = store.list(source.receiver, key).copies.single()
        assertEquals(1, refreshed.openedAtMs)
        assertTrue(refreshed.refreshedAtMs > 1)
        assertEquals(Triple(0L, 0L, false), PhoneScreenFixture.readRequests.last())
        assertTrue(online.remainingMs(SystemClock.elapsedRealtime()) <= onlineBefore - 10_000)
        assertTrue(local.remainingMs(SystemClock.elapsedRealtime()) <= localBefore - 10_000)
        assertEquals("private session text", (store.readPage(source, 0, key).entries.single() as PhoneControlEntry.Message).text)
        PhoneScreenFixture.staleRead = true
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(11))
        repeat(3) { drain() }
        assertTrue(store.list(source.receiver, key).copies.single().refreshFailed)
        assertEquals("private session text", (store.readPage(source, 0, key).entries.single() as PhoneControlEntry.Message).text)
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
    fun historyShowsSourceGapsFromOlderPages() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        PhoneScreenFixture.latest = listOf(PhoneControlEntry.Message(2, "assistant", "recent", false))
        PhoneScreenFixture.older = listOf(PhoneControlEntry.Message(1, "user", "shortened", true))
        PhoneScreenFixture.nextBefore = 2
        PhoneScreenFixture.latestSource = PhoneHistorySource("unverified", "unavailable", "unknown", false, false)
        PhoneScreenFixture.olderSource = PhoneHistorySource("unverified", "unavailable", "present", true, true)
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
        for (id in listOf(R.string.phone_incomplete, R.string.phone_archive_unavailable, R.string.phone_replay_unverified,
            R.string.phone_redacted, R.string.phone_missing, R.string.phone_truncated)) {
            assertTrue(displayed.contains(activity.getString(id)))
        }
    }

    @Test
    fun historyCanLoadPastAnEmptyPage() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        PhoneScreenFixture.latest = listOf(PhoneControlEntry.Message(4, "assistant", "recent", false))
        PhoneScreenFixture.nextBefore = 4
        PhoneScreenFixture.olderNextBefore = 3
        PhoneScreenFixture.olderSource = PhoneHistorySource("not_applicable", "unavailable", "unknown", true, false)
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
        assertTrue(views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains("recent") })
        PhoneScreenFixture.older = listOf(PhoneControlEntry.Message(2, "user", "earlier", false))
        PhoneScreenFixture.olderNextBefore = 0
        buttonText("Load older messages").performClick()
        drain()
        assertTrue(views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains("earlier") })
        assertEquals(listOf(4L, 3L), PhoneScreenFixture.readRequests.takeLast(2).map { it.first })
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
    fun passiveRefreshReconcilesMessagesBetweenTheOldAndNewPages() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        PhoneScreenFixture.latest = listOf(PhoneControlEntry.Message(1, "user", "history first", false),
            PhoneControlEntry.Message(2, "assistant", "history second", false))
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

        PhoneScreenFixture.revision = 2
        PhoneScreenFixture.latest = listOf(PhoneControlEntry.Message(5, "user", "history fifth", false),
            PhoneControlEntry.Message(6, "assistant", "history sixth", false))
        PhoneScreenFixture.older = listOf(PhoneControlEntry.Message(1, "user", "history first", false),
            PhoneControlEntry.Message(2, "assistant", "history second", false),
            PhoneControlEntry.Message(3, "user", "history third", false),
            PhoneControlEntry.Message(4, "assistant", "history fourth", false))
        PhoneScreenFixture.nextBefore = 5
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(6))
        drain()
        val displayed = views(activity.window.decorView).filterIsInstance<TextView>().map { it.text.toString() }.toList()
        val positions = listOf("first", "second", "third", "fourth", "fifth", "sixth").map { word ->
            displayed.indexOfFirst { it.contains("history $word") }
        }
        assertTrue(positions.toString(), positions.all { it >= 0 } && positions.zipWithNext().all { (a, b) -> a < b })
        assertEquals(listOf(Triple(0L, 0L, false), Triple(5L, 2L, false)), PhoneScreenFixture.readRequests.takeLast(2))
    }

    @Test
    fun revocationDuringRefreshRemovesAllProtectedContent() {
        PhoneScreenFixture.status = PhoneControlStatus.Granted(PhoneScreenFixture.grant)
        PhoneScreenFixture.latest = listOf(PhoneControlEntry.Message(1, "user", "old private text", false))
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
        PhoneScreenFixture.revision = 2
        PhoneScreenFixture.latest = listOf(PhoneControlEntry.Message(5, "assistant", "new private text", false))
        PhoneScreenFixture.nextBefore = 5
        PhoneScreenFixture.olderOutcome = PhoneControlOutcome.REVOKED
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(6))
        drain()
        assertTrue(hasText(R.string.phone_revoked))
        assertFalse(requireNotNull(PhoneScreenFixture.lastPeriod).isActive(android.os.SystemClock.elapsedRealtime()))
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains("private text") })
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
        repeat(2) { drain() } // Unlock, then load the newly available Session list.
        assertTrue(hasText(R.string.phone_unlocked))
        assertEquals(1, PhoneScreenFixture.sends)
        controller.pause().stop().restart().start().resume()
        repeat(2) { drain() } // Revalidate pairing, then refresh protected content on return.
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
