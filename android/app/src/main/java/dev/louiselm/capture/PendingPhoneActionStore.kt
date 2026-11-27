package dev.louiselm.capture

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.ByteArrayOutputStream
import java.io.File
import java.io.FileDescriptor
import java.io.FileOutputStream
import java.io.IOException
import java.nio.channels.FileChannel
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.StandardCopyOption
import java.nio.file.StandardOpenOption
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
    val periodId: String, val expectedRevision: Long, val draft: PhoneSubmittedDraft? = null,
)

internal data class PhoneSubmittedDraft(val id: String, val revision: String)

internal enum class PendingReservation { NEW, EXISTING }
internal enum class PendingPhoneActionFailure { MALFORMED, CONFLICT, UNAVAILABLE, LIMIT }
internal class PendingPhoneActionStoreException(val reason: PendingPhoneActionFailure, cause: Throwable? = null) :
    IOException("pending phone action store $reason", cause)

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
    private val syncFile: (FileDescriptor) -> Unit = { it.sync() },
    private val syncDirectory: (File) -> Unit = ::syncPendingDirectory,
) {
    private val directory = context.applicationContext.noBackupFilesDir
    private val snapshot = File(directory, "phone-action-pending-v1")
    // The application runs in one process; Android shares this monitor across store instances.
    private val lock = context.applicationContext.getSharedPreferences("phone-action-pending-v1", Context.MODE_PRIVATE)

    /**
     * Persist the exact Send request before transmission. NEW is evidence, not authority;
     * EXISTING must never be resent. Refusals throw [PendingPhoneActionStoreException].
     */
    fun reserve(request: PhoneControlRequest, draft: PhoneSubmittedDraft? = null): PendingReservation = synchronized(lock) {
        val action = try {
            require(request.action == 3)
            PendingPhoneAction(request.operationId, request.receiverIdentity, request.controllerId, request.subjectId,
                request.action, request.payloadHash, request.grantId, request.grantRevision, request.periodId,
                request.expectedRevision, draft).also(::validate)
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
    fun pending(): List<PendingPhoneAction> = synchronized(lock) {
        load() ?: throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
    }

    private fun load(): List<PendingPhoneAction>? {
        try {
            val currentKey = key(false)
            if (!snapshot.exists() && currentKey == null) return null
            if (!snapshot.isFile || currentKey == null) {
                throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE)
            }
            val encrypted = snapshot.inputStream().use { input ->
                val output = ByteArrayOutputStream()
                val buffer = ByteArray(8192)
                while (true) {
                    val count = input.read(buffer)
                    if (count < 0) break
                    require(output.size() + count <= MAX_SNAPSHOT_BYTES + 28)
                    output.write(buffer, 0, count)
                }
                output.toByteArray()
            }
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
            // A prior rename may have succeeded even if directory synchronization failed.
            syncDirectory(directory)
            return actions
        } catch (error: PendingPhoneActionStoreException) {
            throw error
        } catch (error: GeneralSecurityException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        } catch (error: JSONException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        } catch (error: IllegalArgumentException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        } catch (error: ClassCastException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        } catch (error: SecurityException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        } catch (error: ProviderException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        } catch (error: IOException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        }
    }

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
            val temporary = File(directory, ".phone-action-pending-v1.tmp")
            val temporaryPath = temporary.toPath()
            // Only the renamed snapshot is evidence; an interrupted write leaves this one staging slot.
            if (Files.exists(temporaryPath, LinkOption.NOFOLLOW_LINKS)) {
                if (!Files.isRegularFile(temporaryPath, LinkOption.NOFOLLOW_LINKS)) throw IOException("invalid pending action staging file")
                Files.delete(temporaryPath)
            }
            Files.createFile(temporaryPath)
            try {
                FileOutputStream(temporary).use { output ->
                    output.write(encrypted)
                    syncFile(output.fd)
                }
                Files.move(temporaryPath, snapshot.toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
                syncDirectory(directory)
            } finally {
                Files.deleteIfExists(temporaryPath)
            }
        } catch (error: PendingPhoneActionStoreException) {
            throw error
        } catch (error: GeneralSecurityException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        } catch (error: JSONException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        } catch (error: IllegalArgumentException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        } catch (error: SecurityException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        } catch (error: ProviderException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        } catch (error: IOException) {
            throw PendingPhoneActionStoreException(PendingPhoneActionFailure.UNAVAILABLE, error)
        }
    }
}

internal fun syncPendingDirectory(directory: File) {
    val parent = directory.parentFile ?: throw IOException("pending action directory has no parent")
    for (path in listOf(directory, parent)) {
        FileChannel.open(path.toPath(), StandardOpenOption.READ).use { it.force(true) }
    }
}

private fun validate(action: PendingPhoneAction) {
    for (id in listOf(action.operationId, action.controllerId, action.subjectId, action.grantId, action.periodId)) phoneControlHex(id, 16)
    phoneControlHex(action.receiverIdentity, 32)
    phoneControlHex(action.payloadHash, 32)
    action.draft?.let { phoneControlHex(it.id, 32); phoneControlHex(it.revision, 32) }
    require(action.action in 3..255 && action.grantRevision in 1..9_007_199_254_740_991L &&
        action.expectedRevision in 0..9_007_199_254_740_991L)
}

private fun encode(action: PendingPhoneAction): JSONObject = JSONObject()
    .put("operation_id", action.operationId).put("receiver_identity", action.receiverIdentity)
    .put("controller_id", action.controllerId).put("subject_id", action.subjectId).put("action", action.action)
    .put("payload_hash", action.payloadHash).put("grant_id", action.grantId).put("grant_revision", action.grantRevision)
    .put("period_id", action.periodId).put("expected_revision", action.expectedRevision)
    .apply { action.draft?.let { put("draft_id", it.id); put("draft_revision", it.revision) } }

private fun decode(value: JSONObject): PendingPhoneAction {
    val fields = value.keys().asSequence().toSet()
    val required = setOf("operation_id", "receiver_identity", "controller_id", "subject_id",
        "action", "payload_hash", "grant_id", "grant_revision", "period_id", "expected_revision")
    require(fields == required || fields == required + setOf("draft_id", "draft_revision"))
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
        number("expected_revision"), if ("draft_id" in fields) PhoneSubmittedDraft(text("draft_id"), text("draft_revision")) else null).also(::validate)
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
