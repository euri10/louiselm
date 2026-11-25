package dev.louiselm.capture

import android.annotation.SuppressLint
import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import java.io.IOException
import java.security.GeneralSecurityException
import java.security.KeyStore
import java.security.ProviderException
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject

private const val MAX_PENDING_ACTIONS = 4096
private const val MAX_SNAPSHOT_BYTES = 2 * 1024 * 1024
private const val ACTION_KEY_ALIAS = "louiselm-phone-action-pending-v1"
private val ACTION_AAD = ACTION_KEY_ALIAS.toByteArray(Charsets.UTF_8)

internal data class PendingPhoneAction(
    val operationId: String, val receiverIdentity: String, val controllerId: String, val subjectId: String,
    val action: Int, val payloadHash: String, val grantId: String, val grantRevision: Long,
    val periodId: String, val expectedRevision: Long,
)

internal enum class PendingReservation { NEW, EXISTING }
internal enum class PendingPhoneActionFailure { MALFORMED, CONFLICT, UNAVAILABLE, LIMIT }
internal class PendingPhoneActionStoreException(val reason: PendingPhoneActionFailure) : IOException("pending phone action store $reason")

/**
 * Private, encrypted evidence of phone actions awaiting reconciliation.
 *
 * Calls perform Keystore and filesystem work and must run off the main thread.
 * An empty or missing local store never proves that an operation was not sent;
 * the authoritative owner and receiver must still enforce replay and recovery.
 */
internal class PendingPhoneActionStore(
    context: Context,
    private val key: (Boolean) -> SecretKey? = ::pendingActionKey,
) {
    private val preferences = context.applicationContext.getSharedPreferences("phone-action-pending-v1", Context.MODE_PRIVATE)
    private var failed = false

    /** Persist the exact identity before transmission; existing identities must never be resent. */
    fun reserve(action: PendingPhoneAction): PendingReservation = synchronized(preferences) {
        try {
            validate(action)
        } catch (_: IllegalArgumentException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.MALFORMED)
        }
        val actions = load() ?: emptyList()
        val existing = actions.firstOrNull { it.operationId == action.operationId }
        if (existing != null) {
            if (existing != action) throw PendingPhoneActionStoreException(PendingPhoneActionFailure.CONFLICT)
            return@synchronized PendingReservation.EXISTING
        }
        if (actions.size >= MAX_PENDING_ACTIONS) throw PendingPhoneActionStoreException(PendingPhoneActionFailure.LIMIT)
        save(actions + action)
        PendingReservation.NEW
    }

    /** Read retained identities; a missing or damaged initialized store is unavailable. */
    fun pending(): List<PendingPhoneAction> = synchronized(preferences) {
        load() ?: throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
    }

    private fun load(): List<PendingPhoneAction>? {
        if (failed) throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        try {
            val encoded = preferences.getString("snapshot", null)
            val currentKey = key(false)
            if (encoded == null && currentKey == null) return null
            if (encoded == null || currentKey == null || encoded.length > MAX_SNAPSHOT_BYTES * 2) {
                throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
            }
            val encrypted = Base64.decode(encoded, Base64.NO_WRAP)
            require(encrypted.size in 28..MAX_SNAPSHOT_BYTES + 28)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.DECRYPT_MODE, currentKey, GCMParameterSpec(128, encrypted.copyOfRange(0, 12)))
            cipher.updateAAD(ACTION_AAD)
            val plain = cipher.doFinal(encrypted, 12, encrypted.size - 12)
            val root = JSONObject(String(plain, Charsets.UTF_8))
            require(root.keys().asSequence().toSet() == setOf("v", "actions") && root.get("v") == 1)
            val values = root.get("actions") as? JSONArray ?: throw IllegalArgumentException("invalid pending actions")
            require(values.length() <= MAX_PENDING_ACTIONS)
            val actions = (0 until values.length()).map { decode(values.getJSONObject(it)) }
            require(actions.map { it.operationId }.toSet().size == actions.size)
            return actions
        } catch (error: PendingPhoneActionStoreException) {
            throw error
        } catch (_: GeneralSecurityException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        } catch (_: JSONException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        } catch (_: IllegalArgumentException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        } catch (_: ClassCastException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        } catch (_: SecurityException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        } catch (_: ProviderException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        } catch (_: IOException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        }
    }

    @SuppressLint("UseKtx") // The action must not be transmitted until commit confirms persistence.
    private fun save(actions: List<PendingPhoneAction>) {
        try {
            val values = JSONArray()
            actions.forEach { values.put(encode(it)) }
            val plain = JSONObject().put("v", 1).put("actions", values).toString().toByteArray(Charsets.UTF_8)
            if (plain.size > MAX_SNAPSHOT_BYTES) throw PendingPhoneActionStoreException(PendingPhoneActionFailure.LIMIT)
            val currentKey = key(true) ?: throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            cipher.init(Cipher.ENCRYPT_MODE, currentKey)
            cipher.updateAAD(ACTION_AAD)
            require(cipher.iv.size == 12)
            val encrypted = cipher.iv + cipher.doFinal(plain)
            if (!preferences.edit().putString("snapshot", Base64.encodeToString(encrypted, Base64.NO_WRAP)).commit()) {
                failed = true
                throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
            }
        } catch (error: PendingPhoneActionStoreException) {
            throw error
        } catch (_: GeneralSecurityException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        } catch (_: JSONException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        } catch (_: IllegalArgumentException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        } catch (_: SecurityException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        } catch (_: ProviderException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        } catch (_: IOException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
        }
    }
}

private fun validate(action: PendingPhoneAction) {
    for (id in listOf(action.operationId, action.controllerId, action.subjectId, action.grantId, action.periodId)) phoneControlHex(id, 16)
    phoneControlHex(action.receiverIdentity, 32)
    phoneControlHex(action.payloadHash, 32)
    require(action.action in 3..255 && action.grantRevision in 1..9_007_199_254_740_991L &&
        action.expectedRevision in 0..9_007_199_254_740_991L)
}

private fun encode(action: PendingPhoneAction): JSONObject = JSONObject()
    .put("operation_id", action.operationId).put("receiver_identity", action.receiverIdentity)
    .put("controller_id", action.controllerId).put("subject_id", action.subjectId).put("action", action.action)
    .put("payload_hash", action.payloadHash).put("grant_id", action.grantId).put("grant_revision", action.grantRevision)
    .put("period_id", action.periodId).put("expected_revision", action.expectedRevision)

private fun decode(value: JSONObject): PendingPhoneAction {
    require(value.keys().asSequence().toSet() == setOf("operation_id", "receiver_identity", "controller_id", "subject_id",
        "action", "payload_hash", "grant_id", "grant_revision", "period_id", "expected_revision"))
    fun text(name: String) = value.get(name) as? String ?: throw IllegalArgumentException("invalid action identity")
    fun number(name: String): Long = when (val number = value.get(name)) {
        is Int -> number.toLong()
        is Long -> number
        else -> throw IllegalArgumentException("invalid action number")
    }
    val action = number("action")
    require(action in 3..255)
    return PendingPhoneAction(text("operation_id"), text("receiver_identity"), text("controller_id"), text("subject_id"),
        action.toInt(), text("payload_hash"), text("grant_id"), number("grant_revision"), text("period_id"),
        number("expected_revision")).also(::validate)
}

private fun pendingActionKey(create: Boolean): SecretKey? {
    val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    val existing = store.getKey(ACTION_KEY_ALIAS, null)
    if (existing != null) return existing as? SecretKey ?: throw GeneralSecurityException("invalid action key")
    if (!create) return null
    return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
        init(KeyGenParameterSpec.Builder(ACTION_KEY_ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256).build())
        generateKey()
    }
}
