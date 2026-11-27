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
import java.util.concurrent.ExecutorService
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
        val phase = System.getenv("LOUISELM_E2E_SEND_PHASE")
        assumeTrue(phase != null)
        val text = requireNotNull(System.getenv("LOUISELM_E2E_SEND_TEXT"))
        val subject = requireNotNull(System.getenv("LOUISELM_E2E_SUBJECT"))
        val other = requireNotNull(System.getenv("LOUISELM_E2E_OTHER_SUBJECT"))
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
        var original: String? = null
        var reconnected = false
        var reconciled = false
        val transport: PhoneControlTransport = { pairing, endpoint, body, maySend ->
            val reply = PinnedHttps.phoneControl(pairing, endpoint, body, maySend)
            val request = body.optJSONObject("request")
            if (endpoint == "request" && request?.optInt("action") == 3) {
                posted++
                original = request.getString("operation_id")
                assertEquals(original, PendingPhoneActionStore(app, keyAccess).pending().single().operationId)
                assertTrue("Send must return its archived receipt", reply is PhoneControlReply.ActionHistory)
                val receipt = (reply as PhoneControlReply.ActionHistory).record
                assertEquals(original, receipt.operationId)
                assertEquals(subject, receipt.subjectId)
                assertEquals(grant.grantId, receipt.grantId)
                assertTrue(receipt.acceptedEvidence.isNotEmpty())
                if (phase == "lost_ack" && posted == 1) {
                    lostAcknowledgements++
                    PhoneControlReply.Failure(PhoneControlOutcome.UNAVAILABLE)
                } else reply
            } else if (phase == "lost_ack" && reply is PhoneControlReply.ActionHistory && reply.record.operationId == original) {
                if (reconnected) {
                    reconciled = reply.record.acceptedEvidence.isNotEmpty()
                    reply
                } else PhoneControlReply.Failure(PhoneControlOutcome.UNAVAILABLE)
            } else reply
        }
        fun open(sendOnce: Boolean, oldTarget: Boolean = false) {
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
            fun rows(subjectId: String) = controls().filterIsInstance<Button>().filter { it.text.toString().contains(subjectId) }
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
                button(R.string.phone_unlock).performClick()
                drain()
                if (oldTarget) {
                    assertTrue("old target after restart must be stale", controls().filterIsInstance<TextView>().any {
                        it.text.toString() == activity.getString(R.string.phone_stale)
                    })
                    assertFalse(controls().filterIsInstance<Button>().any { it.text == activity.getString(R.string.phone_send) && it.isEnabled })
                    return
                }
                if (phase == "disabled") {
                    assertTrue("Disable must refuse a fresh unlock", controls().filterIsInstance<TextView>().any {
                        it.text.toString() == activity.getString(R.string.phone_revoked)
                    })
                    assertFalse(rows(subject).any() || rows(other).any())
                    return
                }
                assertEquals("second ready Session available", 1, rows(other).count())
                rows(subject).single().performClick()
                drain()
                if (sendOnce) {
                    val input = controls().filterIsInstance<EditText>().single()
                    input.setText(text)
                    drain()
                    val send = button(R.string.phone_send)
                    assertTrue("reviewed instruction is ready", send.isEnabled)
                    assertTrue(send.performClick())
                    drain()
                }
                // A refresh observes the receiver and Agent even when the Send reply was lost.
                for (attempt in 1..12) {
                    button(R.string.phone_refresh).performClick()
                    drain()
                    if (lines(R.string.phone_agent_message) > 0) break
                }
                transcript(1)
                button(R.string.phone_sessions).performClick()
                drain()
                rows(other).single().performClick()
                drain()
                transcript(0)
            } finally {
                screen.close()
                executor.runAll()
                shadowOf(Looper.getMainLooper()).idle()
                executor.shutdownNow()
            }
        }
        if (phase == "lost_ack") open(sendOnce = false, oldTarget = true)
        open(sendOnce = phase != "disabled")
        if (phase == "lost_ack") {
            assertEquals(original, PendingPhoneActionStore(app, keyAccess).pending().single().operationId)
            reconnected = true
            open(sendOnce = false)
            assertTrue("reconnected phone must read the original operation's evidence", reconciled)
        }
        assertEquals("Android must issue exactly one POST per Send", if (phase == "disabled") 0 else 1, posted)
        assertEquals("successful acknowledgement withheld after delivery", if (phase == "lost_ack") 1 else 0, lostAcknowledgements)
    }
}
