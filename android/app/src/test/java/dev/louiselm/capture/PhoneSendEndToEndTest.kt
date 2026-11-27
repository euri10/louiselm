package dev.louiselm.capture

import android.app.KeyguardManager
import android.content.Intent
import android.os.Looper
import android.os.PowerManager
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import java.nio.file.Files
import java.nio.file.Paths
import java.nio.file.StandardWatchEventKinds
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
import org.robolectric.android.util.concurrent.PausedExecutorService
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.util.ReflectionHelpers

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30], shadows = [EndToEndPairingFixture::class, EndToEndKeysFixture::class,
    EndToEndAuthenticatorFixture::class, PhoneScreenOfflineUnlockFixture::class, PhoneScreenOfflineAuthenticatorFixture::class],
    instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneSendEndToEndTest {
    private fun views(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) for (i in 0 until view.childCount) yieldAll(views(view.getChildAt(i)))
    }

    @Test
    fun phoneSendsReviewedTextAndReadsTheAgentsReply() {
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
        var text = requireNotNull(System.getenv("LOUISELM_E2E_SEND_TEXT"))
        val selectedName = "Phone Send target"
        val otherName = "Second phone Session"
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
        val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        var created = false
        val keyAccess: (Boolean) -> javax.crypto.SecretKey? = { create ->
            if (create) created = true
            if (created) key else null
        }
        var posted = 0
        var lostAcknowledgements = 0
        var loseAcknowledgement = false
        var disabled = false
        var deniedByReceiver = false
        var original: String? = null
        var chosenRow = ""
        var reconnected = false
        var reconciled = false
        val transport: PhoneControlTransport = { pairing, endpoint, body, maySend ->
            val reply = PinnedHttps.phoneControl(pairing, endpoint, body, maySend)
            val request = body.optJSONObject("request")
            if (disabled && reply is PhoneControlReply.Failure && reply.outcome == PhoneControlOutcome.REVOKED) {
                deniedByReceiver = true
                reply
            } else if (endpoint == "request" && request?.optInt("action") == 3) {
                posted++
                original = request.getString("operation_id")
                assertEquals(1, PendingPhoneActionStore(app, keyAccess).pending().count { it.operationId == original })
                for (field in listOf("controller_id", "subject_id")) {
                    assertTrue("signed request must target the displayed row", chosenRow.contains(request.getString(field)))
                }
                val response = if (reply is PhoneControlReply.Failure) reply.outcome.toString() else reply.javaClass.simpleName
                val phase = when {
                    disabled -> "after Disable"
                    loseAcknowledgement -> "after restart"
                    else -> "first"
                }
                assertTrue("$phase Send must return its archived receipt, got $response", reply is PhoneControlReply.ActionHistory)
                val receipt = (reply as PhoneControlReply.ActionHistory).record
                assertEquals(original, receipt.operationId)
                assertEquals(request.getString("subject_id"), receipt.subjectId)
                assertEquals(grant.grantId, receipt.grantId)
                assertTrue(receipt.acceptedEvidence.isNotEmpty())
                if (loseAcknowledgement && lostAcknowledgements == 0) {
                    lostAcknowledgements++
                    PhoneControlReply.Failure(PhoneControlOutcome.UNAVAILABLE)
                } else reply
            } else if (loseAcknowledgement && reply is PhoneControlReply.ActionHistory && reply.record.operationId == original) {
                if (reconnected) {
                    reconciled = reply.record.acceptedEvidence.isNotEmpty()
                    reply
                } else PhoneControlReply.Failure(PhoneControlOutcome.UNAVAILABLE)
            } else reply
        }
        fun open(oldTarget: Boolean = false) {
            val intent = Intent(app, PhoneControlActivity::class.java)
            if (oldTarget) {
                intent.putExtra(PhoneControlActivity.ATTENTION, true)
                intent.putExtra(PhoneControlActivity.CONTROLLER_ID, requireNotNull(System.getenv("LOUISELM_E2E_OLD_CONTROLLER")))
                intent.putExtra(PhoneControlActivity.SUBJECT_ID, requireNotNull(System.getenv("LOUISELM_E2E_OLD_SUBJECT")))
            }
            val screen = Robolectric.buildActivity(PhoneControlActivity::class.java, intent)
            val activity = screen.get()
            ReflectionHelpers.getField<ExecutorService>(activity, "networkExecutor").shutdown()
            val executor = PausedExecutorService()
            ReflectionHelpers.setField(activity, "networkExecutor", executor)
            fun drain() = repeat(12) { executor.runAll(); shadowOf(Looper.getMainLooper()).idle() }
            fun controls() = views(activity.window.decorView).filter { it.isShown }
            fun button(resource: Int) = controls().filterIsInstance<Button>().single { it.text == activity.getString(resource) }
            fun rows(name: String) = controls().filterIsInstance<Button>()
                .filter { it.text.toString().lineSequence().firstOrNull() == name }
            fun lines(resource: Int) = controls().filterIsInstance<TextView>()
                .count { it.text.toString() == activity.getString(resource, text) }
            fun transcript(expected: Int) {
                assertEquals("reviewed prompt in phone transcript", expected, lines(R.string.phone_user_message))
                assertEquals("real echo from the selected Agent", expected, lines(R.string.phone_agent_message))
            }
            try {
                screen.setup()
                ReflectionHelpers.setField(activity, "client", PhoneControlClient(app, transport = transport))
                ReflectionHelpers.setField(activity, "pendingActionsStore", PendingPhoneActionStore(app, keyAccess))
                drain()
                if (disabled && PhoneControlStore(app).grant() == null) {
                    assertTrue(button(R.string.phone_enable).isShown)
                    assertFalse(rows(selectedName).any() || rows(otherName).any())
                    return
                }
                button(R.string.phone_unlock).performClick()
                drain()
                if (oldTarget) {
                    assertTrue("old target after restart must be stale", controls().filterIsInstance<TextView>().any {
                        it.text.toString() == activity.getString(R.string.phone_stale)
                    })
                    assertFalse(controls().filterIsInstance<Button>().any { it.text == activity.getString(R.string.phone_send) && it.isEnabled })
                    return
                }
                if (disabled) {
                    assertTrue("Disable must refuse a fresh unlock", controls().filterIsInstance<TextView>().any {
                        it.text.toString() == activity.getString(R.string.phone_revoked)
                    })
                    assertFalse(rows(selectedName).any() || rows(otherName).any())
                    return
                }
                if (reconnected) {
                    // The new unlock can rotate connection IDs. History uses the original action.
                    assertTrue("reconnected phone must read original evidence", reconciled)
                    rows(selectedName).single().performClick()
                    drain()
                    controls().filterIsInstance<EditText>().single().setText(text)
                    drain()
                    val accepted = setOf(R.string.phone_action_accepted, R.string.phone_action_dispatched, R.string.phone_action_completed)
                        .map { activity.getString(it) }
                    assertTrue("recovered evidence must reach the Send UI", controls().filterIsInstance<TextView>().any {
                        it.text.toString() in accepted
                    })
                    assertFalse("the recovered instruction must not be resent", button(R.string.phone_send).isEnabled)
                    button(R.string.phone_sessions).performClick()
                    drain()
                    val other = rows(otherName).single()
                    chosenRow = other.text.toString()
                    other.performClick()
                    drain()
                    val input = controls().filterIsInstance<EditText>().single()
                    input.setText("This instruction must be refused after Disable")
                    drain()
                    val send = button(R.string.phone_send)
                    assertTrue("second Session was ready before Disable", send.isEnabled)
                    desktopStep("disable")
                    disabled = true
                    assertTrue(send.performClick())
                    drain()
                    return
                }
                assertEquals("second ready Session available", 1, rows(otherName).count())
                val selected = rows(selectedName).single()
                chosenRow = selected.text.toString()
                selected.performClick()
                drain()
                val input = controls().filterIsInstance<EditText>().single()
                input.setText(text)
                drain()
                val send = button(R.string.phone_send)
                assertTrue("reviewed instruction is ready", send.isEnabled)
                assertTrue(send.performClick())
                drain()
                // A refresh observes the receiver and Agent even when the Send reply was lost.
                for (attempt in 1..12) {
                    button(R.string.phone_refresh).performClick()
                    drain()
                    if (lines(R.string.phone_agent_message) > 0) break
                }
                transcript(1)
                button(R.string.phone_sessions).performClick()
                drain()
                rows(otherName).single().performClick()
                drain()
                transcript(0)
            } finally {
                screen.close()
                executor.runAll()
                shadowOf(Looper.getMainLooper()).idle()
                executor.shutdownNow()
            }
        }
        open()
        assertEquals(1, posted)
        val firstOperation = requireNotNull(original)
        desktopStep("restart")
        assertEquals("same mobile enrollment after desktop restart", grant, PhoneControlStore(app).grant())
        open(oldTarget = true)
        loseAcknowledgement = true
        text = requireNotNull(System.getenv("LOUISELM_E2E_NEXT_TEXT"))
        open()
        assertFalse("second Send must have a fresh identity", firstOperation == original)
        assertEquals(1, PendingPhoneActionStore(app, keyAccess).pending().count { it.operationId == original })
        reconnected = true
        open()
        open()
        assertEquals("one delivered POST per reviewed Send", 2, posted)
        assertEquals("successful acknowledgement withheld after delivery", 1, lostAcknowledgements)
        assertTrue("desktop Disable revokes the active phone", deniedByReceiver)
    }
}
