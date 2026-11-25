package dev.louiselm.capture

import org.junit.Assert.assertFalse
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneRunViewsTest {
    @Test
    fun activeRunDoesNotShowAnOldParkExpiryAsCurrent() {
        val summary = PhoneRunSummary("11111111-1111-4111-8111-111111111111", 3, PhoneRunState.ACTIVE)
        val view = PhoneRunView(summary, true, PhoneRunLoad.SUPPORTED, PhoneRunWork(5, 2, 1, 2), 1, 1234, false, false, 0)
        val text = phoneRunDetailViews(RuntimeEnvironment.getApplication(), view).joinToString("\n") { it.text.toString() }
        assertFalse(text.contains("Park expires"))
        assertFalse(text.contains("Park expired"))
    }
}
