package dev.louiselm.capture

import android.app.KeyguardManager
import android.content.Intent
import android.os.CancellationSignal
import android.os.Looper
import android.os.PowerManager
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.Button
import android.widget.TextView
import java.io.File
import java.time.Duration
import java.util.concurrent.AbstractExecutorService
import java.util.concurrent.ConcurrentLinkedQueue
import java.util.concurrent.TimeUnit
import javax.crypto.Cipher
import javax.crypto.spec.SecretKeySpec
import org.junit.After
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNotNull
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

private object OfflineScreenFixture {
    val receiver = "aa".repeat(32)
    val target = PhoneControlTarget("11".repeat(16), "22".repeat(16))
    val material = ByteArray(32) { 7 }
    var authenticate: (() -> Unit)? = null
    var pairingUnavailable = false
}

@Implements(PhoneControlClient::class, isInAndroidSdk = false)
class OfflineScreenClientFixture {
    @Implementation(methodName = "status")
    internal fun status(): PhoneControlResult<PhoneControlStatus> = PhoneControlResult.Value(PhoneControlStatus.Disabled)
}

@Implements(PairingStore::class, isInAndroidSdk = false)
class OfflineScreenPairingFixture {
    @Implementation(methodName = "load")
    internal fun load(): PairingConfig {
        if (OfflineScreenFixture.pairingUnavailable) throw SecurityException("fixture")
        return PairingConfig("https://example.invalid", OfflineScreenFixture.receiver, "device", "credential")
    }
}

@Implements(PhoneOfflineUnlockStore::class, isInAndroidSdk = false)
class OfflineScreenUnlockFixture {
    @Implementation(methodName = "prepare")
    internal fun prepare(): PreparedPhoneOfflineUnlock {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, SecretKeySpec(OfflineScreenFixture.material, "AES"))
        return PreparedPhoneOfflineUnlock(cipher, true, ByteArray(32))
    }

    @Implementation(methodName = "finish")
    internal fun finish(prepared: PreparedPhoneOfflineUnlock, cipher: Cipher, startedAtMs: Long): PhoneOfflinePeriod {
        check(prepared.operation === cipher)
        return PhoneOfflinePeriod(OfflineScreenFixture.material, startedAtMs)
    }
}

@Implements(PhoneOfflineAuthenticator::class, isInAndroidSdk = false)
class OfflineScreenAuthenticatorFixture {
    @Implementation(methodName = "start")
    internal fun start(cipher: Cipher, success: (Cipher?) -> Unit, error: (Boolean) -> Unit): CancellationSignal {
        OfflineScreenFixture.authenticate = { success(cipher) }
        return CancellationSignal()
    }
}

private class OfflineBackground : AbstractExecutorService() {
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
            val thread = Thread(task)
            thread.start()
            thread.join()
        }
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30], shadows = [OfflineScreenClientFixture::class, OfflineScreenPairingFixture::class,
    OfflineScreenUnlockFixture::class, OfflineScreenAuthenticatorFixture::class], instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneOfflineScreenTest {
    private lateinit var controller: ActivityController<PhoneControlActivity>
    private lateinit var activity: PhoneControlActivity
    private lateinit var executor: OfflineBackground

    @Before
    fun setup() {
        OfflineScreenFixture.pairingUnavailable = false
        val app = RuntimeEnvironment.getApplication()
        shadowOf(app).grantPermissions("${app.packageName}.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION")
        shadowOf(app.getSystemService(KeyguardManager::class.java)).setIsDeviceLocked(false)
        shadowOf(app.getSystemService(PowerManager::class.java)).turnScreenOn(true)
        val source = PhoneOfflineSource(OfflineScreenFixture.receiver, OfflineScreenFixture.target, "saved session", "agent", "project")
        val page = PhoneControlReply.SessionPage(source.target.controllerId, source.target.subjectId, "33".repeat(16), 1, "ready",
            listOf(PhoneControlEntry.Message(1, "assistant", "protected offline text", false)), 0, true,
            PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false))
        val store = PhoneOfflineStore(File(app.filesDir, "phone-offline-v1"))
        store.save(source, sequenceOf(page), true, System.currentTimeMillis(), System.currentTimeMillis(), 1_000_000,
            SecretKeySpec(OfflineScreenFixture.material, "AES")) { true }
        create()
    }

    private fun create() {
        controller = Robolectric.buildActivity(PhoneControlActivity::class.java)
        activity = controller.get()
        ReflectionHelpers.getField<java.util.concurrent.ExecutorService>(activity, "networkExecutor").shutdown()
        executor = OfflineBackground()
        ReflectionHelpers.setField(activity, "networkExecutor", executor)
        controller.setup()
        drain()
    }

    @After
    fun close() {
        if (!activity.isDestroyed) controller.close()
        executor.runQueued()
        shadowOf(Looper.getMainLooper()).idle()
    }

    private fun drain() { repeat(5) { executor.runQueued(); shadowOf(Looper.getMainLooper()).idle() } }
    private fun views(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(views(view.getChildAt(index)))
    }
    private fun button(id: Int) = views(activity.window.decorView).filterIsInstance<Button>()
        .single { it.text.toString() == activity.getString(id) }
    private fun contains(text: String) = views(activity.window.decorView).filterIsInstance<TextView>().any { it.text.toString().contains(text) }

    private fun unlockAndOpen() {
        button(R.string.phone_offline_unlock).performClick()
        drain()
        requireNotNull(OfflineScreenFixture.authenticate).invoke()
        drain()
        views(activity.window.decorView).filterIsInstance<Button>().single { it.text.toString().contains("saved session") }.performClick()
        drain()
    }

    @Test
    fun opensAndLocksAStaleCopyWithoutNetworkAndProtectsThePreview() {
        assertTrue(activity.window.attributes.flags and WindowManager.LayoutParams.FLAG_SECURE != 0)
        button(R.string.phone_offline_unlock).performClick()
        drain()
        assertNotNull(views(activity.window.decorView).filterIsInstance<TextView>().joinToString { it.text.toString() },
            OfflineScreenFixture.authenticate)
        requireNotNull(OfflineScreenFixture.authenticate).invoke()
        drain()
        views(activity.window.decorView).filterIsInstance<Button>().single { it.text.toString().contains("saved session") }.performClick()
        drain()
        assertTrue(contains("protected offline text"))
        assertTrue(contains(activity.getString(R.string.phone_offline_stale)))
        assertTrue(views(activity.window.decorView).filterIsInstance<TextView>()
            .single { it.text.toString().contains("protected offline text") }.isTextSelectable)
        assertTrue(contains(activity.getString(R.string.phone_offline_copy_notice)))
        controller.pause()
        assertFalse(views(activity.window.decorView).filterIsInstance<TextView>()
            .any { it.isShown && it.text.toString().contains("protected offline text") })
        controller.resume()
        drain()
        assertTrue(contains("protected offline text"))
        activity.sendBroadcast(Intent(Intent.ACTION_SCREEN_OFF).setPackage(activity.packageName))
        drain()
        assertFalse(contains("protected offline text"))
    }

    @Test
    fun processRecreationStartsLockedAndLateAuthenticationCannotRestoreAView() {
        button(R.string.phone_offline_unlock).performClick()
        drain()
        val late = requireNotNull(OfflineScreenFixture.authenticate)
        controller.close()
        late()
        drain()
        create()
        assertFalse(contains("protected offline text"))
        assertTrue(button(R.string.phone_offline_unlock).isShown)
    }

    @Test
    fun backgroundDoesNotExtendTheLocalDeadline() {
        unlockAndOpen()
        controller.pause()
        shadowOf(Looper.getMainLooper()).idleFor(Duration.ofMinutes(5))
        controller.resume()
        drain()
        assertFalse(contains("protected offline text"))
        assertTrue(button(R.string.phone_offline_unlock).isShown)
    }

    @Test
    fun offlineReadingDoesNotRequireTheCapturePairingKey() {
        OfflineScreenFixture.pairingUnavailable = true
        unlockAndOpen()
        assertTrue(contains("protected offline text"))
    }
}
