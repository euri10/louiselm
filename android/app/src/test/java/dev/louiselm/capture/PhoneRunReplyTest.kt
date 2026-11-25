package dev.louiselm.capture

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneRunReplyTest {
    private val operation = "cd".repeat(16)
    private val run = "11111111-1111-4111-8111-111111111111"
    private fun parse(value: JSONObject) = parsePhoneControlReply(value.toString().toByteArray(Charsets.UTF_8))
    private fun page() = JSONObject().put("v", 1).put("type", "run_page").put("request_id", operation)
        .put("items", JSONArray().put(JSONObject().put("run_id", run).put("revision", 2).put("state", "active")))
        .put("next_after", run)
    private fun detail() = JSONObject().put("v", 1).put("type", "run_detail").put("request_id", operation)
        .put("run", JSONObject().put("run_id", run).put("revision", 2).put("state", "active")
            .put("freshness", "durable_current").put("controller", "not_checked").put("has_session", true)
            .put("load_session", "unsupported").put("work", JSONObject().put("ceiling", 5).put("consumed", 2)
                .put("reserved", 1).put("remaining", 2)).put("pending_mutations", 1)
            .put("park_expires_at_ms", 0).put("park_expired", false).put("resume_pending", false).put("resume_deadline_ms", 0))

    @Test
    fun typedDurableRunPageAndDetailAreAccepted() {
        val page = parse(page()) as PhoneControlReply.RunPage
        assertEquals(PhoneRunState.ACTIVE, page.items.single().state)
        assertEquals(run, page.nextAfter)
        val parsed = parse(detail()) as PhoneControlReply.RunDetail
        assertEquals(run, parsed.run.summary.runId)
        assertEquals(PhoneRunLoad.UNSUPPORTED, parsed.run.loadSession)
        assertEquals(2L, parsed.run.work.remaining)
        val admitted = detail()
        admitted.getJSONObject("run").put("state", "admitted").put("has_session", false).put("load_session", "unknown")
        assertEquals(PhoneRunLoad.UNKNOWN, (parse(admitted) as PhoneControlReply.RunDetail).run.loadSession)
        val resuming = detail()
        resuming.getJSONObject("run").put("state", "resuming").put("resume_pending", true).put("resume_deadline_ms", 1234)
        assertTrue((parse(resuming) as PhoneControlReply.RunDetail).run.resumePending)
        val parked = detail()
        parked.getJSONObject("run").put("state", "cold_parked").put("park_expires_at_ms", 20).put("park_expired", true)
        assertTrue((parse(parked) as PhoneControlReply.RunDetail).run.parkExpired)
    }

    @Test
    fun contradictoryOrUnknownRunDataIsRefused() {
        val malformed = PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED)
        assertEquals(malformed, parse(page().put("next_after", "")))
        val wrongBudget = detail()
        wrongBudget.getJSONObject("run").getJSONObject("work").put("remaining", 5)
        assertEquals(malformed, parse(wrongBudget))
        val nullValue = detail()
        nullValue.getJSONObject("run").put("load_session", JSONObject.NULL)
        assertEquals(malformed, parse(nullValue))
        val invalidResume = detail()
        invalidResume.getJSONObject("run").put("resume_pending", true).put("resume_deadline_ms", 5)
        assertEquals(malformed, parse(invalidResume))
        val future = detail()
        future.getJSONObject("run").put("state", "future_state")
        assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.UNSUPPORTED), parse(future))
        val unsafeIdentity = page()
        unsafeIdentity.getJSONArray("items").getJSONObject(0).put("run_id", "AAAAAAAA-BBBB-4CCC-8DDD-EEEEEEEEEEEE")
        assertEquals(malformed, parse(unsafeIdentity))
        val extra = detail()
        extra.getJSONObject("run").put("working_dir", "/private")
        assertEquals(malformed, parse(extra))
    }
}
