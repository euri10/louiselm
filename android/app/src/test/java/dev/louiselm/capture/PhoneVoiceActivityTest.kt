package dev.louiselm.capture

import android.Manifest
import android.app.KeyguardManager
import android.content.Intent
import android.content.DialogInterface
import android.os.CancellationSignal
import android.os.Looper
import android.os.PowerManager
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.TextView
import java.io.File
import java.io.RandomAccessFile
import java.security.Signature
import java.time.Duration
import java.util.concurrent.AbstractExecutorService
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.ExecutorService
import java.util.concurrent.TimeUnit
import javax.crypto.Cipher
import javax.crypto.spec.SecretKeySpec
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.android.controller.ActivityController
import org.robolectric.annotation.Config
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements
import org.robolectric.annotation.LooperMode
import org.robolectric.util.ReflectionHelpers
import org.robolectric.shadows.ShadowMediaPlayer
import org.robolectric.shadows.ShadowAlertDialog
import org.robolectric.shadows.util.DataSource

private object VoiceScreenFixture {
    val grant = PhoneControlGrant("11".repeat(16), 1, "aa".repeat(32), "22".repeat(32), "33".repeat(32))
    val target = PhoneControlTarget("44".repeat(16), "55".repeat(16))
    var browsedTarget = target
    val working = phoneControlWorkingKey()
    val material = ByteArray(32) { 7 }
    var sends = 0
    var archive: PhoneVoiceTranscript? = null
    var nextArchivePage: PhoneVoicePage? = null
    var archivePageFailure: PhoneControlOutcome? = null
    var voiceAccepted = false
    var voiceSubmissions = 0
    var voiceCancellations = 0
    var voiceRetries = 0
    var backgroundReads = 0
}

@Implements(PhoneVoiceJobClient::class, isInAndroidSdk = false)
class VoiceScreenJobClient {
    @Implementation(methodName = "submit")
    internal fun submit(period: PhoneControlPeriod, record: PhoneVoiceRecord, maySend: () -> Boolean): PhoneControlResult<String> {
        VoiceScreenFixture.voiceSubmissions++
        return if (VoiceScreenFixture.voiceAccepted && maySend()) PhoneControlResult.Value("pending")
            else PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
    }

    @Implementation(methodName = "cancel")
    internal fun cancel(period: PhoneControlPeriod, record: PhoneVoiceRecord, maySend: () -> Boolean): PhoneControlResult<String> {
        VoiceScreenFixture.voiceCancellations++
        return if (VoiceScreenFixture.voiceAccepted && maySend()) PhoneControlResult.Value("cancelled")
            else PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
    }

    @Implementation(methodName = "retry")
    internal fun retry(period: PhoneControlPeriod, record: PhoneVoiceRecord, maySend: () -> Boolean): PhoneControlResult<String> {
        VoiceScreenFixture.voiceRetries++
        return if (VoiceScreenFixture.voiceAccepted && maySend()) PhoneControlResult.Value("pending")
            else PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
    }
}

@Implements(PhoneControlClient::class, isInAndroidSdk = false)
class VoiceScreenClient {
    @Implementation(methodName = "voiceHistory")
    internal fun voiceHistory(period: PhoneControlPeriod, now: Long, after: String?, interaction: Boolean,
        maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.VoiceHistory> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        return PhoneControlResult.Value(PhoneControlReply.VoiceHistory("99".repeat(16), listOfNotNull(VoiceScreenFixture.archive?.row), ""))
    }

    @Implementation(methodName = "voiceResult")
    internal fun voiceResult(period: PhoneControlPeriod, row: PhoneVoiceRow, now: Long, interaction: Boolean,
        maySend: () -> Boolean): PhoneControlResult<PhoneVoiceTranscript> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        if (!interaction) VoiceScreenFixture.backgroundReads++
        return VoiceScreenFixture.archive?.takeIf { it.row.id == row.id && it.row.target == row.target }?.let { PhoneControlResult.Value(it) }
            ?: PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
    }

    @Implementation(methodName = "voiceResultPage")
    internal fun voiceResultPage(period: PhoneControlPeriod, transcript: PhoneVoiceTranscript, offset: Int, now: Long,
        maySend: () -> Boolean): PhoneControlResult<PhoneVoicePage> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        VoiceScreenFixture.archivePageFailure?.let { return PhoneControlResult.Failure(it) }
        return VoiceScreenFixture.nextArchivePage?.takeIf { it.offset == offset }?.let { PhoneControlResult.Value(it) }
            ?: PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
    }

    @Implementation(methodName = "status")
    internal fun status(): PhoneControlResult<PhoneControlStatus> = PhoneControlResult.Value(PhoneControlStatus.Granted(VoiceScreenFixture.grant))

    @Implementation(methodName = "prepareUnlock")
    internal fun prepareUnlock(maySend: () -> Boolean): PhoneControlResult<PreparedPhoneControlUnlock> {
        if (!maySend()) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        val grant = VoiceScreenFixture.grant
        val proof = PhoneControlProof(grant.grantId, 1, grant.keyHash, grant.receiverIdentity, "66".repeat(16), "77".repeat(32),
            phoneControlHex(phoneControlPublicKey(VoiceScreenFixture.working.public)))
        val signature = Signature.getInstance("SHA256withECDSA").apply { initSign(VoiceScreenFixture.working.private) }
        return PhoneControlResult.Value(PreparedPhoneControlUnlock(grant, proof, VoiceScreenFixture.working, signature))
    }

    @Implementation(methodName = "finishUnlock")
    internal fun finishUnlock(prepared: PreparedPhoneControlUnlock, signature: ByteArray, startedAtMs: Long,
        maySend: () -> Boolean): PhoneControlResult<PhoneControlPeriod> {
        if (!maySend()) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        val privateKey = prepared.takeWorkingKey() ?: return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        return PhoneControlResult.Value(PhoneControlPeriod(prepared.grant, "88".repeat(16), privateKey, startedAtMs))
    }

    @Implementation(methodName = "readSession")
    internal fun readSession(period: PhoneControlPeriod, target: PhoneControlTarget, now: Long, before: Long,
        limit: Int, expectedRevision: Long, interaction: Boolean, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.SessionPage> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        return PhoneControlResult.Value(PhoneControlReply.SessionPage(target.controllerId, target.subjectId, "99".repeat(16), 1,
            "ready", emptyList(), 0, true, PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false)))
    }

    @Implementation(methodName = "listControllers")
    internal fun listControllers(period: PhoneControlPeriod, now: Long, after: String?, limit: Int,
        interaction: Boolean, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.ControllerList> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        return PhoneControlResult.Value(PhoneControlReply.ControllerList("99".repeat(16), listOf(VoiceScreenFixture.browsedTarget.controllerId), ""))
    }

    @Implementation(methodName = "listSessions")
    internal fun listSessions(period: PhoneControlPeriod, now: Long, controllerId: String, after: String?, limit: Int,
        interaction: Boolean, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.SessionList> {
        if (!maySend() || !period.isActive(now)) return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
        return PhoneControlResult.Value(PhoneControlReply.SessionList(controllerId, "99".repeat(16), listOf(
            PhoneControlSessionSummary(VoiceScreenFixture.browsedTarget.subjectId, "named session", "agent", "/project", "ready", false)), ""))
    }

    @Implementation(methodName = "sendPrompt")
    internal fun sendPrompt(period: PhoneControlPeriod, target: PhoneControlTarget, revision: Long, text: String,
        store: PendingPhoneActionStore, now: Long, maySend: () -> Boolean): PhoneControlResult<PhoneSendAttempt> {
        VoiceScreenFixture.sends++
        return PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
    }
}

@Implements(PairingStore::class, isInAndroidSdk = false)
class VoiceScreenPairing {
    @Implementation fun binding(): String = "fixture"
    @Implementation(methodName = "load") internal fun load() = PairingConfig("https://example.invalid", VoiceScreenFixture.grant.receiverIdentity, "device", "credential")
}

@Implements(PhoneControlAuthenticator::class, isInAndroidSdk = false)
class VoiceScreenAuthenticator {
    @Implementation fun start(signing: Signature, success: (Signature?) -> Unit, error: (Boolean) -> Unit): CancellationSignal {
        success(signing)
        return CancellationSignal()
    }
}

@Implements(PhoneOfflineUnlockStore::class, isInAndroidSdk = false)
class VoiceScreenUnlock {
    @Implementation(methodName = "prepare") internal fun prepare(): PreparedPhoneOfflineUnlock {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(VoiceScreenFixture.material, "AES"))
        return PreparedPhoneOfflineUnlock(cipher, true, ByteArray(32))
    }
    @Implementation(methodName = "finish") internal fun finish(prepared: PreparedPhoneOfflineUnlock, cipher: Cipher, startedAtMs: Long): PhoneOfflinePeriod {
        check(prepared.operation === cipher)
        return PhoneOfflinePeriod(VoiceScreenFixture.material, startedAtMs)
    }
}

@Implements(PhoneOfflineAuthenticator::class, isInAndroidSdk = false)
class VoiceScreenOfflineAuthenticator {
    @Implementation fun start(cipher: Cipher, success: (Cipher?) -> Unit, error: (Boolean) -> Unit): CancellationSignal {
        success(cipher)
        return CancellationSignal()
    }
}

private class VoiceBackground : AbstractExecutorService() {
    private val queue = ConcurrentLinkedQueue<Runnable>()
    private var stopped = false
    override fun execute(command: Runnable) { check(!stopped); queue.add(command) }
    override fun shutdown() { stopped = true }
    override fun shutdownNow(): MutableList<Runnable> { stopped = true; return mutableListOf() }
    override fun isShutdown() = stopped
    override fun isTerminated() = stopped && queue.isEmpty()
    override fun awaitTermination(timeout: Long, unit: TimeUnit) = isTerminated
    fun runQueued() {
        while (true) {
            val task = queue.poll() ?: break
            var failure: Throwable? = null
            val thread = Thread { try { task.run() } catch (error: Throwable) { failure = error } }
            thread.start(); thread.join()
            failure?.let { throw AssertionError(it) }
        }
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30], shadows = [VoiceScreenClient::class, VoiceScreenPairing::class, VoiceScreenAuthenticator::class,
    VoiceScreenUnlock::class, VoiceScreenOfflineAuthenticator::class, VoiceScreenJobClient::class], instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneVoiceActivityTest {
    private lateinit var screen: ActivityController<PhoneControlActivity>
    private lateinit var activity: PhoneControlActivity
    private lateinit var executor: VoiceBackground

    @Before fun setup() {
        val app = RuntimeEnvironment.getApplication()
        VoiceScreenFixture.sends = 0
        VoiceScreenFixture.voiceAccepted = false
        VoiceScreenFixture.voiceSubmissions = 0
        VoiceScreenFixture.voiceCancellations = 0
        VoiceScreenFixture.voiceRetries = 0
        VoiceScreenFixture.backgroundReads = 0
        VoiceScreenFixture.archive = null
        VoiceScreenFixture.nextArchivePage = null
        VoiceScreenFixture.archivePageFailure = null
        VoiceScreenFixture.browsedTarget = VoiceScreenFixture.target
        shadowOf(app).grantPermissions(Manifest.permission.RECORD_AUDIO, Manifest.permission.ACCESS_LOCAL_NETWORK,
            "${app.packageName}.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION")
        shadowOf(app.getSystemService(PowerManager::class.java)).turnScreenOn(true)
        shadowOf(app.getSystemService(KeyguardManager::class.java)).setIsDeviceLocked(false)
        create()
        button("Unlock workspace").performClick()
        drain()
    }

    private fun create(target: PhoneControlTarget? = VoiceScreenFixture.target) {
        val app = RuntimeEnvironment.getApplication()
        val intent = Intent(app, PhoneControlActivity::class.java)
        if (target != null) intent.putExtra(PhoneControlActivity.ATTENTION, true)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, target.controllerId)
            .putExtra(PhoneControlActivity.SUBJECT_ID, target.subjectId)
        screen = Robolectric.buildActivity(PhoneControlActivity::class.java, intent)
        activity = screen.get()
        ReflectionHelpers.getField<ExecutorService>(activity, "networkExecutor").shutdown()
        executor = VoiceBackground()
        ReflectionHelpers.setField(activity, "networkExecutor", executor)
        screen.setup()
        ReflectionHelpers.setField(activity, "voiceStore", PhoneVoiceStore(activity) { SecretKeySpec(VoiceScreenFixture.material, "AES") })
        drain()
    }

    @After fun close() {
        if (!activity.isDestroyed) screen.close()
        executor.runQueued()
        shadowOf(Looper.getMainLooper()).idle()
    }

    @Test fun stopSavesTheExactSessionWithoutSending() {
        assertTrue(views().filterIsInstance<TextView>().any { it.isShown && it.text.toString().contains("OpenAI") })
        val start = button("Record voice instruction")
        assertTrue(views().filterIsInstance<TextView>().joinToString(" | ") { "${it.text}(${it.isShown})" }, start.isShown && start.isEnabled)
        start.performClick()
        drain()
        val metadata = File(activity.filesDir, "voice-captures-v1").listFiles().orEmpty().single { it.name.startsWith(".recording-") && it.name.endsWith(".json") }
        val audio = File(metadata.parentFile, metadata.name.removeSuffix(".json") + ".m4a")
        audio.writeBytes(byteArrayOf(1, 2, 3, 4))
        button("Stop voice recording").performClick()
        drain()
        val record = PhoneVoiceStore(activity) { SecretKeySpec(VoiceScreenFixture.material, "AES") }.list().records.single()
        assertEquals(VoiceScreenFixture.target, record.source.target)
        assertEquals(PhoneVoiceIntent.REQUESTED, record.intent)
        assertEquals(listOf<Byte>(1, 2, 3, 4), requireNotNull(record.audio).audio.readBytes().toList())
        assertTrue(views().filterIsInstance<TextView>().any { it.text.toString().contains("not available") })
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun stopAdmitsTheOriginalAndDiscardRequestsCancellationWithoutSending() {
        VoiceScreenFixture.voiceAccepted = true
        val id = beginWithDummyAudio()
        button("Stop voice recording").performClick()
        drain()
        assertEquals(PhoneVoiceIntent.ACCEPTED, recordings().single { it.id == id }.intent)
        assertEquals(1, VoiceScreenFixture.voiceSubmissions)
        button("Discard draft").performClick()
        ShadowAlertDialog.getLatestAlertDialog().getButton(DialogInterface.BUTTON_POSITIVE).performClick()
        drain()
        assertEquals(PhoneVoiceIntent.CANCELLED, recordings().single { it.id == id }.intent)
        assertEquals(1, VoiceScreenFixture.voiceCancellations)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun recordingMustFinishBeforeItsDraftCanBeDiscarded() {
        beginWithDummyAudio()
        assertFalse(button("Discard draft").isEnabled)
        button("Stop voice recording").performClick()
        drain()
        assertTrue(button("Discard draft").isEnabled)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun failedJobNeedsAnExplicitRetryFromProtectedHistory() {
        VoiceScreenFixture.voiceAccepted = true
        val id = beginWithDummyAudio()
        button("Stop voice recording").performClick()
        drain()
        val record = recordings().single { it.id == id }
        val row = PhoneVoiceRow(id, record.source.target, record.source.project, record.source.agent, false, "failed", "fixture failure")
        VoiceScreenFixture.archive = PhoneVoiceTranscript(row, record.source.receiver, record.draftId,
            record.draftRevision, 0, null, PhoneVoicePage(0, 0, ""))
        button("Desktop voice history").performClick()
        drain()
        assertEquals(0, VoiceScreenFixture.voiceRetries)
        button("Retry transcription").performClick()
        drain()
        assertEquals(1, VoiceScreenFixture.voiceRetries)
        assertEquals(PhoneVoiceIntent.ACCEPTED, recordings().single { it.id == id }.intent)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun completedVoiceAutomaticallyFillsOnlyTheUntouchedDraftWithoutRenewingOrSending() {
        VoiceScreenFixture.voiceAccepted = true
        val id = beginWithDummyAudio()
        button("Stop voice recording").performClick()
        drain()
        val record = recordings().single { it.id == id }
        val row = PhoneVoiceRow(id, record.source.target, record.source.project, record.source.agent, true, "completed", "")
        VoiceScreenFixture.archive = PhoneVoiceTranscript(row, record.source.receiver, record.draftId,
            record.draftRevision, 20, "synthetic transcript", PhoneVoicePage(0, 0, "synthetic transcript"))
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
        drain()
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        val saved = PhoneDraftStore(File(activity.filesDir, "phone-drafts-v1")).list(key).drafts.single()
        assertEquals("synthetic transcript", saved.text)
        assertEquals("synthetic transcript", views().filterIsInstance<android.widget.EditText>().single().text.toString())
        assertTrue(VoiceScreenFixture.backgroundReads > 0)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun aLateResultNeverOverwritesNewerDraftText() {
        VoiceScreenFixture.voiceAccepted = true
        val id = beginWithDummyAudio()
        button("Stop voice recording").performClick()
        drain()
        val record = recordings().single { it.id == id }
        views().filterIsInstance<android.widget.EditText>().single().setText("newer edit")
        drain()
        val row = PhoneVoiceRow(id, record.source.target, record.source.project, record.source.agent, true, "completed", "")
        VoiceScreenFixture.archive = PhoneVoiceTranscript(row, record.source.receiver, record.draftId,
            record.draftRevision, 20, "synthetic transcript", PhoneVoicePage(0, 0, "synthetic transcript"))
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
        drain()
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        assertEquals("newer edit", PhoneDraftStore(File(activity.filesDir, "phone-drafts-v1")).list(key).drafts.single().text)
        assertEquals(PhoneVoiceIntent.READY, recordings().single { it.id == id }.intent)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun pendingStopWaitsForFreshUnlockBeforeResubmission() {
        val id = beginWithDummyAudio()
        button("Stop voice recording").performClick()
        drain()
        assertEquals(PhoneVoiceIntent.REQUESTED, recordings().single { it.id == id }.intent)
        val beforeLock = VoiceScreenFixture.voiceSubmissions
        shadowOf(activity.getSystemService(PowerManager::class.java)).turnScreenOn(false)
        screen.pause().stop()
        VoiceScreenFixture.voiceAccepted = true
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(20))
        drain()
        assertEquals(beforeLock, VoiceScreenFixture.voiceSubmissions)
        shadowOf(activity.getSystemService(PowerManager::class.java)).turnScreenOn(true)
        screen.restart().start().resume()
        drain()
        button("Unlock workspace").performClick()
        drain()
        assertEquals(PhoneVoiceIntent.ACCEPTED, recordings().single { it.id == id }.intent)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun lockBeforeStopRequiresAnExplicitChoiceAfterOfflineUnlockAndRestart() {
        val id = beginWithDummyAudio()
        shadowOf(activity.getSystemService(PowerManager::class.java)).turnScreenOn(false)
        screen.pause().stop()
        drain()
        assertEquals(PhoneVoiceIntent.UNDECIDED, recordings().single().intent)
        assertTrue(views().filterIsInstance<TextView>().none { it.isShown && it.text.toString().contains(id) })
        screen.destroy()
        drain()
        shadowOf(activity.getSystemService(PowerManager::class.java)).turnScreenOn(true)
        create()
        button("Unlock phone drafts").performClick()
        drain()
        button("Session voice recordings").performClick()
        drain()
        assertTrue(views().filterIsInstance<TextView>().any { it.isShown && it.text.toString().contains(id) && it.text.toString().contains("choose") })
        assertEquals(VoiceScreenFixture.target, recordings().single().source.target)
        button("Request transcription when available").performClick()
        drain()
        assertEquals(PhoneVoiceIntent.REQUESTED, recordings().single().intent)
        assertTrue(views().filterIsInstance<TextView>().any { it.isShown && it.text.toString().contains("no job was submitted") })
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun stopBeforeLockKeepsTheExplicitRequestWhenSavingFinishesLater() {
        val id = beginWithDummyAudio()
        button("Stop voice recording").performClick()
        shadowOf(activity.getSystemService(PowerManager::class.java)).turnScreenOn(false)
        screen.pause().stop()
        drain()
        val record = recordings().single()
        assertEquals(id, record.id)
        assertEquals(PhoneVoiceIntent.REQUESTED, record.intent)
        assertEquals(listOf<Byte>(1, 2, 3, 4), requireNotNull(record.audio).audio.readBytes().toList())
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun keepingAnInterruptedOriginalDoesNotPreventARequestLater() {
        beginWithDummyAudio()
        shadowOf(activity.getSystemService(PowerManager::class.java)).turnScreenOn(false)
        screen.pause().stop()
        drain()
        shadowOf(activity.getSystemService(PowerManager::class.java)).turnScreenOn(true)
        screen.restart().start().resume()
        drain()
        button("Unlock phone drafts").performClick()
        drain()
        button("Session voice recordings").performClick()
        drain()
        button("Keep without transcription").performClick()
        drain()
        assertEquals(PhoneVoiceIntent.KEPT, recordings().single().intent)
        button("Request transcription when available").performClick()
        drain()
        assertEquals(PhoneVoiceIntent.REQUESTED, recordings().single().intent)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test @Config(sdk = [37])
    fun recordingStillSavesWhenTheReceiverIsUnavailable() {
        shadowOf(RuntimeEnvironment.getApplication()).denyPermissions(Manifest.permission.ACCESS_LOCAL_NETWORK)
        val id = beginWithDummyAudio()
        button("Stop voice recording").performClick()
        drain()
        val record = recordings().single { it.id == id }
        assertEquals(PhoneVoiceIntent.REQUESTED, record.intent)
        assertEquals(listOf<Byte>(1, 2, 3, 4), requireNotNull(record.audio).audio.readBytes().toList())
        assertTrue(views().filterIsInstance<TextView>().any { it.isShown && it.text.toString().contains("no job was submitted") })
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun destroyedScreenAndReplacementSessionCannotRetargetACompletedRecording() {
        val id = beginWithDummyAudio()
        button("Stop voice recording").performClick()
        executor.runQueued() // Original is durable; its UI completion is still queued.
        screen.pause().stop().destroy()
        val replacement = PhoneControlTarget("66".repeat(16), "77".repeat(16))
        create(replacement)
        button("Unlock workspace").performClick()
        drain()
        val record = recordings().single { it.id == id }
        assertEquals(VoiceScreenFixture.target, record.source.target)
        assertEquals(PhoneVoiceIntent.REQUESTED, record.intent)
        assertTrue(views().filterIsInstance<android.widget.EditText>().single().text.isEmpty())
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun aLateSaveCannotShowTheOldRecordingStatusInAReplacementSession() {
        val id = beginWithDummyAudio()
        button("Stop voice recording").performClick()
        val replacement = PhoneControlTarget("66".repeat(16), "77".repeat(16))
        VoiceScreenFixture.browsedTarget = replacement
        button("Sessions").performClick()
        drain()
        views().filterIsInstance<Button>().single { it.text.toString().contains("named session") }.performClick()
        drain()
        assertEquals(VoiceScreenFixture.target, recordings().single { it.id == id }.source.target)
        assertTrue(views().filterIsInstance<TextView>().none { it.isShown && it.text.toString().contains("Original saved locally") })
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun normalWorkspaceBrowseRecordsTheSelectedSession() {
        screen.close()
        drain()
        create(null)
        button("Unlock workspace").performClick()
        drain()
        views().filterIsInstance<Button>().single { it.text.toString().contains("named session") }.performClick()
        drain()
        val id = beginWithDummyAudio()
        button("Stop voice recording").performClick()
        drain()
        val source = recordings().single { it.id == id }.source
        assertEquals(VoiceScreenFixture.target, source.target)
        assertEquals("named session", source.name)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun staleMicrophonePermissionCannotStartRecordingAfterLockAndUnlock() {
        val app = RuntimeEnvironment.getApplication()
        shadowOf(app).denyPermissions(Manifest.permission.RECORD_AUDIO)
        button("Record voice instruction").performClick()
        val request = requireNotNull(shadowOf(activity).lastRequestedPermission)
        shadowOf(app.getSystemService(PowerManager::class.java)).turnScreenOn(false)
        screen.pause().stop()
        drain()
        shadowOf(app.getSystemService(PowerManager::class.java)).turnScreenOn(true)
        screen.restart().start().resume()
        drain()
        button("Unlock workspace").performClick()
        drain()
        shadowOf(app).grantPermissions(Manifest.permission.RECORD_AUDIO)
        activity.onRequestPermissionsResult(request.requestCode, request.requestedPermissions, intArrayOf(0))
        drain()
        assertTrue(recordings().isEmpty())
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun expiredWorkspaceStopsAndRetainsTheRecordingForAnExplicitChoice() {
        val id = beginWithDummyAudio()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMinutes(6))
        drain()
        val record = recordings().single { it.id == id }
        assertEquals(PhoneVoiceIntent.UNDECIDED, record.intent)
        assertEquals(listOf<Byte>(1, 2, 3, 4), requireNotNull(record.audio).audio.readBytes().toList())
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun destructionBeforeThePreparedRecorderCallbackCannotStartAudio() {
        button("Record voice instruction").performClick()
        executor.runQueued() // The prepared recording is waiting for the Activity callback.
        screen.pause().stop().destroy()
        drain()
        assertTrue(recordings().isEmpty())
        assertTrue(File(activity.filesDir, "voice-captures-v1").listFiles().orEmpty().none { it.name.startsWith(".recording-") })
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun oversizeRecordingIsRetainedAndTheTransferLimitIsVisible() {
        button("Record voice instruction").performClick()
        drain()
        val metadata = File(activity.filesDir, "voice-captures-v1").listFiles().orEmpty()
            .single { it.name.startsWith(".recording-") && it.name.endsWith(".json") }
        val audio = File(metadata.parentFile, metadata.name.removeSuffix(".json") + ".m4a")
        RandomAccessFile(audio, "rw").use { it.setLength(20L * 1024 * 1024 + 1) }
        button("Stop voice recording").performClick()
        drain()
        assertEquals(20L * 1024 * 1024 + 1, requireNotNull(recordings().single().audio).audio.length())
        assertTrue(views().filterIsInstance<TextView>().any { it.isShown && it.text.toString().contains("20 MiB") })
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun originalCanBePlayedOfflineAndPlaybackStopsWhenTheActivityBackgrounds() {
        beginWithDummyAudio()
        button("Stop voice recording").performClick()
        drain()
        val original = requireNotNull(recordings().single().audio).audio
        ShadowMediaPlayer.addMediaInfo(DataSource.toDataSource(original.absolutePath), ShadowMediaPlayer.MediaInfo(10_000, 0))
        var player: ShadowMediaPlayer? = null
        ShadowMediaPlayer.setCreateListener { _, shadow -> player = shadow }
        try {
            button("Session voice recordings").performClick()
            drain()
            button("Play original").performClick()
            drain()
            assertEquals(ShadowMediaPlayer.State.STARTED, requireNotNull(player).state)
            screen.pause().stop()
            assertEquals(ShadowMediaPlayer.State.END, requireNotNull(player).state)
            assertEquals(0, VoiceScreenFixture.sends)
        } finally {
            ShadowMediaPlayer.setCreateListener(null)
        }
    }

    @Test fun fullDraftQuotaRefusesRecordingWithoutRemovingExistingDrafts() {
        screen.close()
        drain()
        val root = File(activity.filesDir, "phone-drafts-v1")
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        val store = PhoneDraftStore(root)
        for (index in 1..128) {
            val source = PhoneOfflineSource(VoiceScreenFixture.grant.receiverIdentity,
                PhoneControlTarget("44".repeat(16), "%032x".format(index)), "saved", "agent", "project")
            assertEquals(PhoneDraftSave.SAVED, store.save(PhoneDraft(source, "retained", emptyList()), key) { true })
        }
        create()
        button("Unlock workspace").performClick()
        drain()
        button("Record voice instruction").performClick()
        drain()
        assertEquals(128, store.list(key).drafts.size)
        assertTrue(recordings().isEmpty())
        assertTrue(views().filterIsInstance<TextView>().any { it.isShown && it.text.toString() == activity.getString(R.string.phone_draft_limit) })
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun anotherActivitysNewerDraftIsNeverOverwrittenWhenRecordingStarts() {
        val source = PhoneOfflineSource(VoiceScreenFixture.grant.receiverIdentity, VoiceScreenFixture.target, "session", "agent", "project")
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        val store = PhoneDraftStore(File(activity.filesDir, "phone-drafts-v1"))
        assertEquals(PhoneDraftSave.SAVED, store.save(PhoneDraft(source, "newer private edit", emptyList()), key) { true })
        button("Record voice instruction").performClick()
        drain()
        assertEquals("newer private edit", store.list(key).drafts.single().text)
        assertTrue(recordings().isEmpty())
        assertTrue(views().filterIsInstance<TextView>().any { it.isShown && it.text.toString().contains("changed elsewhere") })
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun editingDuringRecordingKeepsTheNewerTextAndOriginalDraftRevision() {
        val id = beginWithDummyAudio()
        val recordingRevision = recordings().single { it.id == id }.draftRevision
        val input = views().filterIsInstance<android.widget.EditText>().single()
        input.setText("newer draft while recording")
        drain()
        button("Stop voice recording").performClick()
        drain()
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        val store = PhoneDraftStore(File(activity.filesDir, "phone-drafts-v1"))
        val draft = store.list(key).drafts.single()
        assertEquals("newer draft while recording", draft.text)
        assertTrue(recordingRevision != store.revision(draft, key))
        assertEquals(recordingRevision, recordings().single { it.id == id }.draftRevision)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun aPreviouslySavedDraftMustStillMatchTheVisibleVersion() {
        val input = views().filterIsInstance<android.widget.EditText>().single()
        input.setText("visible version")
        drain()
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        val store = PhoneDraftStore(File(activity.filesDir, "phone-drafts-v1"))
        val original = store.list(key).drafts.single()
        assertEquals(PhoneDraftSave.SAVED, store.save(original.copy(text = "newer version"), key) { true })
        button("Record voice instruction").performClick()
        drain()
        assertEquals("newer version", store.list(key).drafts.single().text)
        assertEquals("visible version", input.text.toString())
        assertTrue(recordings().isEmpty())
        assertTrue(views().filterIsInstance<TextView>().any { it.isShown && it.text.toString().contains("changed elsewhere") })
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun archivedResultFillsOnlyTheExactUntouchedOriginalDraft() {
        val store = prepareArchivedEmptyDraft()
        button("Desktop voice history").performClick()
        drain()
        button("Review transcript").performClick()
        drain()
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        assertEquals("synthetic transcript", store.list(key).drafts.single().text)
        assertTrue(views().filterIsInstance<TextView>().any { it.isShown && it.text.toString().contains("synthetic transcript") })
        button("Open reviewed draft").performClick()
        drain()
        assertEquals("synthetic transcript", views().filterIsInstance<android.widget.EditText>().single().text.toString())
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun changedOriginalNeedsAnExplicitCurrentDestinationAndInsert() {
        val store = prepareArchivedEmptyDraft()
        views().filterIsInstance<android.widget.EditText>().single().setText("newer draft")
        drain()
        button("Desktop voice history").performClick()
        drain()
        button("Review transcript").performClick()
        drain()
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        assertEquals("newer draft", store.list(key).drafts.single().text)
        button("Choose destination Session").performClick()
        drain()
        views().filterIsInstance<Button>().single { it.text.toString().contains("named session") }.performClick()
        drain()
        assertEquals("newer draft", store.list(key).drafts.single().text)
        button("Insert transcript").performClick()
        drain()
        assertEquals("newer draftsynthetic transcript", store.list(key).drafts.single().text)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun discardedOriginalIsNotRecreatedUntilExplicitReplace() {
        val store = prepareArchivedEmptyDraft()
        button("Discard draft").performClick()
        ShadowAlertDialog.getLatestAlertDialog().getButton(DialogInterface.BUTTON_POSITIVE).performClick()
        drain()
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        assertTrue(store.list(key).drafts.isEmpty())
        button("Desktop voice history").performClick()
        drain()
        button("Review transcript").performClick()
        drain()
        assertTrue(store.list(key).drafts.isEmpty())
        button("Choose destination Session").performClick()
        drain()
        views().filterIsInstance<Button>().single { it.text.toString().contains("named session") }.performClick()
        drain()
        button("Replace draft with transcript").performClick()
        drain()
        assertEquals("synthetic transcript", store.list(key).drafts.single().text)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun aConcurrentSavedEditBeatsALateArchivedResult() {
        val store = prepareArchivedEmptyDraft()
        button("Desktop voice history").performClick()
        drain()
        button("Review transcript").performClick()
        executor.runQueued() // Deliver the read only after another owner changes the durable draft.
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        val original = store.list(key).drafts.single()
        assertEquals(PhoneDraftSave.SAVED, store.save(original.copy(text = "concurrent edit"), key) { true })
        drain()
        assertEquals("concurrent edit", store.list(key).drafts.single().text)
        assertTrue(views().filterIsInstance<TextView>().any { it.isShown && it.text.toString().contains("changed, discarded") })
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun destroyedActivityCannotDisplayOrFillALateResult() {
        val store = prepareArchivedEmptyDraft()
        button("Desktop voice history").performClick()
        drain()
        button("Review transcript").performClick()
        executor.runQueued()
        screen.pause().stop().destroy()
        drain()
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        assertEquals("", store.list(key).drafts.single().text)
        assertTrue(views().filterIsInstance<TextView>().none { it.isShown && it.text.toString().contains("synthetic transcript") })
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun expiryBeforeAnArchivedReadCannotDisplayOrFillTheResult() {
        val store = prepareArchivedEmptyDraft()
        button("Desktop voice history").performClick()
        drain()
        button("Review transcript").performClick()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMinutes(6))
        drain()
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        assertEquals("", store.list(key).drafts.single().text)
        assertTrue(views().filterIsInstance<TextView>().none { it.isShown && it.text.toString().contains("synthetic transcript") })
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun lockingBeforeAPageIsDeliveredCannotRestoreProtectedText() {
        val store = prepareArchivedEmptyDraft()
        val original = requireNotNull(VoiceScreenFixture.archive)
        VoiceScreenFixture.archive = original.copy(totalBytes = 8 * 1024 * 1024 + 1, text = null,
            firstPage = PhoneVoicePage(0, 5, "first"))
        VoiceScreenFixture.nextArchivePage = PhoneVoicePage(5, 0, "protected second")
        button("Desktop voice history").performClick()
        drain()
        button("Review transcript").performClick()
        drain()
        button("Next transcript page").performClick()
        executor.runQueued()
        button("Lock workspace").performClick()
        drain()
        assertTrue(views().filterIsInstance<TextView>().none { it.isShown && it.text.toString().contains("protected second") })
        assertEquals("", store.list(SecretKeySpec(VoiceScreenFixture.material, "AES")).drafts.single().text)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun lostReadScopeCannotDisplayAnotherPageOrChangeTheDraft() {
        val store = prepareArchivedEmptyDraft()
        val original = requireNotNull(VoiceScreenFixture.archive)
        VoiceScreenFixture.archive = original.copy(totalBytes = 8 * 1024 * 1024 + 1, text = null,
            firstPage = PhoneVoicePage(0, 5, "first"))
        VoiceScreenFixture.archivePageFailure = PhoneControlOutcome.OUT_OF_SCOPE
        button("Desktop voice history").performClick()
        drain()
        button("Review transcript").performClick()
        drain()
        button("Next transcript page").performClick()
        drain()
        assertTrue(views().filterIsInstance<TextView>().none { it.isShown && it.text.toString() == "first" })
        assertEquals("", store.list(SecretKeySpec(VoiceScreenFixture.material, "AES")).drafts.single().text)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    @Test fun insertReportsTheWholeDraftLimitAndReplaceStillFits() {
        val store = prepareArchivedEmptyDraft()
        val original = requireNotNull(VoiceScreenFixture.archive)
        val transcript = "y".repeat(64 * 1024)
        VoiceScreenFixture.archive = original.copy(totalBytes = transcript.length, text = transcript,
            firstPage = PhoneVoicePage(0, 32 * 1024, transcript.take(32 * 1024)))
        val prior = "x".repeat(8 * 1024 * 1024 - 32 * 1024)
        views().filterIsInstance<android.widget.EditText>().single().setText(prior)
        drain()
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        assertTrue(store.list(key).drafts.single().text == prior)
        button("Desktop voice history").performClick()
        drain()
        button("Review transcript").performClick()
        drain()
        button("Choose destination Session").performClick()
        drain()
        views().filterIsInstance<Button>().single { it.text.toString().contains("named session") }.performClick()
        drain()
        button("Insert transcript").performClick()
        drain()
        assertTrue(views().filterIsInstance<TextView>().any { it.isShown && it.text.toString().contains("entire transcript cannot fit") })
        assertTrue(store.list(key).drafts.single().text == prior)
        button("Replace draft with transcript").performClick()
        drain()
        assertEquals(transcript, store.list(key).drafts.single().text)
        assertEquals(0, VoiceScreenFixture.sends)
    }

    private fun prepareArchivedEmptyDraft(): PhoneDraftStore {
        button("Sessions").performClick()
        drain()
        views().filterIsInstance<Button>().single { it.text.toString().contains("named session") }.performClick()
        drain()
        val input = views().filterIsInstance<android.widget.EditText>().single()
        input.setText("temporary")
        input.setText("")
        drain()
        val store = PhoneDraftStore(File(activity.filesDir, "phone-drafts-v1"))
        val key = SecretKeySpec(VoiceScreenFixture.material, "AES")
        val draft = store.list(key).drafts.single()
        val row = PhoneVoiceRow("11111111-1111-4111-8111-111111111111", draft.source.target,
            draft.source.project, draft.source.agent, true, "completed", "")
        val text = "synthetic transcript"
        VoiceScreenFixture.archive = PhoneVoiceTranscript(row, draft.source.receiver, store.id(draft.source),
            requireNotNull(store.revision(draft, key)), text.toByteArray().size, text, PhoneVoicePage(0, 0, text))
        return store
    }

    private fun beginWithDummyAudio(): String {
        button("Record voice instruction").performClick()
        drain()
        val metadata = File(activity.filesDir, "voice-captures-v1").listFiles().orEmpty()
            .single { it.name.startsWith(".recording-") && it.name.endsWith(".json") }
        File(metadata.parentFile, metadata.name.removeSuffix(".json") + ".m4a").writeBytes(byteArrayOf(1, 2, 3, 4))
        return metadata.name.removePrefix(".recording-").removeSuffix(".json")
    }

    private fun recordings() = PhoneVoiceStore(activity) { SecretKeySpec(VoiceScreenFixture.material, "AES") }.list().records

    private fun drain() { repeat(12) { executor.runQueued(); shadowOf(Looper.getMainLooper()).idle() } }
    private fun views(): Sequence<View> {
        fun traverse(view: View): Sequence<View> = sequence {
            yield(view)
            if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(traverse(view.getChildAt(index)))
        }
        return traverse(activity.window.decorView)
    }
    private fun button(text: String) = views().filterIsInstance<Button>().single { it.text.toString() == text }
}
