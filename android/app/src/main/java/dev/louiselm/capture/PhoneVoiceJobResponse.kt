package dev.louiselm.capture

import android.util.JsonReader
import android.util.JsonToken
import java.io.IOException
import java.io.StringReader
import java.nio.ByteBuffer
import java.nio.charset.CodingErrorAction

internal sealed class PhoneVoiceJobResponse {
    data class Accepted(val operation: String, val id: String, val state: String) : PhoneVoiceJobResponse()
    data class Failure(val outcome: PhoneControlOutcome) : PhoneVoiceJobResponse()
}

/** Only closed, bounded replies from the pinned receiver can acknowledge a job. */
internal fun parsePhoneVoiceJobResponse(bytes: ByteArray): PhoneVoiceJobResponse {
    val malformed = PhoneVoiceJobResponse.Failure(PhoneControlOutcome.MALFORMED)
    if (bytes.size > 4096) return malformed
    return try {
        val text = Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT).decode(ByteBuffer.wrap(bytes)).toString()
        val fields = mutableMapOf<String, String>()
        JsonReader(StringReader(text)).use { reader ->
            reader.isLenient = false
            reader.beginObject()
            while (reader.hasNext()) {
                val name = reader.nextName()
                require(name !in fields && name in setOf("v", "type", "operation_id", "id", "state", "code"))
                require(reader.peek() == if (name == "v") JsonToken.NUMBER else JsonToken.STRING)
                fields[name] = reader.nextString()
            }
            reader.endObject()
            require(reader.peek() == JsonToken.END_DOCUMENT)
        }
        require(fields["v"] == "1")
        when (fields["type"]) {
            "voice_job" -> {
                require(fields.keys == setOf("v", "type", "operation_id", "id", "state"))
                val operation = requireNotNull(fields["operation_id"])
                val id = requireNotNull(fields["id"])
                val state = requireNotNull(fields["state"])
                phoneControlHex(operation, 16)
                phoneControlRunIdBytes(id)
                require(state in setOf("pending", "running", "retrying", "completed", "failed", "cancelled"))
                PhoneVoiceJobResponse.Accepted(operation, id, state)
            }
            "error" -> {
                require(fields.keys == setOf("v", "type", "code"))
                val outcome = PhoneControlOutcome.entries.firstOrNull { it.name.lowercase() == fields["code"] }
                    ?: return malformed
                PhoneVoiceJobResponse.Failure(outcome)
            }
            else -> malformed
        }
    } catch (_: IOException) {
        malformed
    } catch (_: IllegalStateException) {
        malformed
    } catch (_: IllegalArgumentException) {
        malformed
    }
}
