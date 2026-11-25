package dev.louiselm.capture

import android.app.KeyguardManager
import android.os.Looper
import android.os.PowerManager
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.TextView
import java.util.concurrent.ExecutorService
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
import org.robolectric.android.util.concurrent.PausedExecutorService
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.util.ReflectionHelpers

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30], shadows = [EndToEndPairingFixture::class, EndToEndKeysFixture::class, EndToEndAuthenticatorFixture::class],
    instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneControlBrowseEndToEndTest {
    private fun views(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(views(view.getChildAt(index)))
    }

    @Test
    fun opensTheExactRealSessionFromTheWorkspaceWithoutAttention() {
        assumeTrue(System.getenv("LOUISELM_E2E_URL") != null)
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
        try {
            screen.setup()
            drain()
            views(activity.window.decorView).filterIsInstance<Button>()
                .single { it.text.toString() == activity.getString(R.string.phone_unlock) }.performClick()
            repeat(5) { drain() }
            val subject = requireNotNull(System.getenv("LOUISELM_E2E_OTHER_SUBJECT"))
            val rows = views(activity.window.decorView).filterIsInstance<Button>()
                .filter { it.text.toString().contains("same") }.toList()
            assertEquals(2, rows.size)
            screen.pause().stop()
            assertTrue(rows.none { it.isShown })
            screen.restart().start().resume()
            repeat(3) { drain() }
            val refreshed = views(activity.window.decorView).filterIsInstance<Button>()
                .filter { it.text.toString().contains("same") }.toList()
            assertEquals(2, refreshed.size)
            refreshed.single { it.text.toString().contains(subject) }.performClick()
            repeat(2) { drain() }
            assertTrue(views(activity.window.decorView).filterIsInstance<TextView>()
                .any { it.text.toString() == "You: other fixture request" })
        } finally {
            screen.close()
            executor.runAll()
            shadowOf(Looper.getMainLooper()).idle()
            executor.shutdownNow()
        }
    }
}
