package dev.louiselm.capture

import org.junit.Assert.assertFalse
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import android.widget.Button
import android.widget.TextView
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

    @Test
    fun liveOwnerAndLinkedSessionAreExplicitAndBoundToTheObservedIncarnation() {
        val context = RuntimeEnvironment.getApplication()
        val run = "11111111-1111-4111-8111-111111111111"
        val controller = "11".repeat(16)
        val owner = "22".repeat(16)
        val subject = "33".repeat(16)
        val live = PhoneRunLiveView(controller, owner, PhoneRunOwnerStatus.ACTIVE, PhoneRunPhase.WORKING,
            1, 2, "b-1", listOf(PhoneRunLiveSession(subject, "codex", "ready")), false)
        val reply = PhoneControlReply.RunLive("44".repeat(16), run, 3, PhoneRunController.CONNECTED, live)
        var selected: PhoneRunSessionTarget? = null
        val views = phoneRunLiveViews(context, reply) { selected = it }
        assertTrue(views.filterIsInstance<TextView>().any { it.text.toString().contains("b-1") })
        views.filterIsInstance<Button>().single().performClick()
        assertEquals(PhoneRunSessionTarget(run, 3, owner, PhoneControlTarget(controller, subject)), selected)
        val absent = phoneRunLiveViews(context, reply.copy(controller = PhoneRunController.ABSENT, live = null)) { }
        val unavailable = phoneRunLiveViews(context, reply.copy(controller = PhoneRunController.UNREACHABLE, live = null)) { }
        assertTrue(absent.filterIsInstance<TextView>().any { it.text.toString().contains("No live") })
        assertTrue(unavailable.filterIsInstance<TextView>().any { it.text.toString().contains("could not be verified") })
    }
}
