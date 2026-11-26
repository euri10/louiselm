package dev.louiselm.capture

import java.io.File
import android.media.MediaMetadataRetriever
import android.os.Build
import javax.crypto.spec.SecretKeySpec
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.shadows.ShadowMediaMetadataRetriever

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneVoiceStoreTest {
    @Test fun cancellationAndRetryIntentsSurviveRestartAndCannotReopenADiscardedJob() {
        val context = RuntimeEnvironment.getApplication()
        val key = SecretKeySpec(ByteArray(32) { 9 }, "AES")
        val source = PhoneOfflineSource("aa".repeat(32), PhoneControlTarget("11".repeat(16), "22".repeat(16)), "session", "agent", "project")
        val other = source.copy(target = PhoneControlTarget("11".repeat(16), "33".repeat(16)))
        val drafts = PhoneDraftStore(File(context.filesDir, "phone-drafts-v1"))
        val store = PhoneVoiceStore(context) { key }
        fun capture(destination: PhoneOfflineSource): String {
            val pending = store.begin(destination, drafts.id(destination), "33".repeat(32))
            pending.recording.file.writeBytes(byteArrayOf(1, 2, 3))
            store.finish(pending, 1, true)
            return pending.recording.id
        }
        val first = capture(source)
        val second = capture(other)
        assertTrue(store.markSubmitted(first) { true })
        val operation = "55".repeat(16)
        assertTrue(store.requestRetry(first, operation) { true })
        assertEquals(operation, PhoneVoiceStore(context) { key }.list().records.single { it.id == first }.retryOperation)
        assertFalse(store.requestCancellationForDraft(source) { false })
        assertTrue(store.requestCancellationForDraft(source) { true })
        val reopened = PhoneVoiceStore(context) { key }
        assertEquals(PhoneVoiceIntent.CANCEL_PENDING, reopened.list().records.single { it.id == first }.intent)
        assertEquals(PhoneVoiceIntent.REQUESTED, reopened.list().records.single { it.id == second }.intent)
        assertFalse(reopened.markSubmitted(first) { true })
        assertFalse(reopened.markRetried(first, operation) { true })
        assertTrue(reopened.markCancelled(first) { true })
        assertEquals(PhoneVoiceIntent.CANCELLED, PhoneVoiceStore(context) { key }.list().records.single { it.id == first }.intent)
        assertArrayEquals(byteArrayOf(1, 2, 3), requireNotNull(reopened.list().records.single { it.id == first }.audio).audio.readBytes())
    }

    @Test @Config(sdk = [30, 34, 35, 37])
    fun metadataKeyAvoidsOlderAndroidLockScreenDeletion() {
        val spec = phoneVoiceMetadataKeySpec()
        assertEquals(Build.VERSION.SDK_INT >= 35, spec.isUnlockedDeviceRequired)
        assertEquals(256, spec.keySize)
    }

    @Test fun validEscapedSourceLabelsRemainReadable() {
        val context = RuntimeEnvironment.getApplication()
        val key = SecretKeySpec(ByteArray(32) { 9 }, "AES")
        val source = PhoneOfflineSource("aa".repeat(32), PhoneControlTarget("11".repeat(16), "22".repeat(16)),
            "\u0001".repeat(512), "\u0002".repeat(512), "\u0003".repeat(512))
        val store = PhoneVoiceStore(context) { key }
        val pending = store.begin(source, PhoneDraftStore(File(context.filesDir, "phone-drafts-v1")).id(source), "33".repeat(32))
        pending.recording.file.writeBytes(byteArrayOf(1))
        store.finish(pending, 1, true)
        assertEquals(source, store.list().records.single().source)
    }

    @Test fun voiceHistoryPagesWithoutDroppingOriginals() {
        val context = RuntimeEnvironment.getApplication()
        val key = SecretKeySpec(ByteArray(32) { 9 }, "AES")
        val source = PhoneOfflineSource("aa".repeat(32), PhoneControlTarget("11".repeat(16), "22".repeat(16)), "session", "agent", "project")
        val store = PhoneVoiceStore(context) { key }
        val draftId = PhoneDraftStore(File(context.filesDir, "phone-drafts-v1")).id(source)
        repeat(51) {
            val pending = store.begin(source, draftId, "33".repeat(32))
            pending.recording.file.writeBytes(byteArrayOf(1))
            store.finish(pending, 1, requested = true)
        }
        val first = store.list()
        val second = store.list(requireNotNull(first.next))
        assertEquals(50, first.records.size)
        assertEquals(1, second.records.size)
        assertEquals(null, second.next)
        assertEquals(51, (first.records + second.records).map { it.id }.toSet().size)
        assertTrue((first.records + second.records).all { it.audio != null })
    }

    @Test fun restartRecoversFinalizedAudioButDoesNotPublishAnActiveRecorder() {
        val context = RuntimeEnvironment.getApplication()
        val key = SecretKeySpec(ByteArray(32) { 9 }, "AES")
        val source = PhoneOfflineSource("aa".repeat(32), PhoneControlTarget("11".repeat(16), "22".repeat(16)), "session", "agent", "project")
        val store = PhoneVoiceStore(context) { key }
        val pending = store.begin(source, PhoneDraftStore(File(context.filesDir, "phone-drafts-v1")).id(source), "33".repeat(32))
        pending.recording.file.writeBytes(byteArrayOf(7, 8, 9))
        ShadowMediaMetadataRetriever.addMetadata(pending.recording.file.absolutePath, MediaMetadataRetriever.METADATA_KEY_DURATION, "23")
        assertEquals(null, PhoneVoiceStore(context) { key }.list().records.single().audio)
        pending.lease.close() // Model the process-owned identity disappearing on process death.
        val recovered = PhoneVoiceStore(context) { key }.list().records.single()
        assertEquals(PhoneVoiceIntent.UNDECIDED, recovered.intent)
        assertArrayEquals(byteArrayOf(7, 8, 9), requireNotNull(recovered.audio).audio.readBytes())
        assertEquals(source, recovered.source)
    }

    @Test fun stoppedAndInterruptedOriginalsRemainSeparateAndAssociatedAfterRestart() {
        val context = RuntimeEnvironment.getApplication()
        val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        val voiceKey = SecretKeySpec(ByteArray(32) { 9 }, "AES")
        val source = PhoneOfflineSource("aa".repeat(32), PhoneControlTarget("11".repeat(16), "22".repeat(16)), "private session", "agent", "project")
        val draft = PhoneDraftStore(File(context.filesDir, "phone-drafts-v1"))
        assertEquals(PhoneDraftSave.SAVED, draft.save(PhoneDraft(source, "", emptyList()), key) { true })
        val revision = requireNotNull(draft.revision(PhoneDraft(source, "", emptyList()), key))
        val store = PhoneVoiceStore(context) { voiceKey }
        val first = store.begin(source, draft.id(source), revision)
        first.recording.file.writeBytes(byteArrayOf(1, 2, 3))
        store.finish(first, 10, requested = true)
        val second = store.begin(source, draft.id(source), revision)
        second.recording.file.writeBytes(byteArrayOf(4, 5, 6))
        store.finish(second, 11, requested = false)

        val reopened = PhoneVoiceStore(context) { voiceKey }
        val records = reopened.list().records.associateBy { it.id }
        val stopped = requireNotNull(records[first.recording.id])
        val interrupted = requireNotNull(records[second.recording.id])
        assertEquals(source, stopped.source)
        assertEquals(draft.id(source), stopped.draftId)
        assertEquals(revision, stopped.draftRevision)
        assertEquals(PhoneVoiceIntent.REQUESTED, stopped.intent)
        assertEquals(PhoneVoiceIntent.UNDECIDED, interrupted.intent)
        assertArrayEquals(byteArrayOf(1, 2, 3), requireNotNull(stopped.audio).audio.readBytes())
        assertArrayEquals(byteArrayOf(4, 5, 6), requireNotNull(interrupted.audio).audio.readBytes())
        assertEquals(0, CaptureStore(context).queueStatus().pending)
        assertFalse(File(context.filesDir, "phone-voice-v1").walk().filter { it.isFile }
            .any { it.readBytes().toString(Charsets.ISO_8859_1).contains("private session") })

        assertFalse(reopened.choose(interrupted.id, true) { false })
        assertTrue(reopened.choose(interrupted.id, true) { true })
        assertEquals(PhoneVoiceIntent.REQUESTED, reopened.list().records.single { it.id == interrupted.id }.intent)
        assertArrayEquals(byteArrayOf(4, 5, 6), requireNotNull(reopened.list().records.single { it.id == interrupted.id }.audio).audio.readBytes())
        draft.resetLost()
        draft.installKey(SecretKeySpec(ByteArray(32) { 8 }, "AES"))
        assertEquals(source, reopened.list().records.single { it.id == interrupted.id }.source)
        File(context.filesDir, "phone-voice-v1/11111111-1111-4111-8111-111111111111.voice").writeBytes(byteArrayOf(1, 2, 3))
        File(context.filesDir, "phone-voice-v1/22222222-2222-4222-8222-222222222222.voice").writeBytes(ByteArray(16 * 1024 + 1))
        val isolated = reopened.list()
        assertEquals(2, isolated.damaged)
        assertEquals(2, isolated.records.size)
    }
}
