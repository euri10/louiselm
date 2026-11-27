package dev.louiselm.capture

import android.app.KeyguardManager
import android.content.Intent
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import java.io.File
import java.nio.file.Files
import java.nio.file.Paths
import java.nio.file.StandardWatchEventKinds
import java.util.concurrent.ExecutorService
import java.util.concurrent.Executors
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
import org.robolectric.android.util.concurrent.PausedExecutorService
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.util.ReflectionHelpers

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30], shadows = [EndToEndPairingFixture::class, EndToEndKeysFixture::class,
    EndToEndAuthenticatorFixture::class, PhoneScreenOfflineUnlockFixture::class, PhoneScreenOfflineAuthenticatorFixture::class],
    instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneSendUpgradeEndToEndTest {
    private fun views(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(views(view.getChildAt(index)))
    }

    @Test
    fun unavailableThenReviewedSendKeepsPriorUnknowns() {
        val sync = System.getenv("LOUISELM_E2E_SEND_SYNC")
        assumeTrue(sync != null)
        val directory = Paths.get(requireNotNull(sync))
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
        val oldController = requireNotNull(System.getenv("LOUISELM_E2E_OLD_CONTROLLER"))
        val oldSubject = requireNotNull(System.getenv("LOUISELM_E2E_OLD_SUBJECT"))
        val oldOtherSubject = requireNotNull(System.getenv("LOUISELM_E2E_OLD_OTHER_SUBJECT"))
        val text = requireNotNull(System.getenv("LOUISELM_E2E_SEND_TEXT"))
        val oldText = "Earlier ambiguous instruction"
        val oldIds = listOf("aa".repeat(16), "bb".repeat(16))
        val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        var created = false
        val keyAccess: (Boolean) -> javax.crypto.SecretKey? = { create ->
            if (create) created = true
            if (created) key else null
        }
        fun pendingStore() = PendingPhoneActionStore(app, keyAccess)
        val snapshot = File(app.noBackupFilesDir, "phone-action-pending-v1")
        assertFalse(snapshot.exists())
        var enabled = false
        var posted = 0
        var chosenRow = ""
        val unavailableHistory = mutableSetOf<String>()
        val support = mutableListOf<Boolean?>()
        val transport: PhoneControlTransport = { pairing, endpoint, body, maySend ->
            assertFalse("transport must run off the main thread", Looper.getMainLooper().thread == Thread.currentThread())
            val reply = PinnedHttps.phoneControl(pairing, endpoint, body, maySend)
            if (reply is PhoneControlReply.Unlocked) support.add(reply.sendSupported)
            val request = body.optJSONObject("request")
            if (request?.optInt("action") == 3) {
                posted++
                assertTrue("new Send requires the enabled receiver", enabled)
                assertTrue("Send must have owner evidence", reply is PhoneControlReply.ActionHistory)
                val record = (reply as PhoneControlReply.ActionHistory).record
                assertEquals(request.getString("operation_id"), record.operationId)
                assertEquals(grant.grantId, record.grantId)
                assertEquals(request.getString("subject_id"), record.subjectId)
                assertTrue(record.acceptedEvidence.isNotEmpty())
                for (field in listOf("controller_id", "subject_id")) {
                    assertTrue("signed Send must target the selected row", chosenRow.contains(request.getString(field)))
                }
                assertEquals(1, pendingStore().pending().count { it.operationId == record.operationId })
            }
            if (enabled && request?.optInt("action") == 2 && reply is PhoneControlReply.Failure &&
                reply.outcome == PhoneControlOutcome.UNAVAILABLE) {
                oldIds.filterTo(unavailableHistory) { request.optString("payload") == "00$it" }
            }
            reply
        }

        fun screen(check: (PhoneControlActivity, () -> Unit, () -> Sequence<View>) -> Unit) {
            val controller = Robolectric.buildActivity(PhoneControlActivity::class.java, Intent(app, PhoneControlActivity::class.java))
            val activity = controller.get()
            ReflectionHelpers.getField<ExecutorService>(activity, "networkExecutor").shutdown()
            val executor = PausedExecutorService()
            ReflectionHelpers.setField(activity, "networkExecutor", executor)
            val drain = { repeat(12) { executor.runAll(); shadowOf(Looper.getMainLooper()).idle() }; Unit }
            val controls = { views(activity.window.decorView).filter { it.isShown } }
            try {
                controller.setup()
                ReflectionHelpers.setField(activity, "client", PhoneControlClient(app, transport = transport))
                ReflectionHelpers.setField(activity, "pendingActionsStore", pendingStore())
                drain()
                check(activity, drain, controls)
            } finally {
                controller.close()
                executor.runAll()
                shadowOf(Looper.getMainLooper()).idle()
                executor.shutdownNow()
            }
        }

        fun button(activity: PhoneControlActivity, controls: () -> Sequence<View>, resource: Int) =
            controls().filterIsInstance<Button>().single { it.text == activity.getString(resource) }
        fun select(controls: () -> Sequence<View>, name: String) {
            val row = controls().filterIsInstance<Button>().single { it.text.toString().lineSequence().firstOrNull() == name }
            chosenRow = row.text.toString()
            row.performClick()
        }
        fun hasText(activity: PhoneControlActivity, controls: () -> Sequence<View>, resource: Int) =
            controls().filterIsInstance<TextView>().any { it.text.toString() == activity.getString(resource) }

        // The current client refuses before reservation or a Send POST.
        screen { activity, drain, controls ->
            button(activity, controls, R.string.phone_unlock).performClick(); drain()
            assertEquals(false, support.last())
            select(controls, "Phone Send target"); drain()
            val input = controls().filterIsInstance<EditText>().single()
            input.setText(text); drain()
            val send = button(activity, controls, R.string.phone_send)
            assertTrue(send.isEnabled)
            send.performClick(); drain()
            assertEquals(text, input.text.toString())
            assertTrue(hasText(activity, controls, R.string.phone_unsupported))
            assertEquals(0, posted)
            assertFalse("preflight must not create a pending store", snapshot.exists())

            // Reproduce the old reserve-before-transport client as a fixture. These
            // are real signed requests to the read-only receiver, not fabricated
            // history or a claim that its unsupported reply proves no effect.
            button(activity, controls, R.string.phone_sessions).performClick(); drain()
            select(controls, "Second phone Session"); drain()
            controls().filterIsInstance<EditText>().single().setText(oldText); drain()
            val period = ReflectionHelpers.getField<PhoneControlPeriod>(activity, "period")
            val revision = ReflectionHelpers.getField<PhoneControlReply.SessionPage>(activity, "sessionPage").revision
            val worker = Executors.newSingleThreadExecutor()
            try {
                worker.submit {
                    for ((index, operation) in oldIds.withIndex()) {
                        val request = PhoneControlRequest(grant.grantId, grant.revision, period.periodId, grant.receiverIdentity,
                            oldController, oldOtherSubject, phoneControlSendPayload(if (index == 0) oldText else "Another older unknown"),
                            operation, revision, true, 3)
                        assertEquals(PendingReservation.NEW, pendingStore().reserve(request))
                        val signature = requireNotNull(period.sign(request, SystemClock.elapsedRealtime()))
                        val body = JSONObject().put("v", 1).put("type", "request").put("request", request.json(signature))
                        assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.UNSUPPORTED),
                            PinnedHttps.phoneControl(config, "request", body) { true })
                    }
                }.get(30, TimeUnit.SECONDS)
            } finally {
                worker.shutdownNow()
            }
        }
        val original = pendingStore().pending()
        assertEquals(oldIds.toSet(), original.map { it.operationId }.toSet())
        val originalBytes = snapshot.readBytes().toList()
        screen { activity, drain, controls ->
            button(activity, controls, R.string.phone_unlock).performClick(); drain()
            select(controls, "Second phone Session"); drain()
            assertEquals(oldText, controls().filterIsInstance<EditText>().single().text.toString())
            assertFalse("a prior unknown on this Session blocks Send", button(activity, controls, R.string.phone_send).isEnabled)
            button(activity, controls, R.string.phone_sessions).performClick(); drain()
            select(controls, "Phone Send target"); drain()
            assertEquals(text, controls().filterIsInstance<EditText>().single().text.toString())
            assertEquals(originalBytes, snapshot.readBytes().toList())
        }
        directory.fileSystem.newWatchService().use { changes ->
            directory.register(changes, StandardWatchEventKinds.ENTRY_CREATE)
            Files.createFile(directory.resolve("enable-send"))
            while (!Files.exists(directory.resolve("enable-send.done"))) {
                val event = requireNotNull(changes.poll(60, TimeUnit.SECONDS)) { "Desktop did not enable Send" }
                event.pollEvents()
                check(event.reset())
            }
        }
        enabled = true
        assertEquals(grant, PhoneControlStore(app).grant())
        assertEquals(original, pendingStore().pending())
        assertEquals(originalBytes, snapshot.readBytes().toList())
        assertEquals(0, posted)

        // The original Session identity changed; review and copy its retained
        // phone draft to the explicitly selected new Session.
        var savedText = ""
        screen { activity, drain, controls ->
            button(activity, controls, R.string.phone_draft_unlock).performClick(); drain()
            controls().filterIsInstance<Button>().single {
                it.text.toString().contains(oldController) && it.text.toString().contains(oldOtherSubject)
            }.performClick(); drain()
            assertEquals("ambiguous instruction remains editable", oldText, controls().filterIsInstance<EditText>().single().text.toString())
            button(activity, controls, R.string.phone_draft_list).performClick(); drain()
            controls().filterIsInstance<Button>().single {
                it.text.toString().contains(oldController) && it.text.toString().contains(oldSubject)
            }.performClick(); drain()
            savedText = controls().filterIsInstance<EditText>().single().text.toString()
            assertEquals(text, savedText)
        }
        screen { activity, drain, controls ->
            button(activity, controls, R.string.phone_unlock).performClick(); drain()
            assertEquals(true, support.last())
            assertEquals(oldIds.toSet(), unavailableHistory)
            select(controls, "Phone Send target"); drain()
            val input = controls().filterIsInstance<EditText>().single()
            input.setText(oldText); drain()
            assertFalse("missing old history is not a no-effect fence", button(activity, controls, R.string.phone_send).isEnabled)
            assertEquals(0, posted)
            assertEquals(original, pendingStore().pending())
            input.setText(savedText); drain()
            val send = button(activity, controls, R.string.phone_send)
            assertTrue("the distinct reviewed instruction is ready", send.isEnabled)
            send.performClick(); drain()
            for (attempt in 1..12) {
                button(activity, controls, R.string.phone_refresh).performClick(); drain()
                if (controls().filterIsInstance<TextView>().any { it.text.toString() == activity.getString(R.string.phone_agent_message, text) }) break
            }
            assertTrue(controls().filterIsInstance<TextView>().any { it.text.toString() == activity.getString(R.string.phone_user_message, text) })
            assertTrue(controls().filterIsInstance<TextView>().any { it.text.toString() == activity.getString(R.string.phone_agent_message, text) })
            assertEquals(text, input.text.toString())
        }
        assertEquals(1, posted)
        val after = pendingStore().pending()
        assertTrue("new Send must retain both older identities", after.containsAll(original))
        assertEquals(3, after.size)
        screen { activity, drain, controls ->
            button(activity, controls, R.string.phone_draft_unlock).performClick(); drain()
            controls().filterIsInstance<Button>().single {
                it.text.toString().contains(oldController) && it.text.toString().contains(oldOtherSubject)
            }.performClick(); drain()
            assertEquals(oldText, controls().filterIsInstance<EditText>().single().text.toString())
        }
        assertEquals("opening retained work must not resend", 1, posted)
    }
}
