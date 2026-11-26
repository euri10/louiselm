package dev.louiselm.capture

import android.app.KeyguardManager
import android.os.Looper
import android.os.PowerManager
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.TextView
import java.io.File
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
@Config(sdk = [30], shadows = [EndToEndPairingFixture::class, EndToEndKeysFixture::class,
    EndToEndAuthenticatorFixture::class, PhoneScreenOfflineUnlockFixture::class,
    PhoneScreenOfflineAuthenticatorFixture::class], instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneSelectionEndToEndTest {
    private fun views(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(views(view.getChildAt(index)))
    }

    @Test
    fun selectionUsesTheCurrentOwnerAndNeverStartsAnAgent() {
        assumeTrue(System.getenv("LOUISELM_E2E_SELECTION_PORT") != null)
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
        fun text() = views(activity.window.decorView).filterIsInstance<TextView>().joinToString("\n") { it.text.toString() }
        val project = File(requireNotNull(System.getenv("LOUISELM_E2E_SELECTION_PROJECT")))
        val moved = File(project.parentFile, "temporarily-removed")
        try {
            screen.setup()
            drain()
            button(activity.getString(R.string.phone_unlock)).performClick()
            repeat(12) { drain() }
            assertTrue(text().contains(activity.getString(R.string.phone_no_sessions)))
            button("Session choices").performClick()
            repeat(3) { drain() }
            button("Neovim 1").performClick()
            repeat(3) { drain() }
            assertTrue(text().contains(project.path))
            assertFalse(text().contains("unapproved"))
            assertFalse(text().contains("removed-agent"))
            assertFalse(text().contains("foreign-project"))
            assertFalse(text().contains("removed-project"))
            button(project.path).performClick()
            assertFalse(text().contains("unapproved"))
            assertFalse(text().contains("removed-agent"))
            button("codex").performClick()
            assertTrue(text().contains("Agent: codex"))
            assertTrue(project.renameTo(moved))
            button(activity.getString(R.string.phone_refresh)).performClick()
            repeat(3) { drain() }
            assertFalse(text().contains(project.path))
            assertTrue(moved.renameTo(project))
            button(activity.getString(R.string.phone_refresh)).performClick()
            repeat(3) { drain() }
            assertTrue(text().contains(project.path))
            Socket("127.0.0.1", requireNotNull(System.getenv("LOUISELM_E2E_SELECTION_PORT")).toInt()).use { socket ->
                socket.getOutputStream().write("replace\n".toByteArray())
                assertTrue(socket.getInputStream().bufferedReader().readLine() == "replaced")
            }
            button(activity.getString(R.string.phone_refresh)).performClick()
            repeat(3) { drain() }
            assertFalse(text().contains(project.path))
            button(activity.getString(R.string.phone_sessions)).performClick()
            repeat(3) { drain() }
            button("Session choices").performClick()
            repeat(3) { drain() }
            button("Neovim 1").performClick()
            repeat(3) { drain() }
            assertTrue(text().contains(project.path))
            button(activity.getString(R.string.phone_lock)).performClick()
            assertFalse(text().contains(project.path))
        } finally {
            if (moved.exists()) assertTrue(moved.renameTo(project))
            screen.close()
            executor.runAll()
            shadowOf(Looper.getMainLooper()).idle()
            executor.shutdownNow()
        }
    }
}
