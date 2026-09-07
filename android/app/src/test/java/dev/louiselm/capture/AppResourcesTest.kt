package dev.louiselm.capture

import android.graphics.drawable.AdaptiveIconDrawable
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28, 34], qualifiers = "en")
class AppResourcesTest {
    @Test
    fun launcherUsesAnAdaptiveMask() {
        val application = RuntimeEnvironment.getApplication()
        assertTrue(application.applicationInfo.loadIcon(application.packageManager) is AdaptiveIconDrawable)
    }

    @Test
    fun countMessagesUseSingularAndPluralGrammar() {
        val application = RuntimeEnvironment.getApplication()
        val resources = application.resources
        for (name in listOf("confirm_first_pair", "confirm_receiver_migration", "attention_count")) {
            val id = resources.getIdentifier(name, "plurals", application.packageName)
            assertTrue("$name must be a quantity resource", id != 0)
            for (count in listOf(0, 1, 2)) {
                val text = resources.getQuantityString(id, count, count, "https://receiver.invalid")
                val phrase = if (name == "attention_count") {
                    if (count == 1) "1 item needs" else "$count items need"
                } else {
                    val adjective = if (name == "confirm_first_pair") "unowned pending" else "pending"
                    "$count $adjective " + if (count == 1) "capture to" else "captures to"
                }
                assertTrue("$name ($count): $text", text.contains(phrase))
            }
        }
    }
}
