package dev.louiselm.capture

import org.json.JSONObject
import java.net.URI

internal enum class UploadDisposition {
    SUCCESS,
    RETRY,
    OPERATOR_ACTION,
}

internal enum class PairingTransition {
    FIRST_PAIR,
    ENDPOINT_UPDATE,
    RECEIVER_MIGRATION,
}

internal enum class AttentionKind(val reason: String) {
    TURN_READY("Agent turn is ready"),
    PERMISSION_REQUIRED("Permission is required"),
    RUN_PARKED("Run is Parked"),
    SESSION_FAILED("Session failed"),
    ;

    companion object {
        fun parse(value: String): AttentionKind = when (value) {
            "turn_ready" -> TURN_READY
            "permission_required" -> PERMISSION_REQUIRED
            "run_parked" -> RUN_PARKED
            "session_failed" -> SESSION_FAILED
            else -> throw IllegalArgumentException("attention kind is unsupported")
        }
    }
}

internal enum class AttentionSubjectKind {
    SESSION,
    RUN,
    ;

    companion object {
        fun parse(value: String): AttentionSubjectKind = when (value) {
            "session" -> SESSION
            "run" -> RUN
            else -> throw IllegalArgumentException("attention subject kind is unsupported")
        }
    }
}

internal data class AttentionItem(
    val subjectKind: AttentionSubjectKind,
    val subjectId: String,
    val kind: AttentionKind,
    val sourceOperationId: String,
    val createdAtMs: Long,
    val eligible: Boolean,
    val linkedRunId: String?,
    val stage: String?,
)

internal data class AttentionSnapshot(
    val generation: Long,
    val items: List<AttentionItem>,
) {
    companion object {
        fun parse(payload: String): AttentionSnapshot {
            require(payload.length <= MAX_ATTENTION_RESPONSE) { "attention response is too large" }
            val value = JSONObject(payload)
            requireKeys(value, setOf("generation", "items"), "attention snapshot")
            val generation = value.getLong("generation")
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
                    "kind",
                    "source_operation_id",
                    "created_at_ms",
                    "eligible",
                    "reason",
                    "linked_run_id",
                    "stage",
                ),
                "attention item",
            )
            val subjectKind = AttentionSubjectKind.parse(value.getString("subject_kind"))
            val subjectId = value.getString("subject_id")
            require(validAttentionId(subjectId)) { "attention subject id is invalid" }
            if (subjectKind == AttentionSubjectKind.RUN) {
                require(validCanonicalUuid(subjectId)) { "attention Run id is invalid" }
            }
            val kind = AttentionKind.parse(value.getString("kind"))
            val sourceOperationId = value.getString("source_operation_id")
            require(validCanonicalUuid(sourceOperationId)) { "attention operation id is invalid" }
            val createdAtMs = value.getLong("created_at_ms")
            require(createdAtMs > 0) { "attention creation time is invalid" }
            val reason = value.getString("reason")
            require(reason == kind.reason) { "attention reason is invalid" }
            val linkedRunId = optionalString(value, "linked_run_id")
            if (linkedRunId != null) require(validCanonicalUuid(linkedRunId)) { "attention linked Run id is invalid" }
            val stage = optionalString(value, "stage")
            if (stage != null) {
                require(stage.isNotEmpty() && stage.toByteArray(Charsets.UTF_8).size <= MAX_ATTENTION_STAGE)
                require(stage.all { it.isLetterOrDigit() || it in "._/-" }) { "attention stage is invalid" }
            }
            return AttentionItem(
                subjectKind,
                subjectId,
                kind,
                sourceOperationId,
                createdAtMs,
                value.getBoolean("eligible"),
                linkedRunId,
                stage,
            )
        }

        private fun optionalString(value: JSONObject, key: String): String? {
            if (!value.has(key) || value.isNull(key)) return null
            return value.getString(key)
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
