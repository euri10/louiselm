package dev.louiselm.capture

internal enum class PhoneRunState { ADMITTED, ACTIVE, PARKED, COLD_PARKED, RESUMING, DISPOSED }
internal enum class PhoneRunLoad { UNKNOWN, SUPPORTED, UNSUPPORTED }

internal data class PhoneRunSummary(val runId: String, val revision: Long, val state: PhoneRunState) {
    init {
        phoneControlRunIdBytes(runId)
        require(revision in 1L..9_007_199_254_740_991L)
    }
}

internal data class PhoneRunWork(val ceiling: Long, val consumed: Long, val reserved: Long, val remaining: Long)
internal data class PhoneRunView(
    val summary: PhoneRunSummary, val hasSession: Boolean, val loadSession: PhoneRunLoad, val work: PhoneRunWork,
    val pendingMutations: Long, val parkExpiresAtMs: Long, val parkExpired: Boolean,
    val resumePending: Boolean, val resumeDeadlineMs: Long,
)

private fun runState(value: String): PhoneRunState =
    PhoneRunState.entries.firstOrNull { it.name.lowercase() == value } ?: throw UnsupportedReply()

private fun summary(value: Map<String, Any>): PhoneRunSummary =
    PhoneRunSummary(value.wireString("run_id"), value.wireNumber("revision"), runState(value.wireString("state")))

internal fun parseRunPage(fields: Map<String, Any>): PhoneControlReply.RunPage {
    fields.wireKeys("v", "type", "request_id", "items", "next_after")
    val items = fields.wireArray("items").map {
        val row = requireNotNull(it).wireObject()
        row.wireKeys("run_id", "revision", "state")
        summary(row)
    }
    require(items.size <= 1)
    val next = fields.wireString("next_after")
    if (items.isEmpty()) require(next.isEmpty()) else require(next == items.single().runId)
    return PhoneControlReply.RunPage(fields.wireHex("request_id", 16), items, next)
}

internal fun parseRunDetail(fields: Map<String, Any>): PhoneControlReply.RunDetail {
    fields.wireKeys("v", "type", "request_id", "run")
    val body = requireNotNull(fields["run"]).wireObject()
    body.wireKeys("run_id", "revision", "state", "freshness", "controller", "has_session", "load_session", "work",
        "pending_mutations", "park_expires_at_ms", "park_expired", "resume_pending", "resume_deadline_ms")
    val summary = summary(body)
    if (body.wireString("freshness") != "durable_current" || body.wireString("controller") != "not_checked") throw UnsupportedReply()
    val hasSession = body.wireBoolean("has_session")
    val load = PhoneRunLoad.entries.firstOrNull { it.name.lowercase() == body.wireString("load_session") }
        ?: throw UnsupportedReply()
    require(hasSession == (load != PhoneRunLoad.UNKNOWN))
    val value = requireNotNull(body["work"]).wireObject()
    value.wireKeys("ceiling", "consumed", "reserved", "remaining")
    val work = PhoneRunWork(value.wireNumber("ceiling"), value.wireNumber("consumed"),
        value.wireNumber("reserved"), value.wireNumber("remaining"))
    require(work.ceiling > 0 && work.consumed + work.reserved <= work.ceiling &&
        work.remaining == work.ceiling - work.consumed - work.reserved)
    val parkExpires = body.wireNumber("park_expires_at_ms")
    val parkExpired = body.wireBoolean("park_expired")
    require(!parkExpired || parkExpires > 0 && summary.state in setOf(PhoneRunState.PARKED, PhoneRunState.COLD_PARKED))
    val resume = body.wireBoolean("resume_pending")
    val deadline = body.wireNumber("resume_deadline_ms")
    require(resume == (summary.state == PhoneRunState.RESUMING) && if (resume) deadline > 0 else deadline == 0L)
    return PhoneControlReply.RunDetail(fields.wireHex("request_id", 16),
        PhoneRunView(summary, hasSession, load, work, body.wireNumber("pending_mutations"), parkExpires, parkExpired, resume, deadline))
}
