package dev.louiselm.capture

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneSavedReviewReplyTest {
    private fun page() = JSONObject().put("v", 1).put("type", "saved_review")
        .put("request_id", "11".repeat(16)).put("run_id", "11111111-1111-4111-8111-111111111111")
        .put("result_id", "result").put("revision", "22".repeat(32)).put("producer_agent", "codex")
        .put("agents", JSONArray().put("codex")).put("destination", "/private/checkout")
        .put("base_commit", "a".repeat(40)).put("verification_digest", "sha256:" + "33".repeat(32))
        .put("plan_digest", "sha256:" + "44".repeat(32)).put("verifier_inputs_digest", "sha256:" + "55".repeat(32))
        .put("result_digest", "sha256:" + "66".repeat(32)).put("commands_passed", 1)
        .put("current_verification", "matching").put("current_provenance", "unknown")
        .put("file", JSONObject().put("path", "src/example").put("change", "modified").put("side", "before")
            .put("offset", 0).put("text", "hello").put("binary", false).put("executable", false))
        .put("next", JSONObject().put("file", 0).put("side", "after").put("offset", 0))

    private fun parse(value: JSONObject) = parsePhoneControlReply(value.toString().toByteArray(Charsets.UTF_8))

    @Test
    fun savedReviewIsBoundedAndClosed() {
        val valid = page()
        assertNotEquals(PhoneControlReply.Failure(PhoneControlOutcome.UNSUPPORTED), parse(valid))
        val review = (parse(valid) as PhoneControlReply.SavedReview).review
        assertEquals("hello", review.file?.text)
        assertEquals(PhoneSavedProvenance.UNKNOWN, review.provenance)
        assertEquals(PhoneSavedCursor(0, PhoneSavedSide.AFTER, 0), review.next)
        val longAgent = "a".repeat(200)
        val longAgentPage = page().put("producer_agent", longAgent).put("agents", JSONArray().put(longAgent))
        assertEquals(longAgent, (parse(longAgentPage) as PhoneControlReply.SavedReview).review.producer)
        val malformed = PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED)
        assertEquals(malformed, parse(page().put("extra", true)))
        val foreign = page().put("producer_agent", "other")
        assertEquals(malformed, parse(foreign))
        val binary = page()
        binary.getJSONObject("file").put("binary", true)
        assertEquals(malformed, parse(binary))
    }
}
