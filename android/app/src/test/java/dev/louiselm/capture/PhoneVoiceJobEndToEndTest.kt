package dev.louiselm.capture

import android.Manifest
import android.app.KeyguardManager
import android.os.Looper
import android.os.PowerManager
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import java.io.File
import java.net.Socket
import java.time.Duration
import java.util.concurrent.AbstractExecutorService
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.ExecutorService
import java.util.concurrent.TimeUnit
import javax.crypto.spec.SecretKeySpec
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.util.ReflectionHelpers

private class VoiceJobBackground : AbstractExecutorService() {
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
            thread.start(); thread.join()
            failure?.let { throw AssertionError(it) }
        }
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30], shadows = [EndToEndPairingFixture::class, EndToEndKeysFixture::class,
    EndToEndAuthenticatorFixture::class, PhoneScreenOfflineUnlockFixture::class, PhoneScreenOfflineAuthenticatorFixture::class],
    instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneVoiceJobEndToEndTest {
    @Test fun recordingReachesThePinnedReceiverAndReturnsAnEditableDraftWithoutSending() {
        assumeTrue(System.getenv("LOUISELM_VOICE_WORKER_PORT") != null)
        val app = RuntimeEnvironment.getApplication()
        shadowOf(app).grantPermissions(Manifest.permission.RECORD_AUDIO, "${app.packageName}.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION")
        shadowOf(app.getSystemService(PowerManager::class.java)).turnScreenOn(true)
        shadowOf(app.getSystemService(KeyguardManager::class.java)).setIsDeviceLocked(false)
        val config = requireNotNull(PairingStore(app).load())
        val keyHash = JSONObject(requireNotNull(javaClass.getResourceAsStream("/phone-control-v1.json"))
            .bufferedReader().use { it.readText() }).getJSONObject("proof").getString("enrolled_key_hash")
        PhoneControlStore(app).saveGrant(PhoneControlGrant(requireNotNull(System.getenv("LOUISELM_E2E_GRANT")), 1,
            config.receiverIdentitySha256, phoneControlPairingBinding(config), keyHash))
        val screen = Robolectric.buildActivity(PhoneControlActivity::class.java)
        val activity = screen.get()
        ReflectionHelpers.getField<ExecutorService>(activity, "networkExecutor").shutdown()
        val executor = VoiceJobBackground()
        ReflectionHelpers.setField(activity, "networkExecutor", executor)
        fun drain() { repeat(12) { executor.runQueued(); shadowOf(Looper.getMainLooper()).idle() } }
        fun views(): Sequence<View> {
            fun children(view: View): Sequence<View> = sequence {
                yield(view)
                if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(children(view.getChildAt(index)))
            }
            return children(activity.window.decorView)
        }
        fun button(label: String) = views().filterIsInstance<Button>().single { it.isShown && it.text.toString() == label }
        try {
            screen.setup()
            val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
            ReflectionHelpers.setField(activity, "voiceStore", PhoneVoiceStore(activity) { secret })
            drain()
            button(activity.getString(R.string.phone_unlock)).performClick()
            drain()
            val subject = requireNotNull(System.getenv("LOUISELM_E2E_SUBJECT"))
            views().filterIsInstance<Button>().single { it.isShown && it.text.toString().contains(subject) }.performClick()
            drain()
            button("Record voice instruction").performClick()
            drain()
            val directory = File(app.filesDir, "voice-captures-v1")
            val metadata = directory.listFiles().orEmpty().single { it.name.startsWith(".recording-") && it.name.endsWith(".json") }
            val audio = File(directory, metadata.name.removeSuffix(".json") + ".m4a")
            audio.writeBytes("isolated dummy audio".toByteArray())
            button("Stop voice recording").performClick()
            drain()
            val voice = PhoneVoiceStore(activity) { secret }
            val record = voice.list().records.single()
            assertEquals(PhoneVoiceIntent.ACCEPTED, record.intent)
            assertEquals(subject, record.source.target.subjectId)
            Socket("127.0.0.1", requireNotNull(System.getenv("LOUISELM_VOICE_WORKER_PORT")).toInt()).use { socket ->
                socket.soTimeout = 10_000
                socket.getOutputStream().write((record.id + "\n").toByteArray())
                assertEquals("ready", socket.getInputStream().bufferedReader().readLine())
            }
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
            drain()
            val input = views().filterIsInstance<EditText>().single()
            assertEquals("synthetic voice transcript", input.text.toString())
            input.setText("edited locally")
            drain()
            val saved = PhoneDraftStore(File(app.filesDir, "phone-drafts-v1")).list(secret).drafts.single()
            assertEquals("edited locally", saved.text)
            assertEquals("isolated dummy audio", requireNotNull(voice.list().records.single().audio).audio.readText())
            assertEquals(false, ReflectionHelpers.getField<PhoneControlPeriod>(activity, "period").sendSupported)
        } finally {
            screen.close()
            executor.runQueued()
            shadowOf(Looper.getMainLooper()).idle()
            executor.shutdownNow()
        }
    }
}
