package dev.louiselm.capture

import java.nio.ByteBuffer

internal enum class PhoneSavedSide { BEFORE, AFTER }
internal enum class PhoneSavedChange { ADDED, MODIFIED, DELETED }
internal enum class PhoneSavedVerification { MATCHING, CHANGED, QUARANTINED, UNAVAILABLE }
internal enum class PhoneSavedProvenance { UNTAINTED, SESSION_OUTPUT_TAINTED, UNKNOWN }

internal data class PhoneSavedCursor(val file: Int, val side: PhoneSavedSide, val offset: Int)

internal fun savedCursorAdvances(previous: PhoneSavedCursor, next: PhoneSavedCursor): Boolean =
    next.file > previous.file || next.file == previous.file &&
        (next.side.ordinal > previous.side.ordinal || next.side == previous.side && next.offset > previous.offset)

internal data class PhoneSavedFile(
    val path: String, val change: PhoneSavedChange, val side: PhoneSavedSide, val offset: Int,
    val text: String, val binary: Boolean, val executable: Boolean,
)
internal data class PhoneSavedReview(
    val runId: String, val resultId: String, val revision: String, val producer: String, val agents: List<String>,
    val destination: String, val baseCommit: String, val verificationDigest: String, val planDigest: String,
    val verifierInputsDigest: String, val resultDigest: String, val commandsPassed: Long,
    val verification: PhoneSavedVerification, val provenance: PhoneSavedProvenance,
    val file: PhoneSavedFile?, val next: PhoneSavedCursor?,
)

internal fun validSavedResultId(value: String): Boolean = value.length in 1..128 &&
    value.all { it in 'A'..'Z' || it in 'a'..'z' || it in '0'..'9' || it == '_' || it == '-' }

internal fun phoneSavedReviewPayload(resultId: String, revision: String?, cursor: PhoneSavedCursor?): ByteArray {
    require(validSavedResultId(resultId) && (revision == null) == (cursor == null)) { "invalid saved review target" }
    val version = revision?.let { phoneControlHex(it, 32).also { bytes -> require(bytes.any { it != 0.toByte() }) } }
        ?: ByteArray(32)
    val position = cursor ?: PhoneSavedCursor(0, PhoneSavedSide.BEFORE, 0)
    require(position.file in 0..10_000 && position.offset in 0..128 * 1024 * 1024) { "invalid saved review cursor" }
    val name = resultId.toByteArray(Charsets.US_ASCII)
    return byteArrayOf(1, name.size.toByte()) + name + version + ByteBuffer.allocate(9)
        .putInt(position.file).put(position.side.ordinal.toByte()).putInt(position.offset).array()
}

internal fun validateSavedReviewPayload(payload: ByteArray) {
    require(payload.size in 44..171 && payload[0].toInt() == 1) { "invalid saved review payload" }
    val nameSize = payload[1].toInt() and 0xff
    require(nameSize in 1..128 && payload.size == nameSize + 43) { "invalid saved result length" }
    val nameBytes = payload.copyOfRange(2, 2 + nameSize)
    val name = nameBytes.toString(Charsets.US_ASCII)
    require(validSavedResultId(name) && name.toByteArray(Charsets.US_ASCII).contentEquals(nameBytes))
    val revision = payload.copyOfRange(2 + nameSize, 34 + nameSize)
    val cursor = ByteBuffer.wrap(payload, 34 + nameSize, 9)
    val file = cursor.int
    val side = cursor.get().toInt()
    val offset = cursor.int
    require(file in 0..10_000 && side in 0..1 && offset in 0..128 * 1024 * 1024)
    require(revision.any { it != 0.toByte() } || file == 0 && side == 0 && offset == 0)
}

private inline fun <reified T : Enum<T>> savedEnum(value: String): T =
    enumValues<T>().firstOrNull { it.name.lowercase() == value } ?: throw UnsupportedReply()

internal fun parseSavedReview(fields: Map<String, Any>): PhoneControlReply.SavedReview {
    fields.wireKeys("v", "type", "request_id", "run_id", "result_id", "revision", "producer_agent", "agents",
        "destination", "base_commit", "verification_digest", "plan_digest", "verifier_inputs_digest", "result_digest",
        "commands_passed", "current_verification", "current_provenance", "file", "next")
    val run = fields.wireString("run_id").also { phoneControlRunIdBytes(it) }
    val result = fields.wireString("result_id").also { require(validSavedResultId(it)) }
    val revision = fields.wireHex("revision", 32).also { require(it != "00".repeat(32)) }
    val producer = fields.wireString("producer_agent", 512).also { require(it.isNotEmpty()) }
    val agents = fields.wireArray("agents").map {
        (it as? String ?: throw IllegalArgumentException("invalid Agent")).also { agent ->
            require(agent.isNotEmpty() && agent.toByteArray(Charsets.UTF_8).size <= 512)
        }
    }
    require(agents.isNotEmpty() && agents.distinct().size == agents.size && producer in agents)
    val destination = fields.wireString("destination").also { require(it.startsWith('/')) }
    val base = fields.wireString("base_commit").also {
        require(it.length == 40 || it.length == 64)
        phoneControlHex(it)
    }
    fun digest(name: String): String = fields.wireString(name).also {
        require(it.startsWith("sha256:"))
        phoneControlHex(it.removePrefix("sha256:"), 32)
    }
    val verificationDigest = digest("verification_digest")
    val plan = digest("plan_digest")
    val inputs = digest("verifier_inputs_digest")
    val resultDigest = digest("result_digest")
    val commands = fields.wireNumber("commands_passed").also { require(it in 0L..32L) }
    val verification = savedEnum<PhoneSavedVerification>(fields.wireString("current_verification"))
    val provenance = savedEnum<PhoneSavedProvenance>(fields.wireString("current_provenance"))
    val rawFile = fields["file"] ?: throw IllegalArgumentException("missing review file")
    val file = if (rawFile === PhoneWireNull) null else {
        val body = rawFile.wireObject()
        body.wireKeys("path", "change", "side", "offset", "text", "binary", "executable")
        val path = body.wireString("path", 1025).also { require(it.isNotEmpty() &&
            it.split('/').all { component -> component.isNotEmpty() && component != "." && component != ".." }) }
        val offset = body.wireNumber("offset").also { require(it <= 128 * 1024 * 1024) }.toInt()
        val text = body.wireString("text", 4096)
        val binary = body.wireBoolean("binary")
        require(!binary || text.isEmpty())
        PhoneSavedFile(path, savedEnum(body.wireString("change")), savedEnum(body.wireString("side")),
            offset, text, binary, body.wireBoolean("executable"))
    }
    val rawNext = fields["next"] ?: throw IllegalArgumentException("missing review cursor")
    val next = if (rawNext === PhoneWireNull) null else {
        val body = rawNext.wireObject()
        body.wireKeys("file", "side", "offset")
        val fileIndex = body.wireNumber("file").also { require(it <= 10_000) }.toInt()
        val offset = body.wireNumber("offset").also { require(it <= 128 * 1024 * 1024) }.toInt()
        require(file != null)
        PhoneSavedCursor(fileIndex, savedEnum(body.wireString("side")), offset)
    }
    return PhoneControlReply.SavedReview(fields.wireHex("request_id", 16), PhoneSavedReview(run, result, revision,
        producer, agents, destination, base, verificationDigest, plan, inputs, resultDigest, commands,
        verification, provenance, file, next))
}
