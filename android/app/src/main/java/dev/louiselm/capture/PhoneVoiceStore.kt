package dev.louiselm.capture

import android.content.Context
import android.os.Build
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.Closeable
import java.io.DataInputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.nio.channels.OverlappingFileLockException
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.security.GeneralSecurityException
import java.security.KeyStore
import java.security.KeyStoreException
import java.security.SecureRandom
import java.util.UUID
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONException
import org.json.JSONObject

internal enum class PhoneVoiceIntent { UNDECIDED, REQUESTED, KEPT, ACCEPTED, READY, RETRY_PENDING, CANCEL_PENDING, CANCELLED }

internal data class PhoneVoiceRecord(
    val id: String,
    val source: PhoneOfflineSource,
    val draftId: String,
    val draftRevision: String,
    val intent: PhoneVoiceIntent,
    val audio: CaptureRecord?,
    val retryOperation: String? = null,
)

internal data class PhoneVoiceListing(val records: List<PhoneVoiceRecord>, val damaged: Int, val next: String?)

internal data class PendingPhoneVoice(
    val recording: PendingRecording,
    val initial: ByteArray,
    val requested: ByteArray,
    val lease: PhoneVoiceLease,
)

internal class PhoneVoiceLease(private val released: () -> Unit) : Closeable {
    private var closed = false
    @Synchronized override fun close() {
        if (closed) return
        closed = true
        released()
    }
}

/** App-private Session recordings. Call off the main thread after local unlock. */
internal class PhoneVoiceStore(context: Context, private val metadataKey: (Boolean) -> SecretKey = ::phoneVoiceMetadataKey) {
    private val root = File(context.applicationContext.filesDir, "phone-voice-v1")
    private val app = context.applicationContext as CaptureApplication
    private val captures by lazy { CaptureStore(app, voice = true) }
    private val drafts = PhoneDraftStore(File(context.applicationContext.filesDir, "phone-drafts-v1"))

    fun begin(source: PhoneOfflineSource, draftId: String, revision: String): PendingPhoneVoice {
        require(drafts.id(source) == draftId && revision.matches(HEX_256))
        val recording = captures.begin()
        var lease: PhoneVoiceLease? = null
        try {
            val acquired = recordingLease(recording.id) ?: throw IOException("voice recording is active")
            lease = acquired
            val record = PhoneVoiceRecord(recording.id, source, draftId, revision, PhoneVoiceIntent.UNDECIDED, null)
            return locked {
                val existing = root.listFiles()?.any { it.name.endsWith(".voice") } ?: throw IOException("voice directory unavailable")
                val key = metadataKey(!existing)
                val initial = encrypt(record, key)
                val requested = encrypt(record.copy(intent = PhoneVoiceIntent.REQUESTED), key)
                write(recording.id, initial)
                PendingPhoneVoice(recording, initial, requested, acquired)
            }
        } catch (error: Exception) {
            captures.abort(recording)
            lease?.close()
            throw error
        }
    }

    /** Only call before the recorder started; a started recorder's bytes must be retained. */
    fun abort(pending: PendingPhoneVoice) {
        try {
            captures.abort(pending.recording)
            locked {
                val file = file(pending.recording.id)
                if (file.exists() && !file.delete()) throw IOException("voice metadata could not be removed")
            }
        } finally {
            pending.lease.close()
        }
    }

    /** Publish the original first. A failed intent update leaves it awaiting an explicit choice. */
    fun finish(pending: PendingPhoneVoice, durationMs: Long, requested: Boolean): CaptureRecord {
        try {
            val original = captures.complete(pending.recording, durationMs, null)
            if (requested) locked {
                val existing = readBytes(pending.recording.id)
                if (existing.contentEquals(pending.initial)) write(pending.recording.id, pending.requested)
            }
            return original
        } finally {
            pending.lease.close()
        }
    }

    fun list(after: String? = null): PhoneVoiceListing {
        if (after != null) file(after)
        val metadata = locked {
            val files = root.listFiles() ?: throw IOException("voice directory unavailable")
            val records = files.filter { it.name.endsWith(".voice") }.sortedBy { it.name }
            val valid = records.filter { validId(it.name.removeSuffix(".voice")) }
            var damaged = records.size - valid.size
            val remaining = valid.filter { after == null || it.name.removeSuffix(".voice") > after }
            val page = remaining.take(PAGE_SIZE)
            val next = if (remaining.size > PAGE_SIZE) page.last().name.removeSuffix(".voice") else null
            if (page.isEmpty()) return@locked PhoneVoiceListing(emptyList(), damaged, next)
            val key = metadataKey(false)
            val decoded = mutableListOf<PhoneVoiceRecord>()
            for (entry in page) {
                val id = entry.name.removeSuffix(".voice")
                try { decoded.add(decode(id, readBytes(id), key)) }
                catch (_: IOException) { damaged++ }
                catch (_: GeneralSecurityException) { damaged++ }
                catch (_: IllegalArgumentException) { damaged++ }
                catch (_: JSONException) { damaged++ }
            }
            PhoneVoiceListing(decoded, damaged, next)
        }
        var damaged = metadata.damaged
        val records = mutableListOf<PhoneVoiceRecord>()
        for (record in metadata.records) {
            try { records.add(record.copy(audio = recoverOriginal(record.id))) }
            catch (_: IOException) { damaged++ }
            catch (_: IllegalArgumentException) { damaged++ }
            catch (_: IllegalStateException) { damaged++ }
            catch (_: JSONException) { damaged++ }
        }
        return PhoneVoiceListing(records, damaged, metadata.next)
    }

    /** An interrupted recording can be changed only by an unlocked operator choice. */
    fun choose(id: String, transcribe: Boolean, mayCommit: () -> Boolean): Boolean {
        val lease = recordingLease(id) ?: return false
        return lease.use {
            if (captures.find(id) == null) return@use false
            locked {
                val key = metadataKey(false)
                val record = decode(id, readBytes(id), key)
                if ((record.intent != PhoneVoiceIntent.UNDECIDED && !(record.intent == PhoneVoiceIntent.KEPT && transcribe)) || !mayCommit()) {
                    return@locked false
                }
                val next = if (transcribe) PhoneVoiceIntent.REQUESTED else PhoneVoiceIntent.KEPT
                val encrypted = encrypt(record.copy(intent = next), key)
                if (!mayCommit()) return@locked false
                write(id, encrypted)
                true
            }
        }
    }

    fun original(id: String): CaptureRecord? = captures.find(id)

    /** Successful admission must never undo a concurrent discard. */
    fun markSubmitted(id: String, mayCommit: () -> Boolean): Boolean = change(id, mayCommit) { record ->
        if (record.intent == PhoneVoiceIntent.REQUESTED) record.copy(intent = PhoneVoiceIntent.ACCEPTED) else null
    }

    fun markReady(id: String, mayCommit: () -> Boolean): Boolean = change(id, mayCommit) { record ->
        if (record.intent == PhoneVoiceIntent.ACCEPTED) record.copy(intent = PhoneVoiceIntent.READY) else null
    }

    /** Persist the explicit retry's identity before transport so a lost reply cannot create a second cycle. */
    fun requestRetry(id: String, operation: String, mayCommit: () -> Boolean): Boolean {
        phoneControlHex(operation, 16)
        require(operation != "00".repeat(16))
        return change(id, mayCommit) { record ->
            if (record.intent == PhoneVoiceIntent.ACCEPTED) record.copy(intent = PhoneVoiceIntent.RETRY_PENDING, retryOperation = operation) else null
        }
    }

    fun markRetried(id: String, operation: String, mayCommit: () -> Boolean): Boolean = change(id, mayCommit) { record ->
        if (record.intent == PhoneVoiceIntent.RETRY_PENDING && record.retryOperation == operation)
            record.copy(intent = PhoneVoiceIntent.ACCEPTED, retryOperation = null) else null
    }

    fun markCancelled(id: String, mayCommit: () -> Boolean): Boolean = change(id, mayCommit) { record ->
        if (record.intent == PhoneVoiceIntent.CANCEL_PENDING) record.copy(intent = PhoneVoiceIntent.CANCELLED) else null
    }

    /** Record cancellation before discarding the draft, including uploads with an uncertain outcome. */
    fun requestCancellationForDraft(source: PhoneOfflineSource, mayCommit: () -> Boolean): Boolean = locked {
        if (!mayCommit()) return@locked false
        val files = root.listFiles() ?: throw IOException("voice directory unavailable")
        val entries = files.filter { it.name.endsWith(".voice") && validId(it.name.removeSuffix(".voice")) }
        if (entries.isEmpty()) return@locked true
        val key = metadataKey(false)
        val draftId = drafts.id(source)
        for (entry in entries) {
            val id = entry.name.removeSuffix(".voice")
            val record = decode(id, readBytes(id), key)
            if (record.draftId != draftId || record.intent !in setOf(PhoneVoiceIntent.REQUESTED,
                    PhoneVoiceIntent.ACCEPTED, PhoneVoiceIntent.RETRY_PENDING)) continue
            val encrypted = encrypt(record.copy(intent = PhoneVoiceIntent.CANCEL_PENDING, retryOperation = null), key)
            if (!mayCommit()) return@locked false
            write(id, encrypted)
        }
        true
    }

    private fun change(id: String, mayCommit: () -> Boolean, transform: (PhoneVoiceRecord) -> PhoneVoiceRecord?): Boolean = locked {
        if (!mayCommit()) return@locked false
        val key = metadataKey(false)
        val record = decode(id, readBytes(id), key)
        val changed = transform(record) ?: return@locked false
        val encrypted = encrypt(changed, key)
        if (!mayCommit()) return@locked false
        write(id, encrypted)
        true
    }

    private fun recoverOriginal(id: String): CaptureRecord? {
        val lease = recordingLease(id) ?: return null
        return lease.use {
            try { captures.recoverOne(id) }
            catch (_: RuntimeException) { null } // A recorder killed mid-write may not have finalized its container.
            catch (_: IOException) { null } // Keep incomplete bytes and allow a later retry.
        }
    }

    private fun file(id: String): File {
        require(validId(id))
        return File(root, "$id.voice")
    }

    private fun validId(id: String): Boolean = try { UUID.fromString(id).toString() == id } catch (_: IllegalArgumentException) { false }

    private fun readBytes(id: String): ByteArray {
        val file = file(id)
        val size = file.length()
        if (Files.isSymbolicLink(file.toPath()) || !file.isFile || size !in 28L..MAX_METADATA_BYTES.toLong()) {
            throw IOException("voice metadata is invalid")
        }
        return DataInputStream(FileInputStream(file)).use { input ->
            val bytes = ByteArray(size.toInt())
            input.readFully(bytes)
            if (input.read() != -1) throw IOException("voice metadata changed")
            bytes
        }
    }

    private fun write(id: String, bytes: ByteArray) {
        val target = file(id)
        if (Files.isSymbolicLink(target.toPath())) throw IOException("invalid voice metadata path")
        val temporary = File.createTempFile(".voice-", ".tmp", root)
        try {
            FileOutputStream(temporary).use { it.write(bytes); it.fd.sync() }
            Files.move(temporary.toPath(), target.toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
        } finally {
            // A leftover temporary contains only authenticated ciphertext.
            if (temporary.exists()) temporary.delete()
        }
    }

    private fun encrypt(record: PhoneVoiceRecord, key: SecretKey): ByteArray {
        val source = record.source
        val json = JSONObject().put("v", 1).put("receiver", source.receiver)
            .put("controller", source.target.controllerId).put("subject", source.target.subjectId)
            .put("name", source.name).put("agent", source.agent).put("project", source.project)
            .put("draft", record.draftId).put("revision", record.draftRevision).put("intent", record.intent.name)
            .put("retry", record.retryOperation ?: JSONObject.NULL)
        val nonce = ByteArray(12).also { SecureRandom().nextBytes(it) }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key, GCMParameterSpec(128, nonce))
        cipher.updateAAD(record.id.toByteArray(Charsets.US_ASCII))
        val encrypted = nonce + cipher.doFinal(json.toString().toByteArray(Charsets.UTF_8))
        if (encrypted.size > MAX_METADATA_BYTES) throw IOException("voice metadata is too large")
        return encrypted
    }

    private fun decode(id: String, bytes: ByteArray, key: SecretKey): PhoneVoiceRecord {
        file(id)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, bytes, 0, 12))
        cipher.updateAAD(id.toByteArray(Charsets.US_ASCII))
        val json = JSONObject(String(cipher.doFinal(bytes, 12, bytes.size - 12), Charsets.UTF_8))
        require(json.getInt("v") == 1)
        val source = PhoneOfflineSource(json.getString("receiver"), PhoneControlTarget(json.getString("controller"), json.getString("subject")),
            json.getString("name"), json.getString("agent"), json.getString("project"))
        val draftId = json.getString("draft")
        val revision = json.getString("revision")
        require(drafts.id(source) == draftId && revision.matches(HEX_256))
        val intent = PhoneVoiceIntent.valueOf(json.getString("intent"))
        val retry = if (!json.has("retry") || json.isNull("retry")) null else json.getString("retry")
        require((intent == PhoneVoiceIntent.RETRY_PENDING) == (retry != null))
        if (retry != null) {
            phoneControlHex(retry, 16)
            require(retry != "00".repeat(16))
        }
        return PhoneVoiceRecord(id, source, draftId, revision, intent, null, retry)
    }

    private fun <T> locked(block: () -> T): T {
        ensureRoot()
        val lockFile = File(root, ".lock")
        if (Files.isSymbolicLink(lockFile.toPath())) throw IOException("invalid voice lock")
        return synchronized(app.voiceMetadataLock) {
            FileOutputStream(lockFile, true).channel.use { channel ->
                val lock = try { channel.tryLock() } catch (_: OverlappingFileLockException) { null }
                    ?: throw IOException("voice store busy")
                lock.use { block() }
            }
        }
    }

    // The manifest puts all app components in one process. Its Application owns these
    // identities; process death drops them and allows a later Activity to recover audio.
    private fun recordingLease(id: String): PhoneVoiceLease? {
        file(id)
        val held = app.voiceRecordingLocks
        synchronized(held) {
            if (!held.add(id)) return null
            return PhoneVoiceLease { synchronized(held) { held.remove(id) } }
        }
    }

    private fun ensureRoot() {
        if (Files.isSymbolicLink(root.toPath())) throw IOException("invalid voice directory")
        if (!root.isDirectory && !root.mkdirs() && !root.isDirectory) throw IOException("voice directory unavailable")
    }

    private companion object {
        const val MAX_METADATA_BYTES = 16 * 1024
        const val PAGE_SIZE = 50
        val HEX_256 = Regex("[0-9a-f]{64}")
    }
}

private fun phoneVoiceMetadataKey(create: Boolean): SecretKey {
    val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    (store.getKey(VOICE_METADATA_KEY_ALIAS, null) as? SecretKey)?.let { return it }
    if (!create) throw KeyStoreException("voice metadata key is unavailable")
    return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
        init(phoneVoiceMetadataKeySpec())
        generateKey()
    }
}

internal fun phoneVoiceMetadataKeySpec(): KeyGenParameterSpec {
    val builder = KeyGenParameterSpec.Builder(VOICE_METADATA_KEY_ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
        .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE).setKeySize(256)
    // Android 12–14 can delete these keys when the screen lock is removed.
    // Presentation still requires the separate locally authenticated period on every version.
    // https://developer.android.com/reference/android/security/keystore/KeyGenParameterSpec.Builder#setUnlockedDeviceRequired(boolean)
    if (Build.VERSION.SDK_INT >= 35) builder.setUnlockedDeviceRequired(true)
    return builder.build()
}

private const val VOICE_METADATA_KEY_ALIAS = "louiselm-session-voice-metadata-v1"
