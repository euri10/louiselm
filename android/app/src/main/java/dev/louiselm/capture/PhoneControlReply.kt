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
}

private class UnsupportedReply : Exception()

internal fun parsePhoneControlReply(bytes: ByteArray): PhoneControlReply {
    if (bytes.size > 262_144) return PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED)
    return try {
        val decoder = Charsets.UTF_8.newDecoder().onMalformedInput(CodingErrorAction.REPORT)
            .onUnmappableCharacter(CodingErrorAction.REPORT)
        val text = decoder.decode(ByteBuffer.wrap(bytes)).toString()
        val fields = mutableMapOf<String, Any>()
        JsonReader(StringReader(text)).use { reader ->
            reader.isLenient = false
            reader.beginObject()
            while (reader.hasNext()) {
                val name = reader.nextName()
                require(name !in fields) { "duplicate field" }
                val value: Any = when (reader.peek()) {
                    JsonToken.STRING -> reader.nextString().also { require(validWireString(it)) }
                    JsonToken.NUMBER -> reader.nextString().also {
                        require(it.matches(Regex("0|[1-9][0-9]*"))) { "invalid integer" }
                    }.toLong().also { require(it <= 9_007_199_254_740_991L) }
                    else -> throw IllegalArgumentException("invalid field")
                }
                fields[name] = value
            }
            reader.endObject()
            require(reader.peek() == JsonToken.END_DOCUMENT) { "trailing value" }
        }
        fun string(name: String): String = fields[name] as? String ?: throw IllegalArgumentException("invalid $name")
        fun number(name: String): Long = fields[name] as? Long ?: throw IllegalArgumentException("invalid $name")
        fun hex(name: String, size: Int): String = string(name).also { phoneControlHex(it, size) }
        fun keys(vararg names: String) = require(fields.keys == names.toSet() + setOf("v", "type")) { "invalid fields" }
        if (number("v") != 1L) throw UnsupportedReply()
        when (string("type")) {
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

private fun validWireString(value: String): Boolean {
    if (value.toByteArray(Charsets.UTF_8).size > 8192 || '\u0000' in value) return false
    var index = 0
    while (index < value.length) {
        val character = value[index++]
        if (Character.isHighSurrogate(character)) {
            if (index == value.length || !Character.isLowSurrogate(value[index++])) return false
        } else if (Character.isLowSurrogate(character)) return false
    }
    return true
}
