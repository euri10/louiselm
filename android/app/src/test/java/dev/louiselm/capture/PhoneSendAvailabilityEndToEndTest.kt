package dev.louiselm.capture

import android.Manifest
import android.app.KeyguardManager
import android.content.Intent
import android.os.Build
import android.os.Looper
import android.os.PowerManager
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import java.io.File
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
class PhoneSendAvailabilityEndToEndTest {
    private fun views(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(views(view.getChildAt(index)))
    }

    @Test
    fun unavailableSendPreservesDraftAndNeverCreatesOrClearsAnIdentity() {
        assumeTrue(System.getenv("LOUISELM_E2E_URL") != null)
        val app = RuntimeEnvironment.getApplication()
        shadowOf(app).grantPermissions("${app.packageName}.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION")
        shadowOf(app.getSystemService(PowerManager::class.java)).turnScreenOn(true)
        shadowOf(app.getSystemService(KeyguardManager::class.java)).setIsDeviceLocked(false)
        val config = requireNotNull(PairingStore(app).load())
        val keyHash = JSONObject(requireNotNull(javaClass.getResourceAsStream("/phone-control-v1.json"))
            .bufferedReader().use { it.readText() }).getJSONObject("proof").getString("enrolled_key_hash")
        val grant = PhoneControlGrant(requireNotNull(System.getenv("LOUISELM_E2E_GRANT")), 1,
            config.receiverIdentitySha256, phoneControlPairingBinding(config), keyHash)
        PhoneControlStore(app).saveGrant(grant)
        val controller = requireNotNull(System.getenv("LOUISELM_E2E_CONTROLLER"))
        val subject = requireNotNull(System.getenv("LOUISELM_E2E_SUBJECT"))
        val target = PhoneControlTarget(controller, subject)
        val snapshot = File(app.noBackupFilesDir, "phone-action-pending-v1")
        assertTrue(!snapshot.exists() || snapshot.delete())
        val secret = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        var keyCreated = false
        val keyAccess: (Boolean) -> javax.crypto.SecretKey? = { create ->
            if (create) keyCreated = true
            if (keyCreated) secret else null
        }
        val store = PendingPhoneActionStore(app, keyAccess)
        val intent = Intent(app, PhoneControlActivity::class.java).putExtra(PhoneControlActivity.ATTENTION, true)
            .putExtra(PhoneControlActivity.CONTROLLER_ID, controller).putExtra(PhoneControlActivity.SUBJECT_ID, subject)

        fun runScreen(check: (PhoneControlActivity, PausedExecutorService) -> Unit) {
            val screen = Robolectric.buildActivity(PhoneControlActivity::class.java, intent)
            val activity = screen.get()
            ReflectionHelpers.getField<ExecutorService>(activity, "networkExecutor").shutdown()
            val executor = PausedExecutorService()
            ReflectionHelpers.setField(activity, "networkExecutor", executor)
            fun drain() { executor.runAll(); shadowOf(Looper.getMainLooper()).idle() }
            try {
                screen.setup()
                ReflectionHelpers.setField(activity, "pendingActionsStore", store)
                drain()
                views(activity.window.decorView).filterIsInstance<Button>()
                    .single { it.text.toString() == activity.getString(R.string.phone_unlock) }.performClick()
                repeat(10) { drain() }
                assertTrue(views(activity.window.decorView).filterIsInstance<TextView>()
                    .any { it.text.toString().contains("fixture request") })
                check(activity, executor)
            } finally {
                screen.close()
                executor.runAll()
                shadowOf(Looper.getMainLooper()).idle()
                executor.shutdownNow()
            }
        }

        fun trySend(activity: PhoneControlActivity, executor: PausedExecutorService, denyNetwork: Boolean = false) {
            val draft = "Full reviewed instruction\nwith another line and é."
            val input = views(activity.window.decorView).filterIsInstance<EditText>().single()
            input.setText(draft)
            repeat(3) { executor.runAll(); shadowOf(Looper.getMainLooper()).idle() }
            val button = views(activity.window.decorView).filterIsInstance<Button>()
                .single { it.text.toString() == activity.getString(R.string.phone_send) }
            assertTrue(button.isEnabled)
            val sdk = Build.VERSION.SDK_INT
            try {
                if (denyNetwork) {
                    shadowOf(app).denyPermissions(Manifest.permission.ACCESS_LOCAL_NETWORK)
                    ReflectionHelpers.setStaticField(Build.VERSION::class.java, "SDK_INT", 37)
                }
                button.performClick()
                repeat(3) { executor.runAll(); shadowOf(Looper.getMainLooper()).idle() }
                assertEquals(draft, input.text.toString())
                assertTrue(views(activity.window.decorView).filterIsInstance<TextView>()
                    .any { it.text.toString() == activity.getString(R.string.phone_unsupported) })
                assertEquals(null, ReflectionHelpers.getField<Any?>(activity, "pendingNetworkAction"))
            } finally {
                if (denyNetwork) ReflectionHelpers.setStaticField(Build.VERSION::class.java, "SDK_INT", sdk)
            }
        }

        runScreen { activity, executor ->
            trySend(activity, executor, denyNetwork = true)
            assertFalse(snapshot.exists())
        }
        // The restarted Activity reads the real receiver again; the refused Send left no identity.
        runScreen { activity, executor ->
            trySend(activity, executor)
            assertFalse(snapshot.exists())
        }
        val old = PhoneControlRequest(grant.grantId, grant.revision, "77".repeat(16), grant.receiverIdentity,
            "88".repeat(16), "99".repeat(16), phoneControlSendPayload("old unknown"), "aa".repeat(16), 1, true, 3)
        assertEquals(PendingReservation.NEW, store.reserve(old))
        val original = store.pending()
        val bytes = snapshot.readBytes().toList()
        runScreen { activity, executor ->
            trySend(activity, executor)
            assertEquals(bytes, snapshot.readBytes().toList())
            assertEquals(original, PendingPhoneActionStore(app, keyAccess).pending())
        }
        val related = PhoneControlRequest(grant.grantId, grant.revision, "77".repeat(16), grant.receiverIdentity,
            target.controllerId, target.subjectId, phoneControlSendPayload("other unknown"), "bb".repeat(16), 1, true, 3)
        assertEquals(PendingReservation.NEW, store.reserve(related))
        val all = store.pending()
        runScreen { activity, executor ->
            val input = views(activity.window.decorView).filterIsInstance<EditText>().single()
            input.setText("another reviewed instruction")
            repeat(3) { executor.runAll(); shadowOf(Looper.getMainLooper()).idle() }
            val send = views(activity.window.decorView).filterIsInstance<Button>()
                .single { it.text.toString() == activity.getString(R.string.phone_send) }
            assertFalse(send.isEnabled)
            assertEquals(all, PendingPhoneActionStore(app, keyAccess).pending())
        }
    }
}
