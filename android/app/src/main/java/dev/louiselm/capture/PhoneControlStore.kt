package dev.louiselm.capture

import android.annotation.SuppressLint
import android.content.Context
import java.io.IOException
import java.security.MessageDigest
import org.json.JSONObject

internal data class PhoneControlPending(
    val enrollmentId: String,
    val matchingCode: String,
    val receiverIdentity: String,
    val pairingBinding: String,
    val keyHash: String,
)

internal data class PhoneControlGrant(
    val grantId: String,
    val revision: Long,
    val receiverIdentity: String,
    val pairingBinding: String,
    val keyHash: String,
)

internal fun phoneControlPairingBinding(config: PairingConfig): String = phoneControlHex(
    MessageDigest.getInstance("SHA-256").digest(
        "${config.deviceId}\u0000${config.credential}".toByteArray(Charsets.UTF_8),
    ),
)

internal class PhoneControlStore(context: Context) {
    private val preferences = context.applicationContext.getSharedPreferences("phone-control-v1", Context.MODE_PRIVATE)

    fun pending(): PhoneControlPending? {
        val json = preferences.getString("pending", null) ?: return null
        val value = JSONObject(json)
        require(value.keys().asSequence().toSet() == setOf("id", "code", "receiver", "pairing", "key"))
        return PhoneControlPending(
            value.text("id"), value.text("code"), value.text("receiver"),
            value.text("pairing"), value.text("key"),
        ).also {
            phoneControlHex(it.enrollmentId, 16)
            phoneControlHex(it.receiverIdentity, 32)
            phoneControlHex(it.pairingBinding, 32)
            phoneControlHex(it.keyHash, 32)
            require(it.matchingCode.length == 8 && it.matchingCode.all { digit -> digit in '0'..'9' })
        }
    }

    fun grant(): PhoneControlGrant? {
        val json = preferences.getString("grant", null) ?: return null
        val value = JSONObject(json)
        require(value.keys().asSequence().toSet() == setOf("id", "revision", "receiver", "pairing", "key"))
        val revision = when (val number = value.get("revision")) {
            is Int -> number.toLong()
            is Long -> number
            else -> throw IllegalArgumentException("invalid grant revision")
        }
        return PhoneControlGrant(
            value.text("id"), revision, value.text("receiver"),
            value.text("pairing"), value.text("key"),
        ).also {
            phoneControlHex(it.grantId, 16)
            phoneControlHex(it.receiverIdentity, 32)
            phoneControlHex(it.pairingBinding, 32)
            phoneControlHex(it.keyHash, 32)
            require(it.revision in 1..9_007_199_254_740_991L)
        }
    }

    @SuppressLint("UseKtx") // A pending submission must survive process death before it is sent.
    fun savePending(pending: PhoneControlPending) {
        val json = JSONObject().put("id", pending.enrollmentId).put("code", pending.matchingCode)
            .put("receiver", pending.receiverIdentity).put("pairing", pending.pairingBinding).put("key", pending.keyHash)
        if (!preferences.edit().putString("pending", json.toString()).commit()) throw IOException("enrollment state could not be stored")
    }

    @SuppressLint("UseKtx") // Publish the approved grant and remove its pending record together.
    fun saveGrant(grant: PhoneControlGrant) {
        val json = JSONObject().put("id", grant.grantId).put("revision", grant.revision)
            .put("receiver", grant.receiverIdentity).put("pairing", grant.pairingBinding).put("key", grant.keyHash)
        if (!preferences.edit().putString("grant", json.toString()).remove("pending").commit()) {
            throw IOException("grant could not be stored")
        }
    }

    @SuppressLint("UseKtx") // Do not report locally forgotten control until the durable write succeeds.
    fun clear() {
        if (!preferences.edit().clear().commit()) throw IOException("phone-control state could not be cleared")
    }
}

private fun JSONObject.text(name: String): String =
    get(name) as? String ?: throw IllegalArgumentException("invalid stored phone-control field")
