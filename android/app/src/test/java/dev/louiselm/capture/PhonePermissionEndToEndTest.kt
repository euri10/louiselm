package dev.louiselm.capture

import android.app.KeyguardManager
import android.os.Looper
import android.os.PowerManager
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.EditText
import android.widget.TextView
import java.net.Socket
import java.util.concurrent.ExecutorService
import org.json.JSONObject
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
@Config(sdk = [30], shadows = [EndToEndPairingFixture::class, EndToEndKeysFixture::class, EndToEndAuthenticatorFixture::class,
    PhoneScreenOfflineUnlockFixture::class, PhoneScreenOfflineAuthenticatorFixture::class],
    instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhonePermissionEndToEndTest {
    private fun views(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(views(view.getChildAt(index)))
    }

    @Test
    fun currentRequestRetiresAfterDesktopResolution() {
        assumeTrue(System.getenv("LOUISELM_E2E_PERMISSION_PORT") != null)
        val app = RuntimeEnvironment.getApplication()
        shadowOf(app).grantPermissions("${app.packageName}.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION")
        shadowOf(app.getSystemService(PowerManager::class.java)).turnScreenOn(true)
        shadowOf(app.getSystemService(KeyguardManager::class.java)).setIsDeviceLocked(false)
        val config = requireNotNull(PairingStore(app).load())
        val key = JSONObject(requireNotNull(javaClass.getResourceAsStream("/phone-control-v1.json"))
            .bufferedReader().use { it.readText() }).getJSONObject("proof").getString("enrolled_key_hash")
        PhoneControlStore(app).saveGrant(PhoneControlGrant(requireNotNull(System.getenv("LOUISELM_E2E_GRANT")), 1,
            config.receiverIdentitySha256, phoneControlPairingBinding(config), key))
        val screen = Robolectric.buildActivity(PhoneControlActivity::class.java)
        val activity = screen.get()
        ReflectionHelpers.getField<ExecutorService>(activity, "networkExecutor").shutdown()
        val executor = PausedExecutorService()
        ReflectionHelpers.setField(activity, "networkExecutor", executor)
        fun drain() { executor.runAll(); shadowOf(Looper.getMainLooper()).idle() }
        fun button(text: String) = views(activity.window.decorView).filterIsInstance<Button>().single { it.text.toString() == text }
        fun visible(text: String) = views(activity.window.decorView).filterIsInstance<TextView>().any { it.isShown && it.text.toString() == text }
        try {
            screen.setup()
            drain()
            button(activity.getString(R.string.phone_unlock)).performClick()
            repeat(12) { drain() }
            views(activity.window.decorView).filterIsInstance<Button>()
                .single { it.text.toString().contains(requireNotNull(System.getenv("LOUISELM_E2E_SUBJECT"))) }.performClick()
            repeat(3) { drain() }
            assertTrue(visible("Current permission request: Edit mock file"))
            assertTrue(visible("Offered choice: Allow once"))
            assertTrue(visible("Offered choice: Reject"))
            assertFalse(views(activity.window.decorView).filterIsInstance<Button>().any { it.text.toString().contains("Offered choice:") })
            val draft = views(activity.window.decorView).filterIsInstance<EditText>().single()
            draft.setText("Unsent fixture instruction")
            repeat(3) { drain() }
            assertFalse(button(activity.getString(R.string.phone_send)).isEnabled)
            draft.setText("")
            repeat(3) { drain() }
            assertFalse(views(activity.window.decorView).filterIsInstance<TextView>().any { it.isShown && it.text.toString().contains("mock.txt") })
            button(activity.getString(R.string.phone_lock)).performClick()
            assertFalse(visible("Current permission request: Edit mock file"))
            button(activity.getString(R.string.phone_unlock)).performClick()
            repeat(12) { drain() }
            button(activity.getString(R.string.phone_sessions)).performClick()
            repeat(3) { drain() }
            views(activity.window.decorView).filterIsInstance<Button>()
                .single { it.text.toString().contains(requireNotNull(System.getenv("LOUISELM_E2E_SUBJECT"))) }.performClick()
            repeat(3) { drain() }
            assertTrue(visible("Offered choice: Allow once"))
            Socket("127.0.0.1", requireNotNull(System.getenv("LOUISELM_E2E_PERMISSION_PORT")).toInt()).use { socket ->
                socket.soTimeout = 30_000
                socket.getOutputStream().write("resolve\n".toByteArray())
                assertTrue(socket.getInputStream().bufferedReader().readLine() == "resolved")
            }
            button(activity.getString(R.string.phone_refresh)).performClick()
            repeat(3) { drain() }
            assertFalse(visible("Current permission request: Edit mock file"))
            assertFalse(visible("Offered choice: Allow once"))
            assertFalse(button(activity.getString(R.string.phone_send)).isEnabled)
            button(activity.getString(R.string.phone_lock)).performClick()
            assertFalse(visible("Offered choice: Reject"))
        } finally {
            screen.close()
            executor.runAll()
            shadowOf(Looper.getMainLooper()).idle()
            executor.shutdownNow()
        }
    }
}
