package dev.louiselm.capture

import java.io.DataInputStream
import java.io.DataOutputStream
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.io.IOException
import java.nio.ByteBuffer
import java.nio.file.Files
import java.nio.file.LinkOption
import java.nio.file.StandardCopyOption
import java.security.MessageDigest
import java.security.SecureRandom
import javax.crypto.AEADBadTagException
import javax.crypto.Cipher
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import org.json.JSONException
import org.json.JSONObject

internal data class PhoneOfflineSource(
    val receiver: String, val target: PhoneControlTarget, val name: String, val agent: String, val project: String,
)

internal data class PhoneOfflineCopy(
    val source: PhoneOfflineSource, val openedAtMs: Long, val refreshedAtMs: Long, val pinned: Boolean,
    val pages: Int, val incomplete: Boolean, val refreshFailed: Boolean, val bytes: Long, internal val version: String,
)

internal data class PhoneOfflineListing(val copies: List<PhoneOfflineCopy>, val damaged: List<String>)
internal enum class PhoneOfflineSave { SAVED, BYTE_LIMIT, SESSION_LIMIT, DAMAGED, CANCELLED, KEEP_EXISTING, REVOKED }

/** Encrypted app-private downloads. Call on a background thread with a locally unlocked download key. */
internal class PhoneOfflineStore(private val root: File) {
    /** Pages arrive newest first; a failed read, write, or quota check leaves the old copy intact. */
    fun save(
        source: PhoneOfflineSource, pages: Sequence<PhoneControlReply.SessionPage>, pin: Boolean,
        openedAtMs: Long?, nowMs: Long, budget: Long, key: SecretKey, mayCommit: () -> Boolean,
    ): PhoneOfflineSave {
        validateSource(source)
        require(nowMs >= 0 && budget > 0 && (openedAtMs == null || openedAtMs >= 0))
        val directory = directory(source.receiver)
        ensureDirectory(directory)
        val id = copyId(source)
        val version = phoneControlHex(ByteArray(16).also { SecureRandom().nextBytes(it) })
        val temporary = File.createTempFile(".download-", ".tmp", directory)
        var published: File? = null
        try {
            var count = 0
            var previous: PhoneControlReply.SessionPage? = null
            var incomplete = false
            FileOutputStream(temporary).use { file ->
                val output = DataOutputStream(file)
                output.writeInt(MAGIC)
                for (page in pages) {
                    if (!mayCommit()) return PhoneOfflineSave.CANCELLED
                    require(count < MAX_PAGES && page.controllerId == source.target.controllerId &&
                        page.subjectId == source.target.subjectId)
                    val prior = previous
                    if (prior != null) {
                        require(prior.nextBefore != 0L && page.revision == prior.revision && page.status == prior.status && page.settings == prior.settings && page.currentPermission == prior.currentPermission && page.currentInteractions == prior.currentInteractions &&
                            page.entries.all { it.seq < prior.nextBefore } &&
                            (page.nextBefore == 0L || page.nextBefore < prior.nextBefore))
                    }
                    val encoded = encodePhoneOfflinePage(page)
                    val encrypted = encrypt(key, encoded, pageAad(id, version, count))
                    output.writeInt(encrypted.size)
                    output.write(encrypted)
                    if (file.channel.position() > budget - META_BYTES) return PhoneOfflineSave.BYTE_LIMIT
                    count++
                    incomplete = incomplete || !page.complete
                    previous = page
                }
                require(count > 0)
                incomplete = incomplete || previous?.nextBefore != 0L
                output.flush()
                file.fd.sync()
            }
            return locked {
                if (!mayCommit()) return@locked PhoneOfflineSave.CANCELLED
                if (hasMarker(revocationFile(directory)) || hasMarker(denialFile(directory, id))) return@locked PhoneOfflineSave.REVOKED
                val listing = readAll(key)
                if (listing.damaged.isNotEmpty()) return@locked PhoneOfflineSave.DAMAGED
                val old = listing.copies.firstOrNull { copyId(it.source) == id }
                if (old?.pinned == true && (!pin || incomplete && !old.incomplete)) return@locked PhoneOfflineSave.KEEP_EXISTING
                val opened = openedAtMs ?: old?.openedAtMs ?: throw IllegalArgumentException("a copy must first be opened")
                val dataBytes = temporary.length()
                val copy = PhoneOfflineCopy(source, opened, nowMs, pin || old?.pinned == true,
                    count, incomplete, false, dataBytes + META_BYTES, version)
                val usage = listing.copies.map { PhoneOfflineUsage(copyId(it.source), it.bytes, it.openedAtMs, it.pinned) }
                val decision = planPhoneOfflineRetention(usage, PhoneOfflineUsage(id, copy.bytes, opened, copy.pinned), budget, nowMs)
                when (decision) {
                    PhoneOfflinePlan.Refuse.BYTE_LIMIT -> return@locked PhoneOfflineSave.BYTE_LIMIT
                    PhoneOfflinePlan.Refuse.SESSION_LIMIT -> return@locked PhoneOfflineSave.SESSION_LIMIT
                    is PhoneOfflinePlan.Accept -> {
                        val data = dataFile(directory, id, version)
                        atomicMove(temporary, data)
                        published = data
                        if (!mayCommit()) return@locked PhoneOfflineSave.CANCELLED
                        writeMetadata(directory, id, copy, dataBytes, key)
                        published = null
                        if (old != null) deleteFile(dataFile(directory, id, old.version))
                        for (remove in decision.remove) {
                            val victim = listing.copies.first { copyId(it.source) == remove }
                            val victimDirectory = directory(victim.source.receiver)
                            deleteFile(metadataFile(victimDirectory, remove))
                            deleteFile(dataFile(victimDirectory, remove, victim.version))
                        }
                        PhoneOfflineSave.SAVED
                    }
                }
            }
        } finally {
            // Unpublished files contain only ciphertext; an interrupted cleanup cannot expose content.
            if (temporary.exists()) temporary.delete()
            published?.delete()
        }
    }

    fun list(receiver: String, key: SecretKey): PhoneOfflineListing = locked {
        readListing(directory(receiver), receiver, key)
    }

    /** The encrypted metadata supplies source identity without opening capture credentials. */
    fun listAll(key: SecretKey): PhoneOfflineListing = locked { readAll(key) }

    /** Apply expiry and an adjusted budget without ever selecting pins for removal. */
    fun maintain(receiver: String, budget: Long, nowMs: Long, key: SecretKey): PhoneOfflineSave = locked {
        maintainListing(readListing(directory(receiver), receiver, key), budget, nowMs)
    }

    fun maintainAll(budget: Long, nowMs: Long, key: SecretKey): PhoneOfflineSave = locked {
        maintainListing(readAll(key), budget, nowMs)
    }

    private fun maintainListing(listing: PhoneOfflineListing, budget: Long, nowMs: Long): PhoneOfflineSave {
        if (listing.damaged.isNotEmpty()) return PhoneOfflineSave.DAMAGED
        val usage = listing.copies.map { PhoneOfflineUsage(copyId(it.source), it.bytes, it.openedAtMs, it.pinned) }
        val decision = planPhoneOfflineRetention(usage, null, budget, nowMs)
        val remove = when (decision) {
            is PhoneOfflinePlan.Accept -> decision.remove
            else -> usage.filter { !it.pinned && nowMs >= it.openedAtMs &&
                nowMs - it.openedAtMs >= PHONE_OFFLINE_EXPIRY_MS }.map { it.id }
        }
        for (id in remove) {
            val victim = listing.copies.first { copyId(it.source) == id }
            val victimDirectory = directory(victim.source.receiver)
            deleteFile(metadataFile(victimDirectory, id))
            deleteFile(dataFile(victimDirectory, id, victim.version))
        }
        return when (decision) {
            is PhoneOfflinePlan.Accept -> PhoneOfflineSave.SAVED
            PhoneOfflinePlan.Refuse.BYTE_LIMIT -> PhoneOfflineSave.BYTE_LIMIT
            PhoneOfflinePlan.Refuse.SESSION_LIMIT -> PhoneOfflineSave.SESSION_LIMIT
        }
    }

    /** Read one saved page without loading the complete download into memory. */
    fun readPage(source: PhoneOfflineSource, index: Int, key: SecretKey): PhoneControlReply.SessionPage = locked {
        validateSource(source)
        val directory = directory(source.receiver)
        val id = copyId(source)
        val copy = readMetadata(metadataFile(directory, id), id, source.receiver, key)
        require(copy.source == source && index in 0 until copy.pages)
        val file = dataFile(directory, id, copy.version)
        if (Files.isSymbolicLink(file.toPath())) throw IOException("invalid download path")
        if (file.length() != copy.bytes - META_BYTES) throw IOException("download length changed")
        DataInputStream(FileInputStream(file)).use { input ->
            if (input.readInt() != MAGIC) throw IOException("invalid download")
            for (number in 0..index) {
                val length = input.readInt()
                if (length !in (NONCE_BYTES + TAG_BYTES)..(NONCE_BYTES + TAG_BYTES + MAX_PAGE_BYTES)) {
                    throw IOException("invalid download page")
                }
                val bytes = ByteArray(length)
                input.readFully(bytes)
                if (number == index) {
                    val decoded = decrypt(key, bytes, pageAad(id, copy.version, number))
                    val page = parsePhoneControlReply(decoded) as? PhoneControlReply.SessionPage
                        ?: throw IOException("invalid download page")
                    if (page.controllerId != source.target.controllerId || page.subjectId != source.target.subjectId) {
                        throw IOException("download subject changed")
                    }
                    return@locked page
                }
            }
            throw IOException("missing download page")
        }
    }

    fun markOpened(source: PhoneOfflineSource, nowMs: Long, key: SecretKey) {
        require(nowMs >= 0)
        updateMetadata(source, key) { it.copy(openedAtMs = maxOf(it.openedAtMs, nowMs)) }
    }

    fun setPinned(source: PhoneOfflineSource, pinned: Boolean, key: SecretKey) {
        updateMetadata(source, key) { it.copy(pinned = pinned) }
    }

    fun markRefreshFailed(source: PhoneOfflineSource, key: SecretKey) {
        updateMetadata(source, key) { if (it.pinned) it.copy(refreshFailed = true) else it }
    }

    fun remove(source: PhoneOfflineSource) = denySource(source)

    /** Fence one subject when read scope is lost or its saved copy is explicitly removed. */
    fun denySource(source: PhoneOfflineSource) = locked {
        validateSource(source)
        val directory = directory(source.receiver)
        ensureDirectory(directory)
        val id = copyId(source)
        writeMarker(denialFile(directory, id))
        purgeDeniedSource(directory, id)
    }

    /** Observe the fence before an explicit read, so a later refusal cannot be undone by that read. */
    fun sourceFence(source: PhoneOfflineSource): String? = locked {
        validateSource(source)
        val directory = directory(source.receiver)
        if (hasMarker(revocationFile(directory))) throw SecurityException("downloads were revoked")
        markerValue(denialFile(directory, copyId(source)))
    }

    /** Clear only the exact fence observed before a successful authorized, explicit read. */
    fun allowSource(source: PhoneOfflineSource, observed: String?): Boolean = locked {
        validateSource(source)
        val directory = directory(source.receiver)
        if (hasMarker(revocationFile(directory))) throw SecurityException("downloads were revoked")
        val marker = denialFile(directory, copyId(source))
        if (markerValue(marker) != observed) return@locked false
        if (observed != null) deleteFile(marker)
        true
    }

    /** Recheck before explicitly removing an unreadable download, leaving other copies and keys intact. */
    fun removeDamaged(id: String, key: SecretKey) = locked {
        require(id.matches(Regex("[0-9a-f]{64}")))
        val directories = root.listFiles() ?: throw IOException("downloads unavailable")
        for (directory in directories.filter { it.name.matches(DIRECTORY_NAME) }) {
            if (Files.isSymbolicLink(directory.toPath()) || !directory.isDirectory) throw IOException("invalid download path")
            if (id in readListing(directory, null, key).damaged) {
                writeMarker(denialFile(directory, id))
                purgeDeniedSource(directory, id)
            }
        }
    }

    /** Fence this computer before removing its downloads; the fence survives process restarts. */
    fun markRevoked(receiver: String) = locked {
        val directory = directory(receiver)
        ensureDirectory(directory)
        writeMarker(revocationFile(directory))
    }

    /** Retain the fence until a new computer-approved grant exists. */
    fun purgeRevoked(receiver: String) = locked { purgeRevokedContents(directory(receiver)) }

    fun allowReceiver(receiver: String) = locked {
        val directory = directory(receiver)
        if (hasMarker(revocationFile(directory))) {
            purgeRevokedContents(directory)
            deleteFile(revocationFile(directory))
        }
    }

    private fun updateMetadata(source: PhoneOfflineSource, key: SecretKey, update: (PhoneOfflineCopy) -> PhoneOfflineCopy) = locked {
        validateSource(source)
        val directory = directory(source.receiver)
        val id = copyId(source)
        val current = readMetadata(metadataFile(directory, id), id, source.receiver, key)
        require(current.source == source)
        writeMetadata(directory, id, update(current), current.bytes - META_BYTES, key)
    }

    private fun readAll(key: SecretKey): PhoneOfflineListing {
        val directories = root.listFiles() ?: if (!root.exists()) return PhoneOfflineListing(emptyList(), emptyList())
            else throw IOException("downloads unavailable")
        val copies = mutableListOf<PhoneOfflineCopy>()
        val damaged = mutableListOf<String>()
        for (directory in directories.filter { it.name.matches(DIRECTORY_NAME) }) {
            if (Files.isSymbolicLink(directory.toPath()) || !directory.isDirectory) throw IOException("invalid download path")
            val listing = readListing(directory, null, key)
            copies.addAll(listing.copies)
            damaged.addAll(listing.damaged)
        }
        return PhoneOfflineListing(copies.sortedByDescending { it.openedAtMs }, damaged)
    }

    private fun readListing(directory: File, receiver: String?, key: SecretKey): PhoneOfflineListing {
        if (!directory.exists()) return PhoneOfflineListing(emptyList(), emptyList())
        if (hasMarker(revocationFile(directory))) {
            purgeRevokedContents(directory)
            return PhoneOfflineListing(emptyList(), emptyList())
        }
        val copies = mutableListOf<PhoneOfflineCopy>()
        val damaged = mutableListOf<String>()
        var files = directory.listFiles() ?: throw IOException("downloads unavailable")
        for (file in files.filter { it.name.matches(DENIAL_NAME) }) purgeDeniedSource(directory, file.name.removePrefix(".deny-"))
        files = directory.listFiles() ?: throw IOException("downloads unavailable")
        for (file in files.filter { it.name.matches(METADATA_NAME) }) {
            val id = file.name.removeSuffix(".meta")
            try {
                val copy = readMetadata(file, id, receiver, key)
                require(directory.name == this.directory(copy.source.receiver).name)
                copies.add(copy)
            } catch (_: AEADBadTagException) {
                damaged.add(id)
            } catch (_: IOException) {
                damaged.add(id)
            } catch (_: IllegalArgumentException) {
                damaged.add(id)
            } catch (_: JSONException) {
                damaged.add(id)
            }
        }
        if (damaged.isEmpty()) {
            val active = copies.map { "${copyId(it.source)}.${it.version}.data" }.toSet()
            for (file in files.filter { it.name.matches(DATA_NAME) && it.name !in active }) deleteFile(file)
        }
        return PhoneOfflineListing(copies.sortedByDescending { it.openedAtMs }, damaged)
    }

    private fun readMetadata(file: File, id: String, receiver: String?, key: SecretKey): PhoneOfflineCopy {
        val directory = file.parentFile ?: throw IOException("invalid download path")
        if (hasMarker(revocationFile(directory)) || hasMarker(denialFile(directory, id))) {
            throw SecurityException("downloads were revoked")
        }
        if (Files.isSymbolicLink(file.toPath())) throw IOException("invalid download path")
        if (file.length() != META_BYTES.toLong()) throw IOException("invalid download metadata")
        val encrypted = ByteArray(META_BYTES)
        DataInputStream(FileInputStream(file)).use { input ->
            input.readFully(encrypted)
            if (input.read() != -1) throw IOException("invalid download metadata")
        }
        val plain = decrypt(key, encrypted, metadataAad(id))
        val buffer = ByteBuffer.wrap(plain)
        val length = buffer.int
        require(length in 1..(META_PLAINTEXT_BYTES - 4))
        val encoded = ByteArray(length).also { buffer.get(it) }
        val json = JSONObject(String(encoded, Charsets.UTF_8))
        require(json.keys().asSequence().toSet() == setOf("v", "receiver", "controller", "subject", "name", "agent", "project",
            "opened", "refreshed", "pinned", "pages", "incomplete", "refresh_failed", "length", "version"))
        fun text(name: String) = json.get(name) as? String ?: throw IllegalArgumentException("invalid metadata")
        fun number(name: String): Long = when (val value = json.get(name)) {
            is Int -> value.toLong()
            is Long -> value
            else -> throw IllegalArgumentException("invalid metadata")
        }
        fun boolean(name: String) = json.get(name) as? Boolean ?: throw IllegalArgumentException("invalid metadata")
        require(number("v") == 1L)
        val source = PhoneOfflineSource(text("receiver"), PhoneControlTarget(text("controller"), text("subject")),
            text("name"), text("agent"), text("project"))
        validateSource(source)
        require((receiver == null || source.receiver == receiver) && copyId(source) == id)
        val opened = number("opened")
        val refreshed = number("refreshed")
        val pages = number("pages")
        val bytes = number("length")
        val version = text("version")
        phoneControlHex(version, 16)
        require(opened >= 0 && refreshed >= 0 && pages in 1..MAX_PAGES.toLong() && bytes in 4..Long.MAX_VALUE - META_BYTES)
        return PhoneOfflineCopy(source, opened, refreshed, boolean("pinned"), pages.toInt(), boolean("incomplete"),
            boolean("refresh_failed"), bytes + META_BYTES, version)
    }

    private fun writeMetadata(directory: File, id: String, copy: PhoneOfflineCopy, dataBytes: Long, key: SecretKey) {
        val source = copy.source
        val json = JSONObject().put("v", 1).put("receiver", source.receiver).put("controller", source.target.controllerId)
            .put("subject", source.target.subjectId).put("name", source.name).put("agent", source.agent).put("project", source.project)
            .put("opened", copy.openedAtMs).put("refreshed", copy.refreshedAtMs).put("pinned", copy.pinned)
            .put("pages", copy.pages).put("incomplete", copy.incomplete).put("refresh_failed", copy.refreshFailed)
            .put("length", dataBytes).put("version", copy.version)
        val encoded = json.toString().toByteArray(Charsets.UTF_8)
        require(encoded.size <= META_PLAINTEXT_BYTES - 4)
        val plain = ByteBuffer.allocate(META_PLAINTEXT_BYTES).putInt(encoded.size).put(encoded).array()
        val encrypted = encrypt(key, plain, metadataAad(id))
        val temporary = File.createTempFile(".metadata-", ".tmp", directory)
        try {
            FileOutputStream(temporary).use { it.write(encrypted); it.fd.sync() }
            atomicMove(temporary, metadataFile(directory, id))
        } finally {
            // A leftover temporary file contains only encrypted metadata.
            if (temporary.exists()) temporary.delete()
        }
    }

    private fun <T> locked(block: () -> T): T = synchronized(PhoneOfflineStore::class.java) {
        ensureDirectory(root)
        FileOutputStream(File(root, ".lock"), true).channel.use { channel ->
            channel.lock().use { block() }
        }
    }

    private fun directory(receiver: String): File {
        phoneControlHex(receiver, 32)
        if (Files.isSymbolicLink(root.toPath())) throw IOException("invalid download path")
        return File(root, digest("receiver:$receiver")).also {
            if (Files.isSymbolicLink(it.toPath())) throw IOException("invalid download path")
        }
    }

    private fun validateSource(source: PhoneOfflineSource) {
        phoneControlHex(source.receiver, 32)
        require(source.target.controllerId != "00".repeat(16) && source.target.subjectId != "00".repeat(16))
        require(listOf(source.name, source.agent, source.project).all { '\u0000' !in it && it.toByteArray(Charsets.UTF_8).size <= 512 })
    }

    private fun copyId(source: PhoneOfflineSource): String = digest("copy:${source.receiver}:${source.target.controllerId}:${source.target.subjectId}")
    private fun digest(value: String): String = phoneControlHex(MessageDigest.getInstance("SHA-256").digest(value.toByteArray(Charsets.UTF_8)))
    private fun metadataFile(directory: File, id: String) = File(directory, "$id.meta")
    private fun dataFile(directory: File, id: String, version: String) = File(directory, "$id.$version.data")
    private fun revocationFile(directory: File) = File(directory, ".revoked")
    private fun denialFile(directory: File, id: String) = File(directory, ".deny-$id")
    private fun metadataAad(id: String) = "louiselm-offline-meta-v1:$id".toByteArray(Charsets.UTF_8)
    private fun pageAad(id: String, version: String, index: Int) = "louiselm-offline-page-v1:$id:$version:$index".toByteArray(Charsets.UTF_8)

    private fun encrypt(key: SecretKey, plaintext: ByteArray, aad: ByteArray): ByteArray {
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.ENCRYPT_MODE, key)
        cipher.updateAAD(aad)
        require(cipher.iv.size == NONCE_BYTES)
        return cipher.iv + cipher.doFinal(plaintext)
    }

    private fun decrypt(key: SecretKey, encrypted: ByteArray, aad: ByteArray): ByteArray {
        require(encrypted.size >= NONCE_BYTES + TAG_BYTES)
        val cipher = Cipher.getInstance("AES/GCM/NoPadding")
        cipher.init(Cipher.DECRYPT_MODE, key, GCMParameterSpec(128, encrypted, 0, NONCE_BYTES))
        cipher.updateAAD(aad)
        return cipher.doFinal(encrypted, NONCE_BYTES, encrypted.size - NONCE_BYTES)
    }

    private fun ensureDirectory(directory: File) {
        if (Files.isSymbolicLink(directory.toPath())) throw IOException("invalid download path")
        if (!directory.isDirectory && !directory.mkdirs() && !directory.isDirectory) throw IOException("download directory unavailable")
    }

    private fun atomicMove(from: File, to: File) {
        Files.move(from.toPath(), to.toPath(), StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING)
    }

    private fun deleteFile(file: File) {
        if (Files.exists(file.toPath(), LinkOption.NOFOLLOW_LINKS) && !file.delete()) throw IOException("download removal failed")
    }

    private fun hasMarker(file: File) = Files.exists(file.toPath(), LinkOption.NOFOLLOW_LINKS)

    private fun writeMarker(file: File) {
        if (Files.isSymbolicLink(file.toPath())) throw IOException("invalid download path")
        FileOutputStream(file).use {
            it.write(ByteArray(16).also { bytes -> SecureRandom().nextBytes(bytes) })
            it.fd.sync()
        }
    }

    private fun markerValue(file: File): String? {
        if (!hasMarker(file)) return null
        if (Files.isSymbolicLink(file.toPath()) || file.length() != 16L) throw IOException("invalid download fence")
        return phoneControlHex(file.readBytes().also { if (it.size != 16) throw IOException("invalid download fence") })
    }

    private fun purgeDeniedSource(directory: File, id: String) {
        deleteFile(metadataFile(directory, id))
        val files = directory.listFiles() ?: throw IOException("downloads unavailable")
        files.filter { it.name.startsWith("$id.") && it.name.endsWith(".data") }.forEach(::deleteFile)
    }

    private fun purgeRevokedContents(directory: File) {
        if (!hasMarker(revocationFile(directory))) return
        val files = directory.listFiles() ?: throw IOException("downloads unavailable")
        for (file in files) {
            if (file.name != ".revoked") {
                if (file.isDirectory && !Files.isSymbolicLink(file.toPath())) throw IOException("invalid download path")
                deleteFile(file)
            }
        }
    }

    private companion object {
        const val MAGIC = 0x4c4d4331
        const val NONCE_BYTES = 12
        const val TAG_BYTES = 16
        const val META_PLAINTEXT_BYTES = 4096
        const val META_BYTES = META_PLAINTEXT_BYTES + NONCE_BYTES + TAG_BYTES
        const val MAX_PAGE_BYTES = 262_144
        const val MAX_PAGES = 1_000_000
        val METADATA_NAME = Regex("[0-9a-f]{64}\\.meta")
        val DATA_NAME = Regex("[0-9a-f]{64}\\.[0-9a-f]{32}\\.data")
        val DIRECTORY_NAME = Regex("[0-9a-f]{64}")
        val DENIAL_NAME = Regex("\\.deny-[0-9a-f]{64}")
    }
}
