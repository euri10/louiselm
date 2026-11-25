package dev.louiselm.capture

import android.util.JsonReader
import android.util.JsonToken
import java.io.IOException
import java.io.StringReader
import java.nio.ByteBuffer
import java.nio.charset.CharacterCodingException
import java.nio.charset.CodingErrorAction

internal enum class PhoneControlOutcome {
    UNAUTHENTICATED, EXPIRED, REVOKED, OUT_OF_SCOPE, STALE_SUBJECT, UNAVAILABLE, MALFORMED, UNSUPPORTED,
}

internal sealed class PhoneControlReply {
    data class Failure(val outcome: PhoneControlOutcome) : PhoneControlReply()
    data class Offer(val enrollmentId: String, val matchingCode: String, val receiverIdentity: String, val expiresAtMs: Long) : PhoneControlReply()
    data class Pending(val enrollmentId: String) : PhoneControlReply()
    data class Granted(val enrollmentId: String, val grantId: String, val revision: Long, val keyHash: String, val receiverIdentity: String) : PhoneControlReply()
    data class Challenge(val grantId: String, val revision: Long, val receiverIdentity: String, val challengeId: String, val challenge: String, val expiresAtMs: Long) : PhoneControlReply()
    data class Unlocked(val grantId: String, val revision: Long, val periodId: String, val idleExpiresAtMs: Long, val absoluteExpiresAtMs: Long) : PhoneControlReply()
    data class SessionPage(
        val controllerId: String, val subjectId: String, val operationId: String,
        val revision: Long, val status: String, val entries: List<PhoneControlEntry>,
        val nextBefore: Long, val complete: Boolean, val source: PhoneHistorySource,
    ) : PhoneControlReply()
    data class ControllerList(val operationId: String, val controllers: List<String>, val nextAfter: String) : PhoneControlReply()
    data class SessionList(val controllerId: String, val operationId: String,
        val sessions: List<PhoneControlSessionSummary>, val nextAfter: String) : PhoneControlReply()
    data class PreparedPage(val requestId: String, val items: List<PhonePreparedSummary>, val nextAfter: String) : PhoneControlReply()
    data class PreparedDetail(val requestId: String, val registrationId: String, val runId: String,
        val revision: Long, val detail: PhonePreparedDraft) : PhoneControlReply()
    data class RunPage(val requestId: String, val items: List<PhoneRunSummary>, val nextAfter: String) : PhoneControlReply()
    data class RunDetail(val requestId: String, val run: PhoneRunView) : PhoneControlReply()
    data class ActionHistory(val requestId: String, val record: PhoneActionRecord) : PhoneControlReply()
    data class ActionHistoryPage(val requestId: String, val records: List<PhoneActionRecord>, val nextAfter: String) : PhoneControlReply()
}

internal data class PhoneControlSessionSummary(
    val subjectId: String, val name: String, val agent: String, val project: String,
    val status: String, val truncated: Boolean,
)

internal enum class PhoneActionState { UNKNOWN, ACCEPTED, DISPATCHED, COMPLETED, REJECTED, FENCED_NO_EFFECT }

internal data class PhoneActionRecord(
    val operationId: String, val receiverIdentity: String, val controllerId: String, val subjectId: String,
    val action: Int, val payloadHash: String, val grantId: String, val grantRevision: Long,
    val expectedRevision: Long, val recordedAtMs: Long, val updatedAtMs: Long,
    val state: PhoneActionState, val evidence: String,
    val acceptedEvidence: String, val dispatchedEvidence: String, val completedEvidence: String,
)

internal data class PhoneControlOption(val label: String, val description: String, val preview: String)

internal data class PhoneHistorySource(
    val replay: String, val archive: String, val redaction: String, val missing: Boolean, val truncated: Boolean,
)

internal sealed class PhoneControlEntry(open val seq: Long, open val truncated: Boolean) {
    data class Message(override val seq: Long, val role: String, val text: String, override val truncated: Boolean) : PhoneControlEntry(seq, truncated)
    data class Tool(override val seq: Long, val id: String, val title: String, val status: String, val content: String, override val truncated: Boolean) : PhoneControlEntry(seq, truncated)
    data class Question(override val seq: Long, val requestId: String, val question: String, val options: List<PhoneControlOption>, val multiSelect: Boolean, override val truncated: Boolean) : PhoneControlEntry(seq, truncated)
    data class Plan(override val seq: Long, val requestId: String, val text: String, override val truncated: Boolean) : PhoneControlEntry(seq, truncated)
}

internal class UnsupportedReply : Exception()

private fun readWireValue(reader: JsonReader, depth: Int): Any {
    require(depth <= 5) { "nested wire value" }
    return when (reader.peek()) {
        JsonToken.BEGIN_OBJECT -> {
            val fields = mutableMapOf<String, Any>()
            reader.beginObject()
            while (reader.hasNext()) {
                val name = reader.nextName()
                require(name !in fields && fields.size < 32) { "duplicate or excessive fields" }
                fields[name] = readWireValue(reader, depth + 1)
            }
            reader.endObject()
            fields
        }
        JsonToken.BEGIN_ARRAY -> {
            val values = mutableListOf<Any>()
            reader.beginArray()
            while (reader.hasNext()) {
                require(values.size < 50) { "excessive array" }
                values.add(readWireValue(reader, depth + 1))
            }
            reader.endArray()
            values
        }
        JsonToken.STRING -> reader.nextString().also { require(validWireString(it, 65_536)) }
        JsonToken.NUMBER -> reader.nextString().also {
            require(it.matches(Regex("0|[1-9][0-9]*"))) { "invalid integer" }
        }.toLong().also { require(it <= 9_007_199_254_740_991L) }
        JsonToken.BOOLEAN -> reader.nextBoolean()
        else -> throw IllegalArgumentException("invalid field")
    }
}

internal fun Any.wireObject(): Map<String, Any> {
    require(this is Map<*, *> && keys.all { it is String } && values.all { it != null })
    @Suppress("UNCHECKED_CAST") // readWireValue constructs only string-keyed maps with non-null values.
    return this as Map<String, Any>
}

internal fun Map<String, Any>.wireString(name: String, maxBytes: Int = 8192): String =
    (this[name] as? String ?: throw IllegalArgumentException("invalid $name")).also { require(validWireString(it, maxBytes)) }
internal fun Map<String, Any>.wireNumber(name: String): Long = this[name] as? Long ?: throw IllegalArgumentException("invalid $name")
internal fun Map<String, Any>.wireBoolean(name: String): Boolean = this[name] as? Boolean ?: throw IllegalArgumentException("invalid $name")
internal fun Map<String, Any>.wireArray(name: String): List<*> = this[name] as? List<*> ?: throw IllegalArgumentException("invalid $name")
internal fun Map<String, Any>.wireKeys(vararg names: String) = require(keys == names.toSet()) { "invalid fields" }
internal fun Map<String, Any>.wireHex(name: String, size: Int): String = wireString(name).also { phoneControlHex(it, size) }

private fun parseActionRecord(value: Any): PhoneActionRecord {
    val record = value.wireObject()
    record.wireKeys("operation_id", "receiver_identity", "controller_id", "subject_id", "action", "payload_hash",
        "grant_id", "grant_revision", "expected_revision", "recorded_at_ms", "updated_at_ms", "state", "evidence",
        "accepted_evidence", "dispatched_evidence", "completed_evidence")
    val state = PhoneActionState.entries.firstOrNull { it.name.lowercase() == record.wireString("state") }
        ?: throw UnsupportedReply()
    val evidence = record.wireString("evidence")
    if (state == PhoneActionState.UNKNOWN) require(evidence.isEmpty()) else phoneControlHex(evidence, 32)
    val accepted = record.wireString("accepted_evidence")
    val dispatched = record.wireString("dispatched_evidence")
    val completed = record.wireString("completed_evidence")
    listOf(accepted, dispatched, completed).filter { it.isNotEmpty() }.forEach { phoneControlHex(it, 32) }
    when (state) {
        PhoneActionState.UNKNOWN -> require(completed.isEmpty())
        PhoneActionState.ACCEPTED -> require(evidence == accepted && dispatched.isEmpty() && completed.isEmpty())
        PhoneActionState.DISPATCHED -> require(evidence == dispatched && completed.isEmpty())
        PhoneActionState.COMPLETED -> require(evidence == completed)
        PhoneActionState.REJECTED, PhoneActionState.FENCED_NO_EFFECT ->
            require(accepted.isEmpty() && dispatched.isEmpty() && completed.isEmpty())
    }
    val action = record.wireNumber("action")
    val grantRevision = record.wireNumber("grant_revision")
    val recorded = record.wireNumber("recorded_at_ms")
    val updated = record.wireNumber("updated_at_ms")
    require(action in 3..255 && grantRevision > 0 && updated >= recorded)
    return PhoneActionRecord(record.wireHex("operation_id", 16), record.wireHex("receiver_identity", 32),
        record.wireHex("controller_id", 16), record.wireHex("subject_id", 16), action.toInt(),
        record.wireHex("payload_hash", 32), record.wireHex("grant_id", 16), grantRevision,
        record.wireNumber("expected_revision"), recorded, updated, state, evidence, accepted, dispatched, completed)
}

private fun parseEntry(value: Any): PhoneControlEntry {
    val entry = value.wireObject()
    val seq = entry.wireNumber("seq")
    require(seq > 0)
    val truncated = entry.wireBoolean("truncated")
    return when (entry.wireString("kind")) {
        "message" -> {
            entry.wireKeys("kind", "seq", "role", "text", "truncated")
            val role = entry.wireString("role")
            if (role !in setOf("user", "assistant")) throw UnsupportedReply()
            PhoneControlEntry.Message(seq, role, entry.wireString("text"), truncated)
        }
        "tool" -> {
            entry.wireKeys("kind", "seq", "id", "title", "status", "content", "truncated")
            val status = entry.wireString("status")
            if (status !in setOf("pending", "in_progress", "completed", "failed", "unknown")) throw UnsupportedReply()
            PhoneControlEntry.Tool(seq, entry.wireString("id"), entry.wireString("title"), status, entry.wireString("content"), truncated)
        }
        "question" -> {
            entry.wireKeys("kind", "seq", "request_id", "question", "options", "multi_select", "truncated")
            val options = entry.wireArray("options").map { item ->
                val option = requireNotNull(item).wireObject()
                option.wireKeys("label", "description", "preview")
                PhoneControlOption(option.wireString("label"), option.wireString("description"), option.wireString("preview"))
            }
            PhoneControlEntry.Question(seq, entry.wireString("request_id"), entry.wireString("question"), options, entry.wireBoolean("multi_select"), truncated)
        }
        "plan" -> {
            entry.wireKeys("kind", "seq", "request_id", "text", "truncated")
            PhoneControlEntry.Plan(seq, entry.wireString("request_id"), entry.wireString("text"), truncated)
        }
        else -> throw UnsupportedReply()
    }
}

internal fun parsePhoneControlReply(bytes: ByteArray): PhoneControlReply {
    if (bytes.size > 262_144) return PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED)
    return try {
        val decoder = Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT)
        val text = decoder.decode(ByteBuffer.wrap(bytes)).toString()
        val fields = JsonReader(StringReader(text)).use { reader ->
            reader.isLenient = false
            val value = readWireValue(reader, 0).wireObject()
            require(reader.peek() == JsonToken.END_DOCUMENT) { "trailing value" }
            value
        }
        fun string(name: String): String = fields.wireString(name)
        fun number(name: String): Long = fields[name] as? Long ?: throw IllegalArgumentException("invalid $name")
        fun hex(name: String, size: Int): String = string(name).also { phoneControlHex(it, size) }
        fun keys(vararg names: String) = require(fields.keys == names.toSet() + setOf("v", "type")) { "invalid fields" }
        val type = string("type")
        if (number("v") != if (type in setOf("session_page", "action_history", "action_history_page")) 2L else 1L) throw UnsupportedReply()
        when (type) {
            "error" -> {
                keys("code")
                val code = PhoneControlOutcome.entries.firstOrNull { it.name.lowercase() == string("code") }
                    ?: throw UnsupportedReply()
                PhoneControlReply.Failure(code)
            }
            "enrollment_offer" -> {
                keys("enrollment_id", "matching_code", "receiver_identity", "expires_at_ms")
                val code = string("matching_code")
                require(code.length == 8 && code.all { it in '0'..'9' })
                PhoneControlReply.Offer(hex("enrollment_id", 16), code, hex("receiver_identity", 32), number("expires_at_ms"))
            }
            "enrollment_pending" -> {
                keys("enrollment_id")
                PhoneControlReply.Pending(hex("enrollment_id", 16))
            }
            "enrollment_granted" -> {
                keys("enrollment_id", "grant_id", "grant_revision", "enrolled_key_hash", "receiver_identity")
                val revision = number("grant_revision")
                require(revision > 0)
                PhoneControlReply.Granted(hex("enrollment_id", 16), hex("grant_id", 16), revision,
                    hex("enrolled_key_hash", 32), hex("receiver_identity", 32))
            }
            "challenge" -> {
                keys("grant_id", "grant_revision", "receiver_identity", "challenge_id", "challenge", "expires_at_ms")
                val revision = number("grant_revision")
                require(revision > 0)
                PhoneControlReply.Challenge(hex("grant_id", 16), revision, hex("receiver_identity", 32),
                    hex("challenge_id", 16), hex("challenge", 32), number("expires_at_ms"))
            }
            "unlocked" -> {
                keys("grant_id", "grant_revision", "period_id", "idle_expires_at_ms", "absolute_expires_at_ms")
                val revision = number("grant_revision")
                require(revision > 0)
                val idle = number("idle_expires_at_ms")
                val absolute = number("absolute_expires_at_ms")
                require(idle <= absolute)
                PhoneControlReply.Unlocked(hex("grant_id", 16), revision, hex("period_id", 16), idle, absolute)
            }
            "session_page" -> {
                keys("controller_id", "subject_id", "operation_id", "revision", "status", "entries", "next_before", "complete", "source")
                val revision = number("revision")
                require(revision > 0)
                val status = string("status")
                if (status !in setOf("starting", "ready", "admitting", "configuring", "preparing", "prompting", "running", "waiting_permission", "cancelling", "error")) throw UnsupportedReply()
                val entries = fields.wireArray("entries").map { parseEntry(requireNotNull(it)) }
                require(entries.zipWithNext().all { (left, right) -> left.seq < right.seq })
                val nextBefore = number("next_before")
                require(nextBefore == 0L || entries.isEmpty() || nextBefore <= entries.first().seq)
                val source = requireNotNull(fields["source"]).wireObject()
                source.wireKeys("replay", "archive", "redaction", "missing", "truncated")
                val replay = source.wireString("replay")
                val archive = source.wireString("archive")
                val redaction = source.wireString("redaction")
                if (replay !in setOf("unverified", "not_applicable") || archive != "unavailable" ||
                    redaction !in setOf("present", "unknown")) throw UnsupportedReply()
                val details = PhoneHistorySource(replay, archive, redaction, source.wireBoolean("missing"), source.wireBoolean("truncated"))
                require(nextBefore == 0L || details.missing || entries.firstOrNull()?.seq == nextBefore)
                val complete = fields.wireBoolean("complete")
                require(!complete || replay == "not_applicable" && redaction == "unknown" && !details.missing && !details.truncated)
                PhoneControlReply.SessionPage(hex("controller_id", 16), hex("subject_id", 16), hex("operation_id", 16), revision,
                    status, entries, nextBefore, complete, details)
            }
            "controller_list" -> {
                keys("operation_id", "controllers", "next_after")
                val controllers = fields.wireArray("controllers").map { item ->
                    (item as? String ?: throw IllegalArgumentException("invalid controller")).also {
                        phoneControlHex(it, 16)
                        require(it != "00".repeat(16))
                    }
                }
                require(controllers.zipWithNext().all { (left, right) -> left < right })
                val next = string("next_after")
                if (next.isNotEmpty()) {
                    phoneControlHex(next, 16)
                    require(controllers.lastOrNull() == next)
                }
                PhoneControlReply.ControllerList(hex("operation_id", 16), controllers, next)
            }
            "session_list" -> {
                keys("controller_id", "operation_id", "sessions", "next_after")
                val controller = hex("controller_id", 16)
                require(controller != "00".repeat(16))
                val sessions = fields.wireArray("sessions").map { item ->
                    val row = requireNotNull(item).wireObject()
                    row.wireKeys("subject_id", "name", "agent", "project", "status", "truncated")
                    val subject = row.wireString("subject_id")
                    phoneControlHex(subject, 16)
                    require(subject != "00".repeat(16))
                    val name = row.wireString("name")
                    val agent = row.wireString("agent")
                    val project = row.wireString("project")
                    require(listOf(name, agent, project).all { it.toByteArray(Charsets.UTF_8).size <= 512 })
                    val status = row.wireString("status")
                    if (status !in setOf("starting", "ready", "admitting", "configuring", "preparing", "prompting", "running", "waiting_permission", "cancelling", "error")) throw UnsupportedReply()
                    PhoneControlSessionSummary(subject, name, agent, project, status, row.wireBoolean("truncated"))
                }
                require(sessions.zipWithNext().all { (left, right) -> left.subjectId < right.subjectId })
                val next = string("next_after")
                if (next.isNotEmpty()) {
                    phoneControlHex(next, 16)
                    require(sessions.lastOrNull()?.subjectId == next)
                }
                PhoneControlReply.SessionList(controller, hex("operation_id", 16), sessions, next)
            }
            "prepared_page" -> parsePreparedPage(fields)
            "prepared_detail" -> parsePreparedDetail(fields)
            "run_page" -> parseRunPage(fields)
            "run_detail" -> parseRunDetail(fields)
            "action_history" -> {
                keys("request_id", "record")
                PhoneControlReply.ActionHistory(hex("request_id", 16), parseActionRecord(requireNotNull(fields["record"])))
            }
            "action_history_page" -> {
                keys("request_id", "records", "next_after")
                val records = fields.wireArray("records").map { parseActionRecord(requireNotNull(it)) }
                require(records.zipWithNext().all { (left, right) -> left.operationId < right.operationId })
                val next = string("next_after")
                if (next.isNotEmpty()) {
                    phoneControlHex(next, 16)
                    require(records.isNotEmpty() && next == records.last().operationId)
                }
                PhoneControlReply.ActionHistoryPage(hex("request_id", 16), records, next)
            }
            else -> throw UnsupportedReply()
        }
    } catch (_: UnsupportedReply) {
        PhoneControlReply.Failure(PhoneControlOutcome.UNSUPPORTED)
    } catch (_: CharacterCodingException) {
        PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED)
    } catch (_: IOException) {
        PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED)
    } catch (_: IllegalArgumentException) {
        PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED)
    } catch (_: IllegalStateException) {
        PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED)
    }
}

private fun validWireString(value: String, maxBytes: Int): Boolean {
    if (value.toByteArray(Charsets.UTF_8).size > maxBytes || '\u0000' in value) return false
    var index = 0
    while (index < value.length) {
        val character = value[index++]
        if (Character.isHighSurrogate(character)) {
            if (index == value.length || !Character.isLowSurrogate(value[index++])) return false
        } else if (Character.isLowSurrogate(character)) return false
    }
    return true
}
