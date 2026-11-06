package dev.louiselm.capture

import org.json.JSONArray
import org.json.JSONObject

// Synthetic wire fixtures from capture-service/tests/{attention_supply,attention_withdrawals}.rs.
// No physical-device observations, private profile bytes or real receiver credentials.
internal const val SUPPLY_REQUEST = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa"
internal val supplyPackageDigest = "sha256:" + "a".repeat(64)
internal const val SUPPLY_ENDORSER = "<b>Reviewer</b> https://example.invalid $(touch marker)"

internal fun supplyWithdrawal(): JSONObject = JSONObject()
    .put("subject_kind", "supply").put("subject_id", "1000")
    .put("kind", "endorsement_withdrawn").put("reason", "Endorsement was withdrawn")
    .put("source_operation_id", "11111111-2222-4333-8444-555555555555")
    .put("created_at_ms", 123).put("eligible", true)
    .put("withdrawal", JSONObject()
        .put("package_digest", supplyPackageDigest)
        .put("withdrawal_digest", "sha256:" + "b".repeat(64))
        .put("endorser", SUPPLY_ENDORSER).put("urgency", "relied_on"))

internal fun supplyApproval(): JSONObject = JSONObject()
    .put("subject_kind", "supply").put("subject_id", "1000")
    .put("kind", "skill_approval_pending").put("reason", "Skill approval is pending")
    .put("source_operation_id", SUPPLY_REQUEST)
    .put("created_at_ms", 124).put("eligible", true).put("code", "admission_required")

internal fun supplyAttentionPayload(): String = JSONObject().put("generation", 9)
    .put("items", JSONArray().put(supplyWithdrawal()).put(supplyApproval()).put(
        supplyWithdrawal().put("source_operation_id", "22222222-3333-4444-8555-666666666666").apply {
            getJSONObject("withdrawal").put("urgency", "subject_only")
        },
    )).toString()
