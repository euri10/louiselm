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
class PhonePreparedReplyTest {
    private val registration = "ab".repeat(16)
    private val operation = "cd".repeat(16)
    private val run = "11111111-1111-4111-8111-111111111111"
    private val digest = "sha256:" + "01".repeat(32)

    private fun parse(value: JSONObject) = parsePhoneControlReply(value.toString().toByteArray(Charsets.UTF_8))

    private fun page() = JSONObject().put("v", 1).put("type", "prepared_page").put("request_id", operation)
        .put("items", JSONArray().put(JSONObject().put("registration_id", registration).put("run_id", run)
            .put("revision", 1).put("state", "prepared"))).put("next_after", registration)

    private fun detail(prompt: String = "review /intentional/path"): JSONObject {
        val beads = JSONArray().put(JSONObject().put("id", "a").put("prompt", prompt))
            .put(JSONObject().put("id", "b").put("prompt", "second"))
            .put(JSONObject().put("id", "c").put("prompt", "third"))
        val scope = JSONObject().put("project_digest", digest).put("role", "coordinator")
            .put("issue_ids", JSONArray(listOf("a", "b", "c")))
            .put("effects", JSONArray().put(JSONObject().put("kind", "comment_add")))
            .put("max_mutations", 3).put("expires_at_ms", 0)
        val provider = JSONObject().put("disclosure_profile", digest).put("provider", "openai")
            .put("upstream", "https://api.openai.com/v1/responses").put("addresses", JSONArray(listOf("127.0.0.1")))
            .put("models", JSONArray(listOf("gpt-5.6-luna"))).put("max_effort", "low")
            .put("max_run_requests", 3).put("expires_at_ms", 0)
        val skills = JSONObject().put("mode", "off").put("view_digest", digest)
        val requests = JSONObject().put("enabled", false).put("agents", JSONArray())
            .put("allow_run", false).put("allow_supply", false)
        val body = JSONObject().put("state", "prepared").put("agent", "codex").put("beads", beads)
            .put("execution", JSONObject().put("kind", "duration").put("duration_ms", 1_800_000))
            .put("max_sessions", 6).put("verification_plan_digest", digest).put("bead_scope", scope)
            .put("provider_requests", provider).put("skill_requests", requests).put("skills", skills).put("network", "denied")
        return JSONObject().put("v", 1).put("type", "prepared_detail").put("request_id", operation)
            .put("registration_id", registration).put("run_id", run).put("revision", 1).put("detail", body)
    }

    @Test
    fun boundedPageAndExactDetailAreTypedAndPreserveLongInstructions() {
        val listed = parse(page())
        assertTrue(listed is PhoneControlReply.PreparedPage)
        assertEquals(registration, (listed as PhoneControlReply.PreparedPage).items.single().registrationId)
        val prompt = "x".repeat(9_000) + "/intentional/path"
        val parsed = parse(detail(prompt))
        assertTrue(parsed is PhoneControlReply.PreparedDetail)
        parsed as PhoneControlReply.PreparedDetail
        assertEquals(prompt, parsed.detail.beads.first().prompt)
        assertEquals(digest, parsed.detail.verificationPlanDigest)
        assertEquals(3L, parsed.detail.provider.maxRequests)
        assertEquals(1_800_000L, parsed.detail.execution.value)
        val unicode = detail()
        val body = unicode.getJSONObject("detail")
        body.getJSONArray("beads").getJSONObject(0).put("id", "\uD800\uDC00")
        body.getJSONArray("beads").getJSONObject(1).put("id", "\uE000")
        body.getJSONObject("bead_scope").put("issue_ids", JSONArray(listOf("c", "\uE000", "\uD800\uDC00")))
        assertTrue(parse(unicode) is PhoneControlReply.PreparedDetail)
        val enabled = detail()
        val enabledBody = enabled.getJSONObject("detail")
        enabledBody.put("execution", JSONObject().put("kind", "stop_by").put("expires_at_ms", 1234))
        enabledBody.put("skills", JSONObject().put("mode", "enabled").put("view_digest", digest).put("generation_digest", digest))
        enabledBody.put("skill_requests", JSONObject().put("enabled", true).put("agents", JSONArray(listOf("codex")))
            .put("allow_run", true).put("allow_supply", false))
        val enabledReply = parse(enabled) as PhoneControlReply.PreparedDetail
        assertEquals("stop_by", enabledReply.detail.execution.kind)
        assertEquals(1234L, enabledReply.detail.execution.value)
        assertEquals(digest, enabledReply.detail.skills.generationDigest)
        val mapped = detail()
        mapped.getJSONObject("detail").getJSONObject("provider_requests")
            .put("addresses", JSONArray(listOf("::ffff:127.0.0.1", "127.0.0.1")))
        assertTrue(parse(mapped) is PhoneControlReply.PreparedDetail)
    }

    @Test
    fun rejectsAmbiguousOrContradictoryPreparedData() {
        val malformed = PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED)
        assertEquals(malformed, parse(page().put("next_after", "")))
        assertEquals(malformed, parse(page().put("items", JSONArray().put(JSONObject()).put(JSONObject()))))
        val invalidUrl = detail()
        invalidUrl.getJSONObject("detail").getJSONObject("provider_requests")
            .put("upstream", "https://secret@api.openai.com/v1/responses")
        assertEquals(malformed, parse(invalidUrl))
        val wrongScope = detail()
        wrongScope.getJSONObject("detail").getJSONObject("bead_scope").put("issue_ids", JSONArray(listOf("a", "b", "other")))
        assertEquals(malformed, parse(wrongScope))
        val extra = detail()
        extra.getJSONObject("detail").put("local_path", "/private")
        assertEquals(malformed, parse(extra))
        val active = detail()
        active.getJSONObject("detail").getJSONObject("provider_requests").put("expires_at_ms", 1)
        assertEquals(malformed, parse(active))
        val duplicateAddress = detail()
        duplicateAddress.getJSONObject("detail").getJSONObject("provider_requests")
            .put("addresses", JSONArray(listOf("::1", "0:0:0:0:0:0:0:1")))
        assertEquals(malformed, parse(duplicateAddress))
        val raw = detail().toString()
        val duplicate = raw.replace("\"state\":\"prepared\"", "\"state\":\"prepared\",\"state\":\"prepared\"")
        assertTrue(duplicate != raw)
        assertEquals(malformed, parsePhoneControlReply(duplicate.toByteArray(Charsets.UTF_8)))
        assertEquals(malformed, parse(detail("x".repeat(65_537))))
        val session = """{"v":2,"type":"session_page","controller_id":"${"01".repeat(16)}","subject_id":"${"02".repeat(16)}","operation_id":"${"03".repeat(16)}","revision":1,"status":"ready","entries":[{"kind":"message","seq":1,"role":"assistant","text":"${"x".repeat(8193)}","truncated":false}],"next_before":0,"complete":false,"source":{"replay":"not_applicable","archive":"unavailable","redaction":"unknown","missing":false,"truncated":false}}"""
        assertEquals(malformed, parsePhoneControlReply(session.toByteArray(Charsets.UTF_8)))
    }
}
