package dev.louiselm.capture

import android.app.KeyguardManager
import android.os.Looper
import android.os.PowerManager
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import java.io.File
import java.net.Socket
import java.util.concurrent.AbstractExecutorService
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.ExecutorService
import java.util.concurrent.TimeUnit
import javax.crypto.spec.SecretKeySpec
import org.json.JSONObject
import org.junit.Assert.assertEquals
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
import org.robolectric.util.ReflectionHelpers

private class ArchiveBackground : AbstractExecutorService() {
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
class PhoneVoiceArchiveEndToEndTest {
    @Test fun pinnedReceiverPreservesOriginalAndExplicitDestinationDrafts() {
        assumeTrue(System.getenv("LOUISELM_E2E_SEED_PORT") != null)
        val app = RuntimeEnvironment.getApplication()
        shadowOf(app).grantPermissions("${app.packageName}.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION")
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
        val executor = ArchiveBackground()
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
            drain()
            button(activity.getString(R.string.phone_unlock)).performClick()
            drain()
            val subject = requireNotNull(System.getenv("LOUISELM_E2E_SUBJECT"))
            views().filterIsInstance<Button>().single { it.isShown && it.text.toString().contains(subject) }.performClick()
            drain()
            val input = views().filterIsInstance<EditText>().single()
            input.setText("temporary")
            input.setText("")
            drain()
            val store = PhoneDraftStore(File(app.filesDir, "phone-drafts-v1"))
            val localKey = SecretKeySpec(ByteArray(32) { 7 }, "AES")
            val original = store.list(localKey).drafts.single()
            val seed = JSONObject().put("draft", store.id(original.source)).put("revision", requireNotNull(store.revision(original, localKey)))
            Socket("127.0.0.1", requireNotNull(System.getenv("LOUISELM_E2E_SEED_PORT")).toInt()).use { socket ->
                socket.soTimeout = 10_000
                socket.getOutputStream().write((seed.toString() + "\n").toByteArray())
                assertEquals("ready", socket.getInputStream().bufferedReader().readLine())
            }
            button("Desktop voice history").performClick()
            drain()
            views().filterIsInstance<Button>().filter { it.isShown && it.text.toString() == "Review transcript" }.first().performClick()
            drain()
            assertEquals("synthetic archived transcript", store.list(localKey).drafts.single().text)
            button("Open reviewed draft").performClick()
            drain()
            views().filterIsInstance<EditText>().single().setText("locally edited")
            drain()
            button("Desktop voice history").performClick()
            drain()
            views().filterIsInstance<Button>().filter { it.isShown && it.text.toString() == "Review transcript" }.last().performClick()
            drain()
            assertEquals("locally edited", store.list(localKey).drafts.single().text)
            button("Choose destination Session").performClick()
            drain()
            val other = requireNotNull(System.getenv("LOUISELM_E2E_OTHER_SUBJECT"))
            views().filterIsInstance<Button>().single { it.isShown && it.text.toString().contains(other) }.performClick()
            drain()
            button("Insert transcript").performClick()
            drain()
            val drafts = store.list(localKey).drafts.associateBy { it.source.target.subjectId }
            assertEquals("locally edited", drafts[subject]?.text)
            assertEquals("synthetic archived transcript", drafts[other]?.text)
            assertTrue(views().filterIsInstance<TextView>().any { it.isShown && it.text.toString().contains("device-local draft") })
        } finally {
            screen.close()
            executor.runQueued()
            shadowOf(Looper.getMainLooper()).idle()
            executor.shutdownNow()
        }
    }
}
