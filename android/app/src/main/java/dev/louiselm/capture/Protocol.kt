package dev.louiselm.capture

import org.json.JSONObject
import org.json.JSONException
import java.net.URI

internal enum class UploadDisposition {
    SUCCESS,
    RETRY,
    OPERATOR_ACTION,
}

internal enum class PairingTransition {
    FIRST_PAIR,
    ENDPOINT_UPDATE,
    CREDENTIAL_RENEWAL,
    RECEIVER_MIGRATION,
}

internal enum class AttentionKind(val reason: String) {
    TURN_READY("Agent turn is ready"),
    PERMISSION_REQUIRED("Permission is required"),
    RUN_PARKED("Run is Parked"),
    SESSION_FAILED("Session failed"),
    SKILL_APPROVAL_PENDING("Skill approval is pending"),
    SKILL_UNVERIFIED("Skill supply is unverified"),
    ENDORSEMENT_WITHDRAWN("Endorsement was withdrawn"),
    ;

    companion object {
        fun parse(value: String): AttentionKind = when (value) {
            "turn_ready" -> TURN_READY
            "permission_required" -> PERMISSION_REQUIRED
            "run_parked" -> RUN_PARKED
            "session_failed" -> SESSION_FAILED
            "skill_approval_pending" -> SKILL_APPROVAL_PENDING
            "skill_unverified" -> SKILL_UNVERIFIED
            "endorsement_withdrawn" -> ENDORSEMENT_WITHDRAWN
            else -> throw IllegalArgumentException("attention kind is unsupported")
        }
    }
}

internal enum class AttentionCode {
    ADMISSION_REQUIRED,
    ROOT_TRUST_FAILED,
    SIGNATURE_INVALID,
    WITNESS_MISSING,
    NATIVE_SUPPLY_UNCERTAIN,
    RUNTIME_DRIFT,
    ISOLATION_FAILED,
    BROKER_UNAVAILABLE,
    AUDIT_PERSISTENCE_UNAVAILABLE,
    PROVIDER_DISCLOSURE_MISSING,
    EVIDENCE_MISSING,
    EVIDENCE_INVALIDATED,
    QUARANTINED,
    UNKNOWN_FAILURE,
    ;

    companion object {
        fun parse(value: String): AttentionCode = when (value) {
            "admission_required" -> ADMISSION_REQUIRED
            "root_trust_failed" -> ROOT_TRUST_FAILED
            "signature_invalid" -> SIGNATURE_INVALID
            "witness_missing" -> WITNESS_MISSING
            "native_supply_uncertain" -> NATIVE_SUPPLY_UNCERTAIN
            "runtime_drift" -> RUNTIME_DRIFT
            "isolation_failed" -> ISOLATION_FAILED
            "broker_unavailable" -> BROKER_UNAVAILABLE
            "audit_persistence_unavailable" -> AUDIT_PERSISTENCE_UNAVAILABLE
            "provider_disclosure_missing" -> PROVIDER_DISCLOSURE_MISSING
            "evidence_missing" -> EVIDENCE_MISSING
            "evidence_invalidated" -> EVIDENCE_INVALIDATED
            "quarantined" -> QUARANTINED
            "unknown_failure" -> UNKNOWN_FAILURE
            else -> throw IllegalArgumentException("attention code is unsupported")
        }
    }
}

internal enum class AttentionSubjectKind {
    SESSION,
    RUN,
    SUPPLY,
    ;

    companion object {
        fun parse(value: String): AttentionSubjectKind = when (value) {
            "session" -> SESSION
            "run" -> RUN
            "supply" -> SUPPLY
            else -> throw IllegalArgumentException("attention subject kind is unsupported")
        }
    }
}

internal enum class WithdrawalUrgency {
    RELIED_ON,
    SUBJECT_ONLY,
    ;

    companion object {
        fun parse(value: String): WithdrawalUrgency = when (value) {
            "relied_on" -> RELIED_ON
            "subject_only" -> SUBJECT_ONLY
            else -> throw IllegalArgumentException("withdrawal urgency is unsupported")
        }
    }
}

internal data class AttentionWithdrawal(
    val packageDigest: String,
    val withdrawalDigest: String,
    val endorser: String,
    val urgency: WithdrawalUrgency,
)

internal data class AttentionItem(
    val subjectKind: AttentionSubjectKind,
    val subjectId: String,
    val kind: AttentionKind,
    val sourceOperationId: String,
    val createdAtMs: Long,
    val eligible: Boolean,
    val linkedRunId: String?,
    val stage: String?,
    val code: AttentionCode?,
    val withdrawal: AttentionWithdrawal? = null,
    val sessionName: String? = null,
)

internal data class AttentionSnapshot(
    val generation: Long,
    val items: List<AttentionItem>,
) {
    companion object {
        fun parse(payload: String): AttentionSnapshot {
            require(payload.toByteArray(Charsets.UTF_8).size <= MAX_ATTENTION_RESPONSE) { "attention response is too large" }
            val value = JSONObject(payload)
            requireKeys(value, setOf("generation", "items"), "attention snapshot")
            val generation = requireLong(value, "generation")
            require(generation >= 0) { "attention generation is invalid" }
            val array = value.getJSONArray("items")
            val items = ArrayList<AttentionItem>(array.length())
            for (index in 0 until array.length()) {
                items += parseItem(array.getJSONObject(index))
            }
            return AttentionSnapshot(generation, items)
        }

        private fun parseItem(value: JSONObject): AttentionItem {
            requireKeys(
                value,
                setOf(
                    "subject_kind",
                    "subject_id",
                    "session_name",
                    "kind",
                    "source_operation_id",
                    "created_at_ms",
                    "eligible",
                    "reason",
                    "linked_run_id",
                    "stage",
                    "code",
                    "withdrawal",
                ),
                "attention item",
            )
            val subjectKind = AttentionSubjectKind.parse(requireString(value, "subject_kind"))
            val subjectId = requireString(value, "subject_id")
            val sessionName = optionalString(value, "session_name")
            if (sessionName != null) {
                require(subjectKind == AttentionSubjectKind.SESSION && sessionName.isNotBlank() &&
                    sessionName.toByteArray(Charsets.UTF_8).size <= 256 &&
                    sessionName.none { it.isISOControl() || it == '\u2028' || it == '\u2029' }) {
                    "attention session_name is invalid"
                }
            }
            require(validAttentionId(subjectId)) { "attention subject id is invalid" }
            if (subjectKind == AttentionSubjectKind.RUN) {
                require(validCanonicalUuid(subjectId)) { "attention Run id is invalid" }
            }
            val kind = AttentionKind.parse(requireString(value, "kind"))
            if (subjectKind == AttentionSubjectKind.SUPPLY) {
                val uid = subjectId.toLongOrNull()
                require(uid != null && uid in 1..4294967295L && uid.toString() == subjectId) {
                    "attention supply id is invalid"
                }
                require(kind == AttentionKind.SKILL_APPROVAL_PENDING || kind == AttentionKind.ENDORSEMENT_WITHDRAWN) {
                    "attention supply kind is invalid"
                }
            }
            if (kind == AttentionKind.ENDORSEMENT_WITHDRAWN) {
                require(subjectKind == AttentionSubjectKind.SUPPLY) { "withdrawal requires a supply subject" }
            }
            val sourceOperationId = requireString(value, "source_operation_id")
            require(validCanonicalUuid(sourceOperationId)) { "attention operation id is invalid" }
            val createdAtMs = requireLong(value, "created_at_ms")
            require(createdAtMs > 0) { "attention creation time is invalid" }
            val reason = requireString(value, "reason")
            require(reason == kind.reason) { "attention reason is invalid" }
            val linkedRunId = optionalString(value, "linked_run_id")
            if (linkedRunId != null) require(validCanonicalUuid(linkedRunId)) { "attention linked Run id is invalid" }
            val stage = optionalString(value, "stage")
            if (subjectKind == AttentionSubjectKind.SUPPLY) {
                require(linkedRunId == null && stage == null) { "supply Attention cannot carry Run or stage context" }
            }
            if (stage != null) {
                require(stage.isNotEmpty() && stage.toByteArray(Charsets.UTF_8).size <= MAX_ATTENTION_STAGE)
                require(stage.all { it.isLetterOrDigit() || it in "._/-" }) { "attention stage is invalid" }
            }
            val code = optionalString(value, "code")?.let(AttentionCode::parse)
            val validCode = when (kind) {
                AttentionKind.SKILL_APPROVAL_PENDING -> code == AttentionCode.ADMISSION_REQUIRED
                AttentionKind.SKILL_UNVERIFIED -> code != null && code != AttentionCode.ADMISSION_REQUIRED
                else -> code == null
            }
            require(validCode) { "attention code does not match kind" }
            if (kind == AttentionKind.SKILL_APPROVAL_PENDING || kind == AttentionKind.SKILL_UNVERIFIED) {
                require(stage == null) { "skill Attention cannot carry stage text" }
            }
            val withdrawal = if (value.has("withdrawal") && !value.isNull("withdrawal")) {
                parseWithdrawal(value.getJSONObject("withdrawal"))
            } else null
            require((kind == AttentionKind.ENDORSEMENT_WITHDRAWN) == (withdrawal != null)) {
                "attention withdrawal does not match kind"
            }
            val eligible = value.get("eligible")
            require(eligible is Boolean) { "attention eligibility is invalid" }
            return AttentionItem(
                subjectKind,
                subjectId,
                kind,
                sourceOperationId,
                createdAtMs,
                eligible,
                linkedRunId,
                stage,
                code,
                withdrawal,
                sessionName,
            )
        }

        private fun parseWithdrawal(value: JSONObject): AttentionWithdrawal {
            requireKeys(value, setOf("package_digest", "withdrawal_digest", "endorser", "urgency"), "withdrawal")
            val packageDigest = requireString(value, "package_digest")
            val withdrawalDigest = requireString(value, "withdrawal_digest")
            for (digest in listOf(packageDigest, withdrawalDigest)) {
                require(digest.matches(Regex("sha256:[0-9a-f]{64}"))) { "withdrawal digest is invalid" }
            }
            val endorser = requireString(value, "endorser")
            require(endorser.isNotBlank() && endorser.toByteArray(Charsets.UTF_8).size <= 256 &&
                endorser.none { it.isISOControl() }) { "withdrawal endorser is invalid" }
            return AttentionWithdrawal(
                packageDigest, withdrawalDigest, endorser, WithdrawalUrgency.parse(requireString(value, "urgency")),
            )
        }

        // Android JSONObject getters coerce strings, booleans and numbers. The closed wire schema does not.
        private fun requireString(value: JSONObject, key: String): String {
            val field = value.get(key)
            require(field is String) { "attention $key must be a string" }
            return field
        }

        private fun requireLong(value: JSONObject, key: String): Long = when (val field = value.get(key)) {
            is Int -> field.toLong()
            is Long -> field
            else -> throw IllegalArgumentException("attention $key must be an integer in range")
        }

        private fun optionalString(value: JSONObject, key: String): String? {
            if (!value.has(key) || value.isNull(key)) return null
            return requireString(value, key)
        }

        private fun requireKeys(value: JSONObject, allowed: Set<String>, label: String) {
            val keys = value.keys()
            while (keys.hasNext()) require(keys.next() in allowed) { "$label contains an unsupported field" }
        }

        private const val MAX_ATTENTION_RESPONSE = 64 * 1024
        private const val MAX_ATTENTION_STAGE = 64
    }
}

internal sealed class AttentionFetch {
    data class Success(val snapshot: AttentionSnapshot) : AttentionFetch()
    data class Retry(val message: String) : AttentionFetch()
    data class OperatorAction(val message: String) : AttentionFetch()
}

/** Convert untrusted private-inbox JSON into a sanitized fetch result, without effects. */
internal fun parseAttentionResponse(payload: String): AttentionFetch = try {
    AttentionFetch.Success(AttentionSnapshot.parse(payload))
} catch (_: JSONException) {
    AttentionFetch.OperatorAction("receiver returned an invalid Attention inbox")
} catch (_: IllegalArgumentException) {
    AttentionFetch.OperatorAction("receiver returned an invalid Attention inbox")
}

internal fun validCanonicalUuid(value: String): Boolean {
    if (value.length != 36 || value.lowercase() != value) return false
    if (listOf(8, 13, 18, 23).any { value[it] != '-' }) return false
    val compact = value.replace("-", "")
    return compact.length == 32 && compact.all { it in '0'..'9' || it in 'a'..'f' }
}

private fun validAttentionId(value: String): Boolean =
    value.isNotEmpty() &&
        value.toByteArray(Charsets.UTF_8).size <= 256 &&
        value.none { it.isWhitespace() || it.isISOControl() }

internal fun pairingTransition(currentIdentity: String?, offeredIdentity: String): PairingTransition = when {
    currentIdentity == null -> PairingTransition.FIRST_PAIR
    currentIdentity == offeredIdentity -> PairingTransition.ENDPOINT_UPDATE
    else -> PairingTransition.RECEIVER_MIGRATION
}

internal fun queueBelongsToReceiver(ownerIdentity: String?, receiverIdentity: String): Boolean =
    ownerIdentity != null && ownerIdentity == receiverIdentity

internal fun uploadDisposition(status: Int): UploadDisposition = when {
    status == 200 || status == 201 -> UploadDisposition.SUCCESS
    status == 408 || status == 429 || status >= 500 -> UploadDisposition.RETRY
    else -> UploadDisposition.OPERATOR_ACTION
}

internal fun decodeSha256(value: String): ByteArray? {
    if (value.length != 64 || value.any { it.digitToIntOrNull(16) == null }) return null
    return ByteArray(32) { index -> value.substring(index * 2, index * 2 + 2).toInt(16).toByte() }
}

internal fun validReceiverUrl(value: String): Boolean = runCatching {
    val uri = URI(value)
    uri.scheme == "https" &&
        !uri.host.isNullOrBlank() &&
        uri.userInfo == null &&
        uri.fragment == null &&
        (uri.path.isNullOrEmpty() || uri.path == "/")
}.getOrDefault(false)
