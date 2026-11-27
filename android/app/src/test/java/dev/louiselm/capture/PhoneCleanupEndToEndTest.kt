package dev.louiselm.capture

import android.app.KeyguardManager
import android.content.Intent
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.TextView
import java.nio.file.Files
import java.nio.file.Paths
import java.nio.file.StandardWatchEventKinds
import java.util.concurrent.Callable
import java.util.concurrent.Executors
import java.util.concurrent.ExecutorService
import java.util.concurrent.TimeUnit
import javax.crypto.spec.SecretKeySpec
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.Robolectric
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.android.util.concurrent.PausedExecutorService
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.util.ReflectionHelpers

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30], shadows = [EndToEndPairingFixture::class, EndToEndKeysFixture::class,
    EndToEndAuthenticatorFixture::class, PhoneScreenOfflineUnlockFixture::class, PhoneScreenOfflineAuthenticatorFixture::class],
    instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneCleanupEndToEndTest {
    private fun views(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(views(view.getChildAt(index)))
    }

    @Test
    fun completedSendRemainsNonReplayableAndReadableAfterDesktopRestart() {
        val sync = System.getenv("LOUISELM_E2E_SEND_SYNC")
        assumeTrue(sync != null)
        val directory = Paths.get(requireNotNull(sync))
        fun desktopStep(step: String) {
            directory.fileSystem.newWatchService().use { changes ->
                directory.register(changes, StandardWatchEventKinds.ENTRY_CREATE)
                Files.createFile(directory.resolve(step))
                while (!Files.exists(directory.resolve("$step.done"))) {
                    val event = requireNotNull(changes.poll(60, TimeUnit.SECONDS)) { "Desktop did not finish $step" }
                    event.pollEvents()
                    check(event.reset()) { "Desktop fixture directory disappeared" }
                }
            }
        }
        val app = RuntimeEnvironment.getApplication()
        shadowOf(app).grantPermissions("${app.packageName}.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION")
        shadowOf(app.getSystemService(PowerManager::class.java)).turnScreenOn(true)
        shadowOf(app.getSystemService(KeyguardManager::class.java)).setIsDeviceLocked(false)
        val config = requireNotNull(PairingStore(app).load())
        val keyHash = JSONObject(requireNotNull(javaClass.getResourceAsStream("/phone-control-v1.json"))
            .bufferedReader().use { it.readText() }).getJSONObject("proof").getString("enrolled_key_hash")
        val grant = PhoneControlGrant(requireNotNull(System.getenv("LOUISELM_E2E_GRANT")),
            requireNotNull(System.getenv("LOUISELM_E2E_GRANT_REVISION")).toLong(), config.receiverIdentitySha256,
            phoneControlPairingBinding(config), keyHash)
        PhoneControlStore(app).saveGrant(grant)
        var captured: JSONObject? = null
        val transport: PhoneControlTransport = { pairing, endpoint, body, maySend ->
            if (endpoint == "request" && body.optJSONObject("request")?.optInt("action") == 3) {
                captured = JSONObject(body.toString())
            }
            PinnedHttps.phoneControl(pairing, endpoint, body, maySend)
        }
        val client = PhoneControlClient(app, transport = transport)
        val worker = Executors.newSingleThreadExecutor()
        fun <T> background(work: () -> T): T = worker.submit(Callable { work() }).get(30, TimeUnit.SECONDS)
        fun unlock(): PhoneControlPeriod = background {
            val prepared = (client.prepareUnlock { true } as PhoneControlResult.Value).value
            prepared.signing.update(prepared.proof.transcript())
            val started = SystemClock.elapsedRealtime()
            (client.finishUnlock(prepared, prepared.signing.sign(), started) { true } as PhoneControlResult.Value).value
        }
        val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        var created = false
        val keyAccess: (Boolean) -> javax.crypto.SecretKey? = { create ->
            if (create) created = true
            if (created) key else null
        }
        val pendingStore = PendingPhoneActionStore(app, keyAccess)
        val target = PhoneControlTarget(requireNotNull(System.getenv("LOUISELM_E2E_OLD_CONTROLLER")),
            requireNotNull(System.getenv("LOUISELM_E2E_OLD_SUBJECT")))
        try {
            val firstPeriod = unlock()
            val page = background {
                (client.readSession(firstPeriod, target, SystemClock.elapsedRealtime()) { true } as PhoneControlResult.Value).value
            }
            val attempt = background {
                (client.sendPrompt(firstPeriod, target, page.revision, requireNotNull(System.getenv("LOUISELM_E2E_SEND_TEXT")),
                    pendingStore, SystemClock.elapsedRealtime()) { true } as PhoneControlResult.Value).value
            }
            assertEquals(null, attempt.outcome)
            val original = requireNotNull(captured)
            assertEquals(attempt.pending.operationId, original.getJSONObject("request").getString("operation_id"))
            Files.writeString(directory.resolve("first-operation"), attempt.pending.operationId)
            desktopStep("cleanup")
            val repeated = background { PinnedHttps.phoneControl(config, "request", original) { true } }
            assertTrue(repeated is PhoneControlReply.Failure ||
                repeated is PhoneControlReply.ActionHistory && repeated.record.state.name == "DETAILS_REMOVED")
            val beforeRestart = background {
                (client.readActionHistory(firstPeriod, attempt.pending, SystemClock.elapsedRealtime()) { true } as PhoneControlResult.Value).value
            }
            assertEquals("DETAILS_REMOVED", beforeRestart.state.name)
            firstPeriod.lock()
            desktopStep("restart")
            val secondPeriod = unlock()
            val afterRestart = background {
                (client.readActionHistory(secondPeriod, attempt.pending, SystemClock.elapsedRealtime()) { true } as PhoneControlResult.Value).value
            }
            assertEquals(attempt.pending.operationId, afterRestart.operationId)
            assertEquals("DETAILS_REMOVED", afterRestart.state.name)
            assertEquals(listOf("", "", "", ""), listOf(afterRestart.evidence, afterRestart.acceptedEvidence,
                afterRestart.dispatchedEvidence, afterRestart.completedEvidence))
            assertEquals(attempt.pending, background { pendingStore.pending().single() })
            val lateRepeat = background { PinnedHttps.phoneControl(config, "request", original) { true } }
            assertTrue(lateRepeat is PhoneControlReply.Failure)
            secondPeriod.lock()

            val screen = Robolectric.buildActivity(PhoneControlActivity::class.java, Intent(app, PhoneControlActivity::class.java))
            val activity = screen.get()
            ReflectionHelpers.getField<ExecutorService>(activity, "networkExecutor").shutdown()
            val executor = PausedExecutorService()
            ReflectionHelpers.setField(activity, "networkExecutor", executor)
            fun drain() = repeat(12) { executor.runAll(); shadowOf(Looper.getMainLooper()).idle() }
            fun controls() = views(activity.window.decorView).filter { it.isShown }
            fun button(resource: Int) = controls().filterIsInstance<Button>().single { it.text == activity.getString(resource) }
            try {
                screen.setup()
                ReflectionHelpers.setField(activity, "client", PhoneControlClient(app, transport = transport))
                ReflectionHelpers.setField(activity, "pendingActionsStore", pendingStore)
                drain()
                button(R.string.phone_unlock).performClick(); drain()
                button(R.string.phone_action_history).performClick(); drain()
                assertTrue(controls().filterIsInstance<TextView>().any {
                    it.text.toString().contains(attempt.pending.operationId) &&
                        it.text.toString().contains(activity.getString(R.string.phone_action_removed))
                })
            } finally {
                screen.close()
                executor.runAll()
                shadowOf(Looper.getMainLooper()).idle()
                executor.shutdownNow()
            }
        } finally {
            worker.shutdownNow()
        }
    }
}
