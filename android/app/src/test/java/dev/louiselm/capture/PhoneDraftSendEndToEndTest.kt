package dev.louiselm.capture

import android.app.KeyguardManager
import android.content.DialogInterface
import android.content.Intent
import android.os.Looper
import android.os.PowerManager
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import java.io.File
import java.nio.file.Files
import java.nio.file.Paths
import java.nio.file.StandardWatchEventKinds
import java.time.Duration
import java.util.concurrent.ExecutorService
import java.util.concurrent.TimeUnit
import javax.crypto.SecretKey
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
import org.robolectric.android.util.concurrent.PausedExecutorService
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.shadows.ShadowAlertDialog
import org.robolectric.util.ReflectionHelpers

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30], shadows = [EndToEndPairingFixture::class, EndToEndKeysFixture::class,
    EndToEndAuthenticatorFixture::class, PhoneScreenOfflineUnlockFixture::class, PhoneScreenOfflineAuthenticatorFixture::class],
    instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneDraftSendEndToEndTest {
    private fun views(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(views(view.getChildAt(index)))
    }

    @Test
    fun reviewedContextAndEditedDraftSurviveRealSendOrderings() {
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
                    check(event.reset())
                }
            }
        }
        val app = RuntimeEnvironment.getApplication()
        shadowOf(app).grantPermissions("${app.packageName}.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION")
        val power = shadowOf(app.getSystemService(PowerManager::class.java))
        power.turnScreenOn(true)
        shadowOf(app.getSystemService(KeyguardManager::class.java)).setIsDeviceLocked(false)
        val config = requireNotNull(PairingStore(app).load())
        val keyHash = JSONObject(requireNotNull(javaClass.getResourceAsStream("/phone-control-v1.json"))
            .bufferedReader().use { it.readText() }).getJSONObject("proof").getString("enrolled_key_hash")
        val grant = PhoneControlGrant(requireNotNull(System.getenv("LOUISELM_E2E_GRANT")),
            requireNotNull(System.getenv("LOUISELM_E2E_GRANT_REVISION")).toLong(), config.receiverIdentitySha256,
            phoneControlPairingBinding(config), keyHash)
        PhoneControlStore(app).saveGrant(grant)
        val captures = CaptureStore(app)
        val pendingCapture = captures.begin()
        val audio = byteArrayOf(1, 2, 3, 4)
        pendingCapture.file.writeBytes(audio)
        val recording = captures.complete(pendingCapture, 1_000, null)
        val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        var created = false
        val keyAccess: (Boolean) -> SecretKey? = { create ->
            if (create) created = true
            if (created) secret else null
        }
        var posts = 0
        var holdHistory = false
        var chosenRow = ""
        val observedStates = mutableListOf<String>()
        val payloads = mutableListOf<String>()
        val transport: PhoneControlTransport = { pairing, endpoint, body, maySend ->
            assertFalse(Looper.getMainLooper().thread == Thread.currentThread())
            val reply = PinnedHttps.phoneControl(pairing, endpoint, body, maySend)
            if (reply is PhoneControlReply.ActionHistory) observedStates.add(reply.record.state.toString())
            if (reply is PhoneControlReply.Failure) observedStates.add(reply.outcome.toString())
            val request = body.optJSONObject("request")
            if (endpoint == "request" && request?.optInt("action") == 3) {
                posts++
                val payload = phoneControlHex(request.getString("payload"))
                assertEquals(1, payload.first().toInt())
                payloads.add(payload.copyOfRange(1, payload.size).toString(Charsets.UTF_8))
                for (field in listOf("controller_id", "subject_id")) assertTrue(chosenRow.contains(request.getString(field)))
                assertEquals(1, PendingPhoneActionStore(app, keyAccess).pending().count { it.operationId == request.getString("operation_id") })
                if (posts == 1) Files.writeString(directory.resolve("first-operation"), request.getString("operation_id"))
                if (posts == 2) Files.writeString(directory.resolve("second-operation"), request.getString("operation_id"))
                if (posts == 2) PhoneControlReply.Failure(PhoneControlOutcome.UNAVAILABLE) else reply
            } else if (holdHistory && reply is PhoneControlReply.ActionHistory) {
                PhoneControlReply.Failure(PhoneControlOutcome.UNAVAILABLE)
            } else reply
        }
        val store = PhoneDraftStore(File(app.filesDir, "phone-drafts-v1"))
        val screen = Robolectric.buildActivity(PhoneControlActivity::class.java)
        val activity = screen.get()
        ReflectionHelpers.getField<ExecutorService>(activity, "networkExecutor").shutdown()
        val executor = PausedExecutorService()
        ReflectionHelpers.setField(activity, "networkExecutor", executor)
        fun drain() = repeat(12) { executor.runAll(); shadowOf(Looper.getMainLooper()).idle() }
        fun controls() = views(activity.window.decorView).filter { it.isShown }
        fun button(id: Int) = controls().filterIsInstance<Button>().single { it.text == activity.getString(id) }
        fun input() = controls().filterIsInstance<EditText>().single()
        fun openSession() {
            val row = controls().filterIsInstance<Button>().single { it.text.toString().lineSequence().firstOrNull() == "Phone Send target" }
            chosenRow = row.text.toString()
            row.performClick()
            drain()
        }
        try {
            screen.setup()
            ReflectionHelpers.setField(activity, "client", PhoneControlClient(app, transport = transport))
            ReflectionHelpers.setField(activity, "pendingActionsStore", PendingPhoneActionStore(app, keyAccess))
            drain()
            button(R.string.phone_unlock).performClick()
            drain()
            openSession()
            input().setText("Reviewed phone instruction")
            val additions = controls().filterIsInstance<Button>().filter { it.text == activity.getString(R.string.phone_draft_add_context) }.toList()
            assertTrue("desktop seed must be visible", additions.isNotEmpty())
            additions.last().performClick()
            drain()
            assertEquals(1, store.list(secret).drafts.single().context.size)
            val saved = store.list(secret).drafts.single()
            assertEquals(PhoneSessionSettings(), saved.context.single().page.settings)
            assertEquals(null, saved.context.single().page.queue)
            assertTrue(button(R.string.phone_send).isEnabled)
            button(R.string.phone_send).performClick()
            val firstReview = ShadowAlertDialog.getLatestAlertDialog()
            val reviewed = shadowOf(firstReview).message.toString()
            assertTrue(reviewed.contains("Desktop seed for selected context"))
            desktopStep("desktop-before")
            firstReview.getButton(DialogInterface.BUTTON_NEGATIVE).performClick()
            button(R.string.phone_refresh).performClick()
            drain()
            assertEquals(0, posts)
            assertTrue("after refresh: states=$observedStates visible=${controls().filterIsInstance<TextView>().map { it.text }.toList()}",
                button(R.string.phone_send).isEnabled)
            button(R.string.phone_send).performClick()
            val secondReview = ShadowAlertDialog.getLatestAlertDialog()
            assertEquals(reviewed, shadowOf(secondReview).message.toString())
            Files.writeString(directory.resolve("first-payload"), reviewed)
            holdHistory = true
            secondReview.getButton(DialogInterface.BUTTON_POSITIVE).performClick()
            drain()
            assertEquals(1, posts)
            assertEquals(reviewed, payloads.single())
            desktopStep("first-complete")
            holdHistory = false
            shadowOf(Looper.getMainLooper()).idleFor(Duration.ofSeconds(10))
            drain()
            assertEquals("", input().text.toString())
            assertTrue(store.list(secret).drafts.isEmpty())
            desktopStep("desktop-after")
            button(R.string.phone_refresh).performClick()
            drain()
            val secondText = "Second phone instruction"
            input().setText(secondText)
            drain()
            Files.writeString(directory.resolve("second-payload"), secondText)
            holdHistory = true
            button(R.string.phone_send).performClick()
            drain()
            assertEquals(2, posts)
            assertEquals(secondText, payloads[1])
            input().setText("Newer edit after the uncertain Send")
            drain()
            desktopStep("second-complete")
            activity.sendBroadcast(Intent(Intent.ACTION_SCREEN_OFF).setPackage(activity.packageName))
            drain()
            assertEquals("Newer edit after the uncertain Send", store.list(secret).drafts.single().text)
        } finally {
            screen.close()
            executor.runAll()
            shadowOf(Looper.getMainLooper()).idle()
            executor.shutdownNow()
        }

        power.turnScreenOn(true)
        holdHistory = false
        val reopened = Robolectric.buildActivity(PhoneControlActivity::class.java)
        val activity2 = reopened.get()
        ReflectionHelpers.getField<ExecutorService>(activity2, "networkExecutor").shutdown()
        val executor2 = PausedExecutorService()
        ReflectionHelpers.setField(activity2, "networkExecutor", executor2)
        fun drain2() = repeat(12) { executor2.runAll(); shadowOf(Looper.getMainLooper()).idle() }
        fun controls2() = views(activity2.window.decorView).filter { it.isShown }
        try {
            reopened.setup()
            ReflectionHelpers.setField(activity2, "client", PhoneControlClient(app, transport = transport))
            ReflectionHelpers.setField(activity2, "pendingActionsStore", PendingPhoneActionStore(app, keyAccess))
            drain2()
            assertEquals("same enrollment after the receiver restart", grant, PhoneControlStore(app).grant())
            assertEquals("reopening must not send", 2, posts)
            controls2().filterIsInstance<Button>().single { it.text == activity2.getString(R.string.phone_unlock) }.performClick()
            drain2()
            controls2().filterIsInstance<Button>().single { it.text == activity2.getString(R.string.phone_draft_list) }.performClick()
            controls2().filterIsInstance<Button>().single { it.text.toString().contains("Phone Send target") }.performClick()
            controls2().filterIsInstance<Button>().single { it.text == activity2.getString(R.string.phone_draft_destination) }.performClick()
            drain2()
            val row = controls2().filterIsInstance<Button>().single { it.text.toString().lineSequence().firstOrNull() == "Phone Send target" }
            chosenRow = row.text.toString()
            row.performClick()
            drain2()
            assertEquals("Newer edit after the uncertain Send", controls2().filterIsInstance<EditText>().single().text.toString())
            assertEquals("Newer edit after the uncertain Send", store.list(secret).drafts.single().text)
            assertTrue(controls2().filterIsInstance<TextView>().any { it.text == activity2.getString(R.string.phone_action_completed) })
            assertEquals(2, posts)
            assertTrue(controls2().filterIsInstance<Button>().single { it.text == activity2.getString(R.string.phone_send) }.isEnabled)
            controls2().filterIsInstance<Button>().single { it.text == activity2.getString(R.string.phone_send) }.performClick()
            val conflictReview = ShadowAlertDialog.getLatestAlertDialog()
            desktopStep("desktop-conflict")
            holdHistory = true
            conflictReview.getButton(DialogInterface.BUTTON_POSITIVE).performClick()
            drain2()
            assertEquals(3, posts)
            assertEquals("Newer edit after the uncertain Send", controls2().filterIsInstance<EditText>().single().text.toString())
            assertFalse(controls2().filterIsInstance<Button>().single { it.text == activity2.getString(R.string.phone_send) }.isEnabled)
            assertTrue(controls2().filterIsInstance<TextView>().any { it.text == activity2.getString(R.string.phone_send_unknown) })
            activity2.sendBroadcast(Intent(Intent.ACTION_SCREEN_OFF).setPackage(activity2.packageName))
            drain2()
        } finally {
            reopened.close()
            executor2.runAll()
            shadowOf(Looper.getMainLooper()).idle()
            executor2.shutdownNow()
        }

        power.turnScreenOn(true)
        val finalScreen = Robolectric.buildActivity(PhoneControlActivity::class.java)
        val activity3 = finalScreen.get()
        ReflectionHelpers.getField<ExecutorService>(activity3, "networkExecutor").shutdown()
        val executor3 = PausedExecutorService()
        ReflectionHelpers.setField(activity3, "networkExecutor", executor3)
        fun drain3() = repeat(12) { executor3.runAll(); shadowOf(Looper.getMainLooper()).idle() }
        fun controls3() = views(activity3.window.decorView).filter { it.isShown }
        try {
            finalScreen.setup()
            ReflectionHelpers.setField(activity3, "client", PhoneControlClient(app, transport = transport))
            ReflectionHelpers.setField(activity3, "pendingActionsStore", PendingPhoneActionStore(app, keyAccess))
            drain3()
            controls3().filterIsInstance<Button>().single { it.text == activity3.getString(R.string.phone_unlock) }.performClick()
            drain3()
            controls3().filterIsInstance<Button>().single { it.text == activity3.getString(R.string.phone_draft_list) }.performClick()
            controls3().filterIsInstance<Button>().single { it.text.toString().contains("Phone Send target") }.performClick()
            drain3()
            assertEquals("Newer edit after the uncertain Send", controls3().filterIsInstance<EditText>().single().text.toString())
            assertFalse(controls3().filterIsInstance<Button>().single { it.text == activity3.getString(R.string.phone_send) }.isEnabled)
            assertEquals(3, posts)
            controls3().filterIsInstance<Button>().single { it.text == activity3.getString(R.string.phone_draft_discard) }.performClick()
            ShadowAlertDialog.getLatestAlertDialog().getButton(DialogInterface.BUTTON_POSITIVE).performClick()
            drain3()
            assertTrue(store.list(secret).drafts.isEmpty())
            assertEquals(3, PendingPhoneActionStore(app, keyAccess).pending().size)
            assertTrue(recording.audio.readBytes().contentEquals(audio))
        } finally {
            finalScreen.close()
            executor3.runAll()
            shadowOf(Looper.getMainLooper()).idle()
            executor3.shutdownNow()
        }
    }
}
