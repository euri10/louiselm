package dev.louiselm.capture

import java.net.InetAddress
import java.net.URI
import java.net.URISyntaxException
import java.net.UnknownHostException
import java.util.UUID

internal data class PhonePreparedSummary(val registrationId: String, val runId: String, val revision: Long) {
    init {
        phoneControlHex(registrationId, 16)
        require(registrationId != "00".repeat(16) && revision in 1L..9_007_199_254_740_991L)
        canonicalRun(runId)
    }
}
internal data class PhonePreparedBead(val id: String, val prompt: String)
internal data class PhonePreparedExecution(val kind: String, val value: Long)
internal data class PhonePreparedBeadScope(val projectDigest: String, val issueIds: List<String>, val maxMutations: Long)
internal data class PhonePreparedProvider(
    val disclosureProfile: String, val name: String, val upstream: String, val addresses: List<String>,
    val models: List<String>, val maxEffort: String, val maxRequests: Long,
)
internal data class PhonePreparedSkillScope(val enabled: Boolean, val agents: List<String>, val allowRun: Boolean, val allowSupply: Boolean)
internal data class PhonePreparedSkills(val mode: String, val viewDigest: String, val generationDigest: String?)
internal data class PhonePreparedDraft(
    val agent: String, val beads: List<PhonePreparedBead>, val execution: PhonePreparedExecution,
    val maxSessions: Long, val verificationPlanDigest: String, val beadScope: PhonePreparedBeadScope,
    val provider: PhonePreparedProvider, val skillRequests: PhonePreparedSkillScope,
    val skills: PhonePreparedSkills, val network: String,
)

private fun Map<String, Any>.digest(name: String): String = wireString(name).also {
    require(it.matches(Regex("sha256:[0-9a-f]{64}"))) { "invalid digest" }
}

private fun canonicalRun(value: String): String = value.also {
    require(UUID.fromString(it).toString() == it) { "invalid Run identity" }
}

private fun compareUtf8(left: String, right: String): Int {
    val a = left.toByteArray(Charsets.UTF_8)
    val b = right.toByteArray(Charsets.UTF_8)
    for (index in 0 until minOf(a.size, b.size)) {
        val difference = (a[index].toInt() and 0xff) - (b[index].toInt() and 0xff)
        if (difference != 0) return difference
    }
    return a.size.compareTo(b.size)
}

private fun Map<String, Any>.strings(name: String, limit: Int, maxBytes: Int): List<String> = wireArray(name).map {
    val value = it as? String ?: throw IllegalArgumentException("invalid list value")
    require(value.toByteArray(Charsets.UTF_8).size <= maxBytes)
    value
}.also { require(it.size <= limit) }

internal fun parsePreparedPage(fields: Map<String, Any>): PhoneControlReply.PreparedPage {
    fields.wireKeys("v", "type", "request_id", "items", "next_after")
    val items = fields.wireArray("items").map { value ->
        val item = requireNotNull(value).wireObject()
        item.wireKeys("registration_id", "run_id", "revision", "state")
        val id = item.wireHex("registration_id", 16)
        val revision = item.wireNumber("revision")
        require(id != "00".repeat(16) && revision > 0 && item.wireString("state") == "prepared")
        PhonePreparedSummary(id, canonicalRun(item.wireString("run_id")), revision)
    }
    require(items.size <= 1)
    val next = fields.wireString("next_after")
    if (items.isEmpty()) require(next.isEmpty()) else require(next == items.single().registrationId)
    return PhoneControlReply.PreparedPage(fields.wireHex("request_id", 16), items, next)
}

internal fun parsePreparedDetail(fields: Map<String, Any>): PhoneControlReply.PreparedDetail {
    fields.wireKeys("v", "type", "request_id", "registration_id", "run_id", "revision", "detail")
    val registration = fields.wireHex("registration_id", 16)
    val revision = fields.wireNumber("revision")
    require(registration != "00".repeat(16) && revision > 0)
    val body = requireNotNull(fields["detail"]).wireObject()
    body.wireKeys("state", "agent", "beads", "execution", "max_sessions", "verification_plan_digest",
        "bead_scope", "provider_requests", "skill_requests", "skills", "network")
    require(body.wireString("state") == "prepared")
    val agent = body.wireString("agent", 512)
    require(agent.isNotEmpty())
    val beads = body.wireArray("beads").map { value ->
        val bead = requireNotNull(value).wireObject()
        bead.wireKeys("id", "prompt")
        PhonePreparedBead(bead.wireString("id", 512), bead.wireString("prompt", 65_536))
    }
    require(beads.size in 3..5 && beads.all { it.id.isNotEmpty() && it.prompt.isNotEmpty() } &&
        beads.map { it.id }.toSet().size == beads.size && beads.sumOf { it.prompt.toByteArray(Charsets.UTF_8).size } <= 65_536)
    val execution = parseExecution(requireNotNull(body["execution"]).wireObject())
    val maxSessions = body.wireNumber("max_sessions")
    require(maxSessions == 2L * beads.size)
    val scope = parseBeadScope(requireNotNull(body["bead_scope"]).wireObject(), beads)
    val provider = parseProvider(requireNotNull(body["provider_requests"]).wireObject())
    val skillScope = parseSkillScope(requireNotNull(body["skill_requests"]).wireObject())
    val skills = parseSkills(requireNotNull(body["skills"]).wireObject())
    val network = body.wireString("network")
    if (network !in setOf("denied", "brokered")) throw UnsupportedReply()
    return PhoneControlReply.PreparedDetail(fields.wireHex("request_id", 16), registration,
        canonicalRun(fields.wireString("run_id")), revision,
        PhonePreparedDraft(agent, beads, execution, maxSessions, body.digest("verification_plan_digest"),
            scope, provider, skillScope, skills, network))
}

private fun parseExecution(value: Map<String, Any>): PhonePreparedExecution {
    val kind = value.wireString("kind")
    val field = when (kind) {
        "duration" -> "duration_ms"
        "stop_by" -> "expires_at_ms"
        else -> throw UnsupportedReply()
    }
    value.wireKeys("kind", field)
    val limit = value.wireNumber(field)
    require(limit > 0)
    return PhonePreparedExecution(kind, limit)
}

private fun parseBeadScope(value: Map<String, Any>, beads: List<PhonePreparedBead>): PhonePreparedBeadScope {
    value.wireKeys("project_digest", "role", "issue_ids", "effects", "max_mutations", "expires_at_ms")
    require(value.wireString("role") == "coordinator" && value.wireNumber("expires_at_ms") == 0L)
    val ids = value.strings("issue_ids", 5, 512)
    require(ids == beads.map { it.id }.sortedWith(::compareUtf8))
    val effects = value.wireArray("effects")
    require(effects.size == 1)
    val effect = requireNotNull(effects.single()).wireObject()
    effect.wireKeys("kind")
    require(effect.wireString("kind") == "comment_add")
    val max = value.wireNumber("max_mutations")
    require(max == beads.size.toLong())
    return PhonePreparedBeadScope(value.digest("project_digest"), ids, max)
}

private fun parseProvider(value: Map<String, Any>): PhonePreparedProvider {
    value.wireKeys("disclosure_profile", "provider", "upstream", "addresses", "models", "max_effort", "max_run_requests", "expires_at_ms")
    val name = value.wireString("provider", 64)
    require(name.isNotEmpty() && name.all { it in 'a'..'z' || it in '0'..'9' || it == '-' })
    val upstream = value.wireString("upstream", 2048)
    val uri = try { URI(upstream) } catch (_: URISyntaxException) { throw IllegalArgumentException("invalid upstream") }
    require(uri.scheme == "https" && uri.host != null && uri.rawUserInfo == null && uri.rawQuery == null &&
        uri.rawFragment == null && uri.rawPath == "/v1/responses" && uri.port != 443 && uri.toASCIIString() == upstream)
    val addresses = value.strings("addresses", 16, 45)
    val parsedAddresses = addresses.map { numericAddress(it) ?: throw IllegalArgumentException("invalid address") }
    require(addresses.isNotEmpty() && parsedAddresses.toSet().size == addresses.size)
    val models = value.strings("models", 16, 128)
    require(models == listOf("gpt-5.6-luna"))
    val effort = value.wireString("max_effort")
    if (effort !in setOf("none", "minimal", "low", "medium", "high")) throw UnsupportedReply()
    val requests = value.wireNumber("max_run_requests")
    require(requests in 1L..10_000L && value.wireNumber("expires_at_ms") == 0L)
    return PhonePreparedProvider(value.digest("disclosure_profile"), name, upstream, addresses, models, effort, requests)
}

// The restricted alphabet and numeric shape prevent a DNS lookup here.
private data class NumericAddress(val ipv6: Boolean, val bytes: List<Byte>)

private fun numericAddress(value: String): NumericAddress? {
    if (value.isEmpty() || value.any { it !in "0123456789abcdef:." }) return null
    val ipv6 = ':' in value
    if (!ipv6) {
        val parts = value.split('.')
        if (parts.size != 4 || parts.any {
                val number = it.toIntOrNull()
                it.isEmpty() || it.length > 3 || (it.length > 1 && it[0] == '0') ||
                    number == null || number !in 0..255
            }) return null
    }
    return try {
        val parsed = InetAddress.getByName(value).address
        // Java collapses IPv4-mapped IPv6; preserve the family used by Rust's IpAddr.
        val bytes = if (ipv6 && parsed.size == 4) ByteArray(16).also {
            it[10] = 0xff.toByte()
            it[11] = 0xff.toByte()
            parsed.copyInto(it, 12)
        } else parsed
        if (bytes.size != (if (ipv6) 16 else 4)) return null
        val unspecified = bytes.all { it == 0.toByte() }
        val first = bytes[0].toInt() and 0xff
        val multicast = if (ipv6) first == 0xff else first in 224..239
        if (unspecified || multicast) null else NumericAddress(ipv6, bytes.toList())
    } catch (_: UnknownHostException) {
        null
    }
}

private fun parseSkillScope(value: Map<String, Any>): PhonePreparedSkillScope {
    value.wireKeys("enabled", "agents", "allow_run", "allow_supply")
    val enabled = value.wireBoolean("enabled")
    val agents = value.strings("agents", 32, 128)
    require(agents == agents.distinct().sorted() && agents.all { it.isNotEmpty() && it.all { c ->
        c in 'a'..'z' || c in 'A'..'Z' || c in '0'..'9' || c == '-' || c == '_'
    } })
    val allowRun = value.wireBoolean("allow_run")
    val allowSupply = value.wireBoolean("allow_supply")
    require(if (enabled) agents.isNotEmpty() else agents.isEmpty() && !allowRun && !allowSupply)
    return PhonePreparedSkillScope(enabled, agents, allowRun, allowSupply)
}

private fun parseSkills(value: Map<String, Any>): PhonePreparedSkills {
    val mode = value.wireString("mode")
    val generation = when (mode) {
        "off" -> { value.wireKeys("mode", "view_digest"); null }
        "enabled" -> { value.wireKeys("mode", "view_digest", "generation_digest"); value.digest("generation_digest") }
        else -> throw UnsupportedReply()
    }
    return PhonePreparedSkills(mode, value.digest("view_digest"), generation)
}
