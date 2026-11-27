package dev.louiselm.capture

import android.Manifest
import android.app.KeyguardManager
import android.content.DialogInterface
import android.os.Looper
import android.os.PowerManager
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import java.io.File
import java.net.Socket
import java.nio.file.Files
import java.nio.file.Paths
import java.nio.file.StandardWatchEventKinds
import java.time.Duration
import java.util.concurrent.AbstractExecutorService
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.ExecutorService
import java.util.concurrent.TimeUnit
import javax.crypto.spec.SecretKeySpec
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.shadows.ShadowAlertDialog
import org.robolectric.util.ReflectionHelpers

private class VoiceSendBackground : AbstractExecutorService() {
    private val tasks = ConcurrentLinkedQueue<Runnable>()
    private var stopped = false
    override fun execute(command: Runnable) { check(!stopped); tasks.add(command) }
    override fun shutdown() { stopped = true }
    override fun shutdownNow(): MutableList<Runnable> { stopped = true; return mutableListOf() }
    override fun isShutdown() = stopped
    override fun isTerminated() = stopped && tasks.isEmpty()
    override fun awaitTermination(timeout: Long, unit: TimeUnit) = isTerminated
    fun runQueued() {
        while (true) {
            val task = tasks.poll() ?: return
            var failure: Throwable? = null
            val thread = Thread { try { task.run() } catch (error: Throwable) { failure = error } }
            thread.start()
            thread.join()
            failure?.let { throw AssertionError(it) }
        }
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30], shadows = [EndToEndPairingFixture::class, EndToEndKeysFixture::class,
    EndToEndAuthenticatorFixture::class, PhoneScreenOfflineUnlockFixture::class, PhoneScreenOfflineAuthenticatorFixture::class],
    instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneVoiceSendEndToEndTest {
    private fun views(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(views(view.getChildAt(index)))
    }

    @Test fun reviewedVoiceAndSelectedContextReachOnlyTheChosenSession() {
        val sync = System.getenv("LOUISELM_VOICE_SEND_SYNC")
        assumeTrue(sync != null)
        val directory = Paths.get(requireNotNull(sync))
        fun checkpoint(step: String) {
            directory.fileSystem.newWatchService().use { changes ->
                directory.register(changes, StandardWatchEventKinds.ENTRY_CREATE)
                Files.createFile(directory.resolve(step))
                while (!Files.exists(directory.resolve("$step.done"))) {
                    val event = requireNotNull(changes.poll(60, TimeUnit.SECONDS)) { "Missing checkpoint $step" }
                    event.pollEvents()
                    check(event.reset())
                }
            }
        }
        val app = RuntimeEnvironment.getApplication()
        shadowOf(app).grantPermissions(Manifest.permission.RECORD_AUDIO, "${app.packageName}.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION")
        shadowOf(app.getSystemService(PowerManager::class.java)).turnScreenOn(true)
        shadowOf(app.getSystemService(KeyguardManager::class.java)).setIsDeviceLocked(false)
        val config = requireNotNull(PairingStore(app).load())
        val keyHash = JSONObject(requireNotNull(javaClass.getResourceAsStream("/phone-control-v1.json"))
            .bufferedReader().use { it.readText() }).getJSONObject("proof").getString("enrolled_key_hash")
        PhoneControlStore(app).saveGrant(PhoneControlGrant(requireNotNull(System.getenv("LOUISELM_E2E_GRANT")), 1,
            config.receiverIdentitySha256, phoneControlPairingBinding(config), keyHash))
        val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        var pendingKeyCreated = false
        val ordinary = CaptureStore(app)
        val capture = ordinary.begin()
        capture.file.writeBytes("ordinary capture".toByteArray())
        val ordinaryRecording = ordinary.complete(capture, 1_000, null)
        var posts = 0
        var holdHistory = false
        lateinit var subject: String
        lateinit var controller: String
        val transport: PhoneControlTransport = { pairing, endpoint, body, maySend ->
            assertFalse(Looper.getMainLooper().thread == Thread.currentThread())
            val reply = PinnedHttps.phoneControl(pairing, endpoint, body, maySend)
            val request = body.optJSONObject("request")
            if (endpoint == "request" && request?.optInt("action") == 3) {
                posts++
                assertEquals(subject, request.getString("subject_id"))
                assertEquals(controller, request.getString("controller_id"))
                Files.writeString(directory.resolve("operation"), request.getString("operation_id"))
            }
            if (holdHistory && reply is PhoneControlReply.ActionHistory) PhoneControlReply.Failure(PhoneControlOutcome.UNAVAILABLE) else reply
        }
        val store = PhoneDraftStore(File(app.filesDir, "phone-drafts-v1"))
        val screen = Robolectric.buildActivity(PhoneControlActivity::class.java)
        val activity = screen.get()
        ReflectionHelpers.getField<ExecutorService>(activity, "networkExecutor").shutdown()
        val executor = VoiceSendBackground()
        ReflectionHelpers.setField(activity, "networkExecutor", executor)
        fun drain() = repeat(12) { executor.runQueued(); shadowOf(Looper.getMainLooper()).idle() }
        fun controls() = views(activity.window.decorView).filter { it.isShown }
        fun button(id: Int) = controls().filterIsInstance<Button>().singleOrNull { it.text == activity.getString(id) }
            ?: error("Missing ${activity.getString(id)}; buttons=${controls().filterIsInstance<Button>().map { it.text }.toList()}")
        var recordId = ""
        try {
            screen.setup()
            ReflectionHelpers.setField(activity, "client", PhoneControlClient(app, transport = transport))
            ReflectionHelpers.setField(activity, "voiceStore", PhoneVoiceStore(activity) { secret })
            drain()
            button(R.string.phone_unlock).performClick()
            drain()
            checkpoint("unlocked")
            val target = JSONObject(Files.readString(directory.resolve("target.json")))
            subject = target.getString("subject_id")
            controller = target.getString("controller_id")
            assertEquals("the current controller must support Send", true, ReflectionHelpers.getField<PhoneControlPeriod>(activity, "period").sendSupported)
            val row = controls().filterIsInstance<Button>().singleOrNull { it.text.toString().contains(subject) }
                ?: error("Missing target $subject; buttons=${controls().filterIsInstance<Button>().map { it.text }.toList()}")
            row.performClick()
            drain()
            button(R.string.phone_voice_start).performClick()
            drain()
            val voiceDirectory = File(app.filesDir, "voice-captures-v1")
            val metadata = voiceDirectory.listFiles().orEmpty().single { it.name.startsWith(".recording-") && it.name.endsWith(".json") }
            File(voiceDirectory, metadata.name.removeSuffix(".json") + ".m4a").writeBytes("isolated dummy audio".toByteArray())
            checkpoint("recording")
            button(R.string.phone_voice_stop).performClick()
            drain()
            val voice = PhoneVoiceStore(activity) { secret }
            val record = voice.list().records.single()
            recordId = record.id
            assertEquals(PhoneVoiceIntent.ACCEPTED, record.intent)
            assertEquals(subject, record.source.target.subjectId)
            assertEquals(controller, record.source.target.controllerId)
            assertEquals(config.receiverIdentitySha256, record.source.receiver)
            checkpoint("stopped")
            Socket("127.0.0.1", requireNotNull(System.getenv("LOUISELM_VOICE_WORKER_PORT")).toInt()).use { socket ->
                socket.soTimeout = 10_000
                socket.getOutputStream().write((record.id + "\n").toByteArray())
                assertEquals("ready", socket.getInputStream().bufferedReader().readLine())
            }
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
            drain()
            assertEquals("synthetic voice transcript", controls().filterIsInstance<EditText>().single().text.toString())
            checkpoint("transcribed")
            assertEquals(0, posts)
        } finally {
            screen.close()
            executor.runQueued()
            shadowOf(Looper.getMainLooper()).idle()
            executor.shutdownNow()
        }

        val reopened = Robolectric.buildActivity(PhoneControlActivity::class.java)
        val next = reopened.get()
        ReflectionHelpers.getField<ExecutorService>(next, "networkExecutor").shutdown()
        val nextExecutor = VoiceSendBackground()
        ReflectionHelpers.setField(next, "networkExecutor", nextExecutor)
        fun drainNext() = repeat(12) { nextExecutor.runQueued(); shadowOf(Looper.getMainLooper()).idle() }
        fun visible() = views(next.window.decorView).filter { it.isShown }
        fun nextButton(id: Int) = visible().filterIsInstance<Button>().singleOrNull { it.text == next.getString(id) }
            ?: error("Missing ${next.getString(id)}; buttons=${visible().filterIsInstance<Button>().map { it.text }.toList()}")
        try {
            reopened.setup()
            ReflectionHelpers.setField(next, "client", PhoneControlClient(app, transport = transport))
            ReflectionHelpers.setField(next, "voiceStore", PhoneVoiceStore(next) { secret })
            ReflectionHelpers.setField(next, "pendingActionsStore", PendingPhoneActionStore(app, key = { create ->
                if (create) pendingKeyCreated = true
                if (pendingKeyCreated) secret else null
            }))
            drainNext()
            nextButton(R.string.phone_unlock).performClick()
            drainNext()
            checkpoint("reunlock")
            val currentTarget = JSONObject(Files.readString(directory.resolve("target.json")))
            subject = currentTarget.getString("subject_id")
            controller = currentTarget.getString("controller_id")
            nextButton(R.string.phone_draft_list).performClick()
            drainNext()
            val savedRow = visible().filterIsInstance<Button>().singleOrNull { it.text.toString().contains("Phone Send target") }
                ?: error("Missing saved draft; buttons=${visible().filterIsInstance<Button>().map { it.text }.toList()}")
            savedRow.performClick()
            nextButton(R.string.phone_draft_destination).performClick()
            drainNext()
            val destination = visible().filterIsInstance<Button>().singleOrNull { it.text.toString().contains(subject) }
                ?: error("Missing destination $subject; buttons=${visible().filterIsInstance<Button>().map { it.text }.toList()}")
            destination.performClick()
            drainNext()
            val input = visible().filterIsInstance<EditText>().single()
            assertEquals("synthetic voice transcript", input.text.toString())
            checkpoint("reconnected")
            input.setText("Edited voice instruction")
            val additions = visible().filterIsInstance<Button>().filter { it.text == next.getString(R.string.phone_draft_add_context) }.toList()
            assertTrue(additions.isNotEmpty())
            additions.last().performClick()
            drainNext()
            assertEquals(1, store.list(secret).drafts.single().context.size)
            nextButton(R.string.phone_send).performClick()
            val review = ShadowAlertDialog.getLatestAlertDialog()
            val reviewed = shadowOf(review).message.toString()
            assertTrue(reviewed.contains("Edited voice instruction"))
            assertTrue(reviewed.contains("Desktop seed for selected context"))
            Files.writeString(directory.resolve("reviewed"), reviewed)
            checkpoint("review")
            assertEquals(0, posts)
            holdHistory = true
            review.getButton(DialogInterface.BUTTON_POSITIVE).performClick()
            drainNext()
            assertEquals("Send status: ${visible().filterIsInstance<TextView>().map { it.text }.toList()}", 1, posts)
            input.setText("Later phone edit")
            drainNext()
            checkpoint("sent")
            holdHistory = false
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
            drainNext()
            assertTrue(visible().filterIsInstance<TextView>().any { it.text == next.getString(R.string.phone_action_completed) })
            assertEquals("Later phone edit", store.list(secret).drafts.single().text)
            val originalSource = store.list(secret).drafts.single().source
            assertEquals("isolated dummy audio", requireNotNull(PhoneVoiceStore(next) { secret }.list().records.single { it.id == recordId }.audio).audio.readText())
            assertEquals("ordinary capture", ordinaryRecording.audio.readText())
            checkpoint("finished")
            assertEquals(1, posts)

            nextButton(R.string.phone_voice_archive).performClick()
            drainNext()
            checkpoint("other-before")
            val otherTarget = JSONObject(Files.readString(directory.resolve("target.json")))
            subject = otherTarget.getString("other_subject_id")
            controller = otherTarget.getString("other_controller_id")
            nextButton(R.string.phone_voice_archive_review).performClick()
            drainNext()
            assertTrue(visible().filterIsInstance<TextView>().any { it.text.toString().contains("synthetic voice transcript") })
            nextButton(R.string.phone_voice_archive_choose).performClick()
            drainNext()
            val other = visible().filterIsInstance<Button>().singleOrNull { it.text.toString().contains(subject) }
                ?: error("Missing other Session $subject; buttons=${visible().filterIsInstance<Button>().map { it.text }.toList()}")
            other.performClick()
            drainNext()
            nextButton(R.string.phone_voice_archive_insert).performClick()
            drainNext()
            assertEquals("Later phone edit", store.list(secret).drafts.single { it.source == originalSource }.text)
            assertEquals("synthetic voice transcript", store.list(secret).drafts.single { it.source.target.subjectId == subject }.text)
            nextButton(R.string.phone_voice_archive_open).performClick()
            nextButton(R.string.phone_draft_destination).performClick()
            drainNext()
            visible().filterIsInstance<Button>().single { it.text.toString().contains(subject) }.performClick()
            drainNext()
            val otherInput = visible().filterIsInstance<EditText>().single()
            assertEquals("synthetic voice transcript", otherInput.text.toString())
            otherInput.setText("Edited voice for the other Session")
            drainNext()
            nextButton(R.string.phone_send).performClick()
            val otherReview = ShadowAlertDialog.getLatestAlertDialog()
            val otherReviewed = shadowOf(otherReview).message.toString()
            assertEquals("Edited voice for the other Session", otherReviewed)
            Files.writeString(directory.resolve("other-reviewed"), otherReviewed)
            checkpoint("other-review")
            assertEquals(1, posts)
            holdHistory = true
            otherReview.getButton(DialogInterface.BUTTON_POSITIVE).performClick()
            drainNext()
            assertEquals(2, posts)
            checkpoint("other-sent")
            holdHistory = false
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
            drainNext()
            assertEquals("only the unchanged original draft remains", 1, store.list(secret).drafts.size)
            assertEquals("Later phone edit", store.list(secret).drafts.single { it.source == originalSource }.text)
            assertEquals("isolated dummy audio", requireNotNull(PhoneVoiceStore(next) { secret }.list().records.single { it.id == recordId }.audio).audio.readText())
            assertEquals("ordinary capture", ordinaryRecording.audio.readText())
            checkpoint("other-finished")
            assertEquals(2, posts)
        } finally {
            reopened.close()
            nextExecutor.runQueued()
            shadowOf(Looper.getMainLooper()).idle()
            nextExecutor.shutdownNow()
        }
    }
}
