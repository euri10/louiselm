package dev.louiselm.capture

import java.io.File
import java.io.DataInputStream
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.nio.channels.OverlappingFileLockException
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.security.GeneralSecurityException
import java.security.MessageDigest
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject

internal data class PhoneDraftSelection(val source: PhoneOfflineSource, val page: PhoneControlReply.SessionPage)
internal data class PhoneDraft(val source: PhoneOfflineSource, val text: String, val context: List<PhoneDraftSelection>)
internal data class PhoneDraftListing(val drafts: List<PhoneDraft>, val damaged: List<String>)
internal enum class PhoneDraftSave { SAVED, BYTE_LIMIT, COUNT_LIMIT, DAMAGED, CANCELLED, INVALID, CONFLICT }

/** Separate encrypted phone work. Call on a background thread with the locally unlocked draft key. */
internal class PhoneDraftStore(private val root: File) {
    fun id(source: PhoneOfflineSource): String {
        validateSource(source)
        val identity = "draft:${source.receiver}:${source.target.controllerId}:${source.target.subjectId}"
        return phoneControlHex(MessageDigest.getInstance("SHA-256").digest(identity.toByteArray(Charsets.UTF_8)))
    }

    /** The authenticated ciphertext identifies this exact saved version, including after recreation. */
    fun revision(draft: PhoneDraft, key: SecretKey): String? = locked {
        val file = File(root, "${id(draft.source)}.draft")
        if (!file.exists()) return@locked null
        if (!keyMatches(key, false)) throw IOException("draft key changed")
        if (read(file, key) != draft) return@locked null
        phoneControlHex(MessageDigest.getInstance("SHA-256").digest(readCiphertext(file)))
    }

    /** Publish a complete replacement atomically; refusal leaves the previous ciphertext untouched. */
    fun save(draft: PhoneDraft, key: SecretKey, onlyIfAbsentOrSame: Boolean = false, mayCommit: () -> Boolean): PhoneDraftSave {
        val id = id(draft.source)
        if (!validText(draft.text)) return PhoneDraftSave.INVALID
        if (draft.text.toByteArray(Charsets.UTF_8).size > MAX_DRAFT_BYTES || draft.context.size > MAX_SELECTIONS) return PhoneDraftSave.BYTE_LIMIT
        val encoded = encode(draft) ?: return PhoneDraftSave.BYTE_LIMIT
        if (encoded.size > MAX_DRAFT_BYTES - OVERHEAD) return PhoneDraftSave.BYTE_LIMIT
        val encrypted = encrypt(encoded, id, key)
        return locked {
            if (!mayCommit()) return@locked PhoneDraftSave.CANCELLED
            val files = records()
            if (!keyMatches(key, files.isEmpty())) return@locked PhoneDraftSave.DAMAGED
            val destination = File(root, "$id.draft")
            val existing = files.firstOrNull { it.name == destination.name }
            if (existing != null) {
                val old = try { read(existing, key) } catch (_: IOException) { return@locked PhoneDraftSave.DAMAGED }
                catch (_: GeneralSecurityException) { return@locked PhoneDraftSave.DAMAGED }
                catch (_: IllegalArgumentException) { return@locked PhoneDraftSave.DAMAGED }
                catch (_: JSONException) { return@locked PhoneDraftSave.DAMAGED }
                if (onlyIfAbsentOrSame) return@locked if (old == draft) PhoneDraftSave.SAVED else PhoneDraftSave.CONFLICT
            } else if (files.size >= MAX_DRAFTS) return@locked PhoneDraftSave.COUNT_LIMIT
            val used = files.sumOf { it.length() }
            if (used - (existing?.length() ?: 0L) + encrypted.size > MAX_TOTAL_BYTES) return@locked PhoneDraftSave.BYTE_LIMIT
            val temporary = File.createTempFile(".draft-", ".tmp", root)
            try {
                FileOutputStream(temporary).use { it.write(encrypted); it.fd.sync() }
                if (!mayCommit()) return@locked PhoneDraftSave.CANCELLED
                Files.move(temporary.toPath(), destination.toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
                PhoneDraftSave.SAVED
            } finally {
                // Temporary files contain only authenticated ciphertext.
                if (temporary.exists()) temporary.delete()
            }
        }
    }

    fun list(key: SecretKey): PhoneDraftListing = locked {
        val drafts = mutableListOf<PhoneDraft>()
        val damaged = mutableListOf<String>()
        val files = records()
        if (files.isNotEmpty() && !keyMatches(key, false)) return@locked PhoneDraftListing(emptyList(), files.map { it.name })
        for (file in files) {
            try { drafts.add(read(file, key)) }
            catch (_: IOException) { damaged.add(file.name) }
            catch (_: GeneralSecurityException) { damaged.add(file.name) }
            catch (_: IllegalArgumentException) { damaged.add(file.name) }
            catch (_: JSONException) { damaged.add(file.name) }
        }
        PhoneDraftListing(drafts, damaged)
    }

    /** Only explicit discard removes a readable draft; pending actions and recordings live elsewhere. */
    fun discard(source: PhoneOfflineSource, key: SecretKey, mayCommit: () -> Boolean): Boolean = locked {
        val file = File(root, "${id(source)}.draft")
        if (!file.exists()) return@locked false
        if (!keyMatches(key, false)) throw IOException("draft key changed")
        read(file, key)
        if (!mayCommit()) return@locked false
        if (!file.delete()) throw IOException("draft could not be discarded")
        true
    }

    /** A newly created wrapped key may replace a loss fence only after all old records are gone. */
    fun installKey(key: SecretKey) = locked {
        if (records().isNotEmpty()) throw IOException("drafts still exist")
        writeKeyMarker(keyId(key))
    }

    /** Caller has proved key loss and obtained explicit confirmation. Fence old periods before deletion. */
    fun resetLost() = locked {
        writeKeyMarker("retired")
        for (file in records()) {
            if (Files.isSymbolicLink(file.toPath()) || !file.isFile || !file.delete()) throw IOException("draft deletion failed")
        }
    }

    private fun keyMatches(key: SecretKey, allowInitial: Boolean): Boolean {
        val marker = File(root, ".key-id")
        if (Files.isSymbolicLink(marker.toPath())) throw IOException("invalid draft key marker")
        if (!marker.exists()) {
            if (!allowInitial) return false
            writeKeyMarker(keyId(key))
        }
        if (marker.length() !in 1..64) throw IOException("invalid draft key marker")
        return marker.readText(Charsets.US_ASCII) == keyId(key)
    }

    private fun keyId(key: SecretKey): String {
        val material = key.encoded ?: throw GeneralSecurityException("draft key unavailable")
        require(material.size == 32)
        return phoneControlHex(MessageDigest.getInstance("SHA-256").digest(material))
    }

    private fun writeKeyMarker(value: String) {
        val temporary = File.createTempFile(".key-id-", ".tmp", root)
        try {
            FileOutputStream(temporary).use { it.write(value.toByteArray(Charsets.US_ASCII)); it.fd.sync() }
            Files.move(temporary.toPath(), File(root, ".key-id").toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
        } finally {
            // This marker identifies a random key and contains no draft content.
            if (temporary.exists()) temporary.delete()
        }
    }

    private fun read(file: File, key: SecretKey): PhoneDraft {
        val id = file.name.removeSuffix(".draft")
        if (!id.matches(Regex("[0-9a-f]{64}"))) throw IOException("invalid draft record")
        val encrypted = readCiphertext(file)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, encrypted, 0, NONCE_BYTES))
        cipher.updateAAD(id.toByteArray(Charsets.US_ASCII))
        val draft = decode(cipher.doFinal(encrypted, NONCE_BYTES, encrypted.size - NONCE_BYTES))
        if (id(draft.source) != id) throw IOException("draft identity changed")
        return draft
    }

    private fun readCiphertext(file: File): ByteArray {
        val size = file.length()
        if (Files.isSymbolicLink(file.toPath()) || !file.isFile || size !in OVERHEAD.toLong()..MAX_DRAFT_BYTES.toLong()) {
            throw IOException("invalid draft record")
        }
        return DataInputStream(FileInputStream(file)).use { input ->
            val bytes = ByteArray(size.toInt())
            input.readFully(bytes)
            if (input.read() != -1) throw IOException("draft length changed")
            bytes
        }
    }

    private fun records(): List<File> = (root.listFiles() ?: throw IOException("draft directory unavailable"))
        .filter { it.name.endsWith(".draft") }

    private fun <T> locked(block: () -> T): T {
        if (Files.isSymbolicLink(root.toPath())) throw IOException("invalid draft directory")
        if (!root.isDirectory && !root.mkdirs() && !root.isDirectory) throw IOException("draft directory unavailable")
        val lockFile = File(root, ".lock")
        if (Files.isSymbolicLink(lockFile.toPath())) throw IOException("invalid draft lock")
        return FileOutputStream(lockFile, true).channel.use { channel ->
            val lock = try { channel.tryLock() } catch (_: OverlappingFileLockException) { null }
                ?: throw IOException("draft store busy")
            lock.use { block() }
        }
    }

    private fun encrypt(bytes: ByteArray, id: String, key: SecretKey): ByteArray {
        val nonce = ByteArray(NONCE_BYTES).also { SecureRandom().nextBytes(it) }
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key, GCMParameterSpec(128, nonce))
        cipher.updateAAD(id.toByteArray(Charsets.US_ASCII))
        return nonce + cipher.doFinal(bytes)
    }

    private fun encode(draft: PhoneDraft): ByteArray? {
        val selections = JSONArray()
        var estimatedBytes = draft.text.toByteArray(Charsets.UTF_8).size
        for (selection in draft.context) {
            validateSource(selection.source)
            require(selection.page.controllerId == selection.source.target.controllerId && selection.page.subjectId == selection.source.target.subjectId)
            require(selection.page.entries.size == 1)
            val page = encodePhoneOfflinePage(selection.page)
            estimatedBytes += page.size
            if (estimatedBytes > MAX_DRAFT_BYTES) return null
            selections.put(JSONObject().put("source", sourceJson(selection.source))
                .put("page", String(page, Charsets.UTF_8)))
        }
        return JSONObject().put("v", 1).put("source", sourceJson(draft.source)).put("text", draft.text)
            .put("context", selections).toString().toByteArray(Charsets.UTF_8)
    }

    private fun decode(bytes: ByteArray): PhoneDraft {
        val json = JSONObject(String(bytes, Charsets.UTF_8))
        require(json.getInt("v") == 1)
        val source = parseSource(json.getJSONObject("source"))
        val text = json.getString("text")
        val items = json.getJSONArray("context")
        require(items.length() <= MAX_SELECTIONS)
        val selections = (0 until items.length()).map { index ->
            val item = items.getJSONObject(index)
            val selectedSource = parseSource(item.getJSONObject("source"))
            val page = parsePhoneControlReply(item.getString("page").toByteArray(Charsets.UTF_8)) as? PhoneControlReply.SessionPage
                ?: throw IOException("invalid selected context")
            require(page.controllerId == selectedSource.target.controllerId && page.subjectId == selectedSource.target.subjectId && page.entries.size == 1)
            PhoneDraftSelection(selectedSource, page)
        }
        return PhoneDraft(source, text, selections)
    }

    private fun sourceJson(source: PhoneOfflineSource): JSONObject {
        validateSource(source)
        return JSONObject().put("receiver", source.receiver).put("controller", source.target.controllerId)
            .put("subject", source.target.subjectId).put("name", source.name).put("agent", source.agent).put("project", source.project)
    }

    private fun parseSource(json: JSONObject) = PhoneOfflineSource(json.getString("receiver"),
        PhoneControlTarget(json.getString("controller"), json.getString("subject")),
        json.getString("name"), json.getString("agent"), json.getString("project")).also(::validateSource)

    private fun validText(value: String): Boolean {
        var index = 0
        while (index < value.length) {
            val character = value[index++]
            if (Character.isHighSurrogate(character)) {
                if (index == value.length || !Character.isLowSurrogate(value[index++])) return false
            } else if (Character.isLowSurrogate(character)) return false
        }
        return true
    }

    private fun validateSource(source: PhoneOfflineSource) {
        phoneControlHex(source.receiver, 32)
        require(source.target.controllerId != "00".repeat(16) && source.target.subjectId != "00".repeat(16))
        require(listOf(source.name, source.agent, source.project).all { '\u0000' !in it && it.toByteArray(Charsets.UTF_8).size <= 512 })
    }

    private companion object {
        const val MAX_DRAFTS = 128
        const val MAX_DRAFT_BYTES = 8 * 1024 * 1024
        const val MAX_TOTAL_BYTES = 64L * 1024 * 1024
        // Even the shortest encoded selection exceeds 32 bytes; the byte quota is tighter.
        const val MAX_SELECTIONS = MAX_DRAFT_BYTES / 32
        const val NONCE_BYTES = 12
        const val OVERHEAD = NONCE_BYTES + 16
    }
}
