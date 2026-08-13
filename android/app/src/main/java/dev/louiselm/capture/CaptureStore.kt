package dev.louiselm.capture

import android.content.Context
import android.media.MediaMetadataRetriever
import org.json.JSONObject
import java.io.File
import java.io.FileInputStream
import java.io.FileOutputStream
import java.nio.file.Files
import java.nio.file.StandardCopyOption
import java.security.MessageDigest
import java.util.UUID

internal data class PendingRecording(
    val id: String,
    val file: File,
    val metadata: File,
    val recordedAtMs: Long,
)

internal data class QueueCounts(val waiting: Int, val attention: Int)

internal data class CaptureRecord(
    val id: String,
    val recordedAtMs: Long,
    val durationMs: Long,
    val mimeType: String,
    val bytes: Long,
    val sha256: String,
    val audio: File,
)

internal class CaptureStore(context: Context) {
    private val root = File(context.applicationContext.filesDir, "captures")

    init {
        check(root.mkdirs() || root.isDirectory) { "capture directory could not be created" }
    }

    fun begin(): PendingRecording {
        val id = UUID.randomUUID().toString()
        val pending = PendingRecording(
            id,
            File(root, ".recording-$id.m4a"),
            File(root, ".recording-$id.json"),
            System.currentTimeMillis(),
        )
        writeSynced(
            pending.metadata,
            JSONObject().put("id", pending.id).put("recorded_at_ms", pending.recordedAtMs).toString() + "\n",
        )
        return pending
    }

    fun abort(recording: PendingRecording) {
        recording.file.delete()
        recording.metadata.delete()
    }

    fun complete(recording: PendingRecording, durationMs: Long): CaptureRecord {
        require(durationMs > 0) { "recording duration must be positive" }
        val incoming = File(root, ".incoming-${recording.id}")
        val destination = File(root, recording.id)
        if (destination.isDirectory) {
            recording.metadata.delete()
            return load(destination)
        }
        check(incoming.mkdir() || incoming.isDirectory) { "incoming capture directory could not be created" }
        val audio = File(incoming, AUDIO_NAME)
        if (!audio.isFile) {
            require(recording.file.isFile && recording.file.length() > 0) { "recorder produced no audio" }
            Files.move(recording.file.toPath(), audio.toPath(), StandardCopyOption.ATOMIC_MOVE)
        }
        val record = CaptureRecord(
            id = recording.id,
            recordedAtMs = recording.recordedAtMs,
            durationMs = durationMs,
            mimeType = MIME_TYPE,
            bytes = audio.length(),
            sha256 = sha256(audio),
            audio = audio,
        )
        val manifestFile = File(incoming, MANIFEST_NAME)
        if (!manifestFile.exists()) writeSynced(manifestFile, manifest(record).toString(2) + "\n")
        check(!destination.exists()) { "capture UUID already exists" }
        Files.move(incoming.toPath(), destination.toPath(), StandardCopyOption.ATOMIC_MOVE)
        recording.metadata.delete()
        return record.copy(audio = File(destination, AUDIO_NAME))
    }

    fun recover() {
        root.listFiles()
            .orEmpty()
            .filter { it.isFile && it.name.startsWith(".recording-") && it.name.endsWith(".json") }
            .sortedBy { it.name }
            .forEach { metadata -> runCatching { recover(metadata) } }
    }

    fun pending(): List<CaptureRecord> = root.listFiles()
        .orEmpty()
        .asSequence()
        .filter { it.isDirectory && !it.name.startsWith('.') && !File(it, UPLOADED_NAME).exists() }
        .map(::load)
        .sortedBy { it.id }
        .toList()

    fun markUploaded(id: String) {
        writeState(id, JSONObject().put("status", "uploaded").put("uploaded_at_ms", System.currentTimeMillis()))
        val directory = captureDirectory(id)
        val temporary = File(directory, ".$UPLOADED_NAME")
        writeSynced(temporary, "uploaded\n")
        Files.move(
            temporary.toPath(),
            File(directory, UPLOADED_NAME).toPath(),
            StandardCopyOption.ATOMIC_MOVE,
            StandardCopyOption.REPLACE_EXISTING,
        )
    }

    fun markUploadError(id: String, message: String) {
        writeState(id, JSONObject().put("status", "operator_action").put("error", message.take(300)))
    }

    fun queueCounts(): QueueCounts {
        val waiting = root.listFiles()
            .orEmpty()
            .filter { it.isDirectory && !it.name.startsWith('.') && !File(it, UPLOADED_NAME).exists() }
        val attention = waiting.count { directory ->
            val state = File(directory, STATE_NAME)
            state.isFile && runCatching { JSONObject(state.readText()).optString("status") == "operator_action" }
                .getOrDefault(false)
        }
        return QueueCounts(waiting.size, attention)
    }

    private fun recover(metadata: File) {
        val value = JSONObject(metadata.readText())
        val id = value.getString("id")
        require(runCatching { UUID.fromString(id) }.isSuccess) { "pending capture ID is invalid" }
        val recordedAtMs = value.getLong("recorded_at_ms")
        require(recordedAtMs > 0) { "pending capture time is invalid" }
        val destination = File(root, id)
        if (destination.isDirectory) {
            metadata.delete()
            return
        }
        val recording = File(root, ".recording-$id.m4a")
        val incomingAudio = File(File(root, ".incoming-$id"), AUDIO_NAME)
        val audio = if (recording.isFile) recording else incomingAudio
        if (!audio.isFile || audio.length() == 0L) return
        val durationMs = mediaDurationMs(audio) ?: return
        complete(PendingRecording(id, recording, metadata, recordedAtMs), durationMs)
    }

    private fun load(directory: File): CaptureRecord {
        val value = JSONObject(File(directory, MANIFEST_NAME).readText())
        val id = value.getString("id")
        require(id == directory.name && runCatching { UUID.fromString(id) }.isSuccess) { "capture ID is invalid" }
        val audio = File(directory, AUDIO_NAME)
        require(audio.isFile) { "capture audio is missing" }
        val record = CaptureRecord(
            id = id,
            recordedAtMs = value.getLong("recorded_at_ms"),
            durationMs = value.getLong("duration_ms"),
            mimeType = value.getString("mime_type"),
            bytes = value.getLong("bytes"),
            sha256 = value.getString("sha256"),
            audio = audio,
        )
        require(record.recordedAtMs > 0 && record.durationMs > 0) { "capture metadata is invalid" }
        require(record.mimeType == MIME_TYPE && record.bytes == audio.length()) { "capture audio metadata changed" }
        val recordedDigest = decodeSha256(record.sha256) ?: throw IllegalArgumentException("capture digest is invalid")
        val actualDigest = decodeSha256(sha256(audio)) ?: throw IllegalStateException("computed digest is invalid")
        require(MessageDigest.isEqual(recordedDigest, actualDigest)) {
            "capture audio digest changed"
        }
        return record
    }

    private fun writeState(id: String, value: JSONObject) {
        val directory = captureDirectory(id)
        val target = File(directory, STATE_NAME)
        val temporary = File(directory, ".$STATE_NAME")
        writeSynced(temporary, value.toString(2) + "\n")
        Files.move(
            temporary.toPath(),
            target.toPath(),
            StandardCopyOption.ATOMIC_MOVE,
            StandardCopyOption.REPLACE_EXISTING,
        )
    }

    private fun captureDirectory(id: String): File {
        require(runCatching { UUID.fromString(id) }.isSuccess) { "capture ID is invalid" }
        return File(root, id).also { require(it.isDirectory) { "capture does not exist" } }
    }

    private fun manifest(record: CaptureRecord) = JSONObject()
        .put("schema_version", 1)
        .put("id", record.id)
        .put("source", "android")
        .put("recorded_at_ms", record.recordedAtMs)
        .put("duration_ms", record.durationMs)
        .put("mime_type", record.mimeType)
        .put("bytes", record.bytes)
        .put("sha256", record.sha256)

    companion object {
        private const val MIME_TYPE = "audio/mp4"
        private const val AUDIO_NAME = "audio.m4a"
        private const val MANIFEST_NAME = "capture.json"
        private const val STATE_NAME = "upload.json"
        private const val UPLOADED_NAME = "uploaded"

        private fun writeSynced(file: File, value: String) {
            FileOutputStream(file).use { output ->
                output.write(value.toByteArray(Charsets.UTF_8))
                output.fd.sync()
            }
        }

        private fun sha256(file: File): String {
            val digest = MessageDigest.getInstance("SHA-256")
            FileInputStream(file).use { input ->
                val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
                while (true) {
                    val read = input.read(buffer)
                    if (read < 0) break
                    digest.update(buffer, 0, read)
                }
            }
            return digest.digest().joinToString("") { "%02x".format(it.toInt() and 0xff) }
        }

        private fun mediaDurationMs(file: File): Long? {
            val retriever = MediaMetadataRetriever()
            return try {
                retriever.setDataSource(file.absolutePath)
                retriever.extractMetadata(MediaMetadataRetriever.METADATA_KEY_DURATION)?.toLongOrNull()?.takeIf { it > 0 }
            } finally {
                retriever.release()
            }
        }
    }
}
