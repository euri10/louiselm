package dev.louiselm.capture

import org.json.JSONArray
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28, 34, 37])
class AttentionSupplyTest {
    @Test
    fun supplyInboxAcceptsBothUrgenciesAndApprovalAndSurvivesCacheRestart() {
        val result = parseAttentionResponse(supplyAttentionPayload())
        assertTrue(result.toString(), result is AttentionFetch.Success)
        val snapshot = (result as AttentionFetch.Success).snapshot
        assertEquals(listOf("SUPPLY", "SUPPLY", "SUPPLY"), snapshot.items.map { it.subjectKind.name })
        assertEquals(SUPPLY_REQUEST, snapshot.items[1].sourceOperationId)
        val context = RuntimeEnvironment.getApplication()
        assertTrue(AttentionState(context) { "pair" }.cache("pair", snapshot))
        assertEquals(snapshot, AttentionState(context) { "pair" }.cached("pair"))
    }

    @Test
    fun supplySubjectRequiresCanonicalNonzeroU32AndClosedKinds() {
        for (id in listOf("1", "1000", "4294967295")) {
            accepted(supplyApproval().put("subject_id", id))
        }
        for (id in listOf("0", "00", "01000", "+1000", "-1", "1.0", "1e3", "4294967296", "session", "١", "1\n")) {
            rejected(supplyApproval().put("subject_id", id))
        }
        for ((kind, reason) in listOf(
            "turn_ready" to "Agent turn is ready", "permission_required" to "Permission is required",
            "run_parked" to "Run is Parked", "session_failed" to "Session failed",
            "skill_unverified" to "Skill supply is unverified", "future_kind" to "Future kind",
        )) {
            rejected(supplyApproval().put("kind", kind).put("reason", reason))
        }
        for (subject in listOf("session", "run", "unknown")) {
            rejected(supplyWithdrawal().put("subject_kind", subject)
                .put("subject_id", "11111111-2222-4333-8444-555555555555"))
        }
    }

    @Test
    fun supplyRejectsContradictoryContextAndPrivateOrUnknownFields() {
        for (item in listOf(supplyWithdrawal(), supplyApproval())) {
            for ((field, value) in listOf(
                "linked_run_id" to SUPPLY_REQUEST, "stage" to "review",
                "reason" to "remote prose", "profile" to "private bytes",
                "dossier" to "private diff", "path" to "/private/profile",
                "next_action" to "quarantine now",
            )) rejected(JSONObject(item.toString()).put(field, value))
        }
        rejected(supplyWithdrawal().put("code", "admission_required"))
        rejected(supplyApproval().put("code", "quarantined"))
        rejected(supplyApproval().apply { remove("code") })
        rejected(supplyApproval().put("withdrawal", supplyWithdrawal().getJSONObject("withdrawal")))
        for (subject in listOf("session", "run")) {
            rejected(supplyApproval().put("subject_kind", subject).put("subject_id", SUPPLY_REQUEST)
                .put("withdrawal", supplyWithdrawal().getJSONObject("withdrawal")))
        }
        for (field in listOf("subject_kind", "subject_id", "kind", "source_operation_id", "created_at_ms", "eligible", "reason", "withdrawal")) {
            rejected(supplyWithdrawal().apply { remove(field) })
            rejected(supplyWithdrawal().put(field, JSONObject.NULL))
        }
        accepted(supplyApproval().put("withdrawal", JSONObject.NULL).put("stage", JSONObject.NULL).put("linked_run_id", JSONObject.NULL))
    }

    @Test
    fun withdrawalMetadataIsClosedTypedAndByteBounded() {
        for (field in listOf("package_digest", "withdrawal_digest", "endorser", "urgency")) {
            rejected(supplyWithdrawal().apply { getJSONObject("withdrawal").remove(field) })
            for (invalid in listOf(JSONObject.NULL, 1, true, JSONArray(), JSONObject())) {
                rejected(supplyWithdrawal().apply { getJSONObject("withdrawal").put(field, invalid) })
            }
        }
        for (field in listOf("package_digest", "withdrawal_digest")) {
            for (digest in listOf("a".repeat(64), "sha256:" + "A".repeat(64), "sha256:" + "a".repeat(63))) {
                rejected(supplyWithdrawal().apply { getJSONObject("withdrawal").put(field, digest) })
            }
        }
        for (label in listOf("", " \t", "\u00a0\u2003", "a".repeat(257), "é".repeat(129), "review\nprivate", "review\u0085private", "\u0000")) {
            rejected(supplyWithdrawal().apply { getJSONObject("withdrawal").put("endorser", label) })
        }
        for (label in listOf(SUPPLY_ENDORSER, "é".repeat(128), "key sha256:" + "a".repeat(64))) {
            accepted(supplyWithdrawal().apply { getJSONObject("withdrawal").put("endorser", label) })
        }
        rejected(supplyWithdrawal().apply { getJSONObject("withdrawal").put("urgency", "critical") })
        rejected(supplyWithdrawal().apply { getJSONObject("withdrawal").put("next_action", "remote command") })
    }

    @Test
    fun androidJsonCoercionsCannotTurnMalformedInboxFieldsIntoValidValues() {
        // Use a supported Session item so these checks also fail before supply support exists.
        val session = JSONObject(invalidatedAttentionPayload).getJSONArray("items").getJSONObject(0)
        for ((field, value) in listOf(
            "subject_id" to 1000, "eligible" to "true", "created_at_ms" to "123",
            "created_at_ms" to 1.5, "created_at_ms" to 1e30,
        )) rejected(JSONObject(session.toString()).put(field, value))
        for (generation in listOf("9", 1.5, 1e30, JSONObject.NULL)) {
            val payload = JSONObject().put("generation", generation).put("items", JSONArray()).toString()
            assertEquals(invalidInbox, parseAttentionResponse(payload))
        }
        rejected(supplyApproval().put("subject_id", 1000))
    }

    @Test
    fun responseLimitCountsUtf8BytesBeforeParsing() {
        val item = supplyWithdrawal().apply { getJSONObject("withdrawal").put("endorser", "é".repeat(128)) }
        val items = JSONArray()
        var response = payload(item)
        while (response.toByteArray(Charsets.UTF_8).size <= 64 * 1024) {
            items.put(item)
            response = JSONObject().put("generation", 9).put("items", items).toString()
        }
        assertTrue(response.length < 64 * 1024)
        assertEquals(invalidInbox, parseAttentionResponse(response))
    }

    private fun accepted(item: JSONObject) {
        val result = parseAttentionResponse(payload(item))
        assertTrue(result.toString(), result is AttentionFetch.Success)
    }

    private fun rejected(item: JSONObject) {
        assertEquals(item.toString(), invalidInbox, parseAttentionResponse(payload(item)))
    }

    private fun payload(item: JSONObject): String = JSONObject()
        .put("generation", 9).put("items", JSONArray().put(item)).toString()

    private val invalidInbox = AttentionFetch.OperatorAction("receiver returned an invalid Attention inbox")
}
