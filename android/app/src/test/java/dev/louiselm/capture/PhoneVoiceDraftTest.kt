package dev.louiselm.capture

import javax.crypto.spec.SecretKeySpec
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneVoiceDraftTest {
    @get:Rule val temporary = TemporaryFolder()

    @Test fun savedRevisionChangesAfterEditOrDiscardAndRecreation() {
        val root = temporary.newFolder()
        val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        val source = PhoneOfflineSource("aa".repeat(32), PhoneControlTarget("11".repeat(16), "22".repeat(16)), "session", "agent", "project")
        val draft = PhoneDraft(source, "", emptyList())
        val store = PhoneDraftStore(root)
        assertNull(store.revision(draft, key))
        assertEquals(PhoneDraftSave.SAVED, store.save(draft, key) { true })
        val recorded = store.revision(draft, key)
        assertEquals(recorded, PhoneDraftStore(root).revision(draft, key))
        assertEquals(PhoneDraftSave.SAVED, store.save(draft.copy(text = "newer"), key) { true })
        assertNull(store.revision(draft, key))
        assertNotEquals(recorded, store.revision(draft.copy(text = "newer"), key))
        assertEquals(true, store.discard(source, key) { true })
        assertNull(store.revision(draft, key))
        assertEquals(PhoneDraftSave.SAVED, store.save(draft, key) { true })
        assertNotEquals(recorded, store.revision(draft, key))
    }

    @Test fun transcriptOnlyFillsTheExactUntouchedEmptyDraft() {
        val root = temporary.newFolder()
        val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        val source = PhoneOfflineSource("aa".repeat(32), PhoneControlTarget("11".repeat(16), "22".repeat(16)), "session", "agent", "project")
        val empty = PhoneDraft(source, "", emptyList())
        val store = PhoneDraftStore(root)
        assertEquals(PhoneDraftSave.SAVED, store.save(empty, key) { true })
        val revision = requireNotNull(store.revision(empty, key))

        assertEquals(PhoneDraftSave.CANCELLED, store.fillVoiceTranscript(source, revision, "original transcript", key) { false })
        assertEquals(listOf(empty), store.list(key).drafts)
        var checks = 0
        assertEquals(PhoneDraftSave.CANCELLED, store.fillVoiceTranscript(source, revision, "original transcript", key) { ++checks == 1 })
        assertEquals(listOf(empty), store.list(key).drafts)
        assertEquals(PhoneDraftSave.CONFLICT, store.fillVoiceTranscript(source.copy(project = "other"), revision, "wrong source", key) { true })
        assertEquals(PhoneDraftSave.SAVED, store.fillVoiceTranscript(source, revision, "original transcript", key) { true })
        assertEquals(listOf(empty.copy(text = "original transcript")), store.list(key).drafts)
        assertEquals(PhoneDraftSave.CONFLICT, store.fillVoiceTranscript(source, revision, "duplicate", key) { true })

        assertEquals(PhoneDraftSave.SAVED, store.save(empty, key) { true })
        val beforeEdit = requireNotNull(store.revision(empty, key))
        assertEquals(PhoneDraftSave.SAVED, store.save(empty.copy(text = "newer work"), key) { true })
        assertEquals(PhoneDraftSave.CONFLICT, store.fillVoiceTranscript(source, beforeEdit, "late", key) { true })
        assertEquals(listOf(empty.copy(text = "newer work")), store.list(key).drafts)
        assertEquals(true, store.discard(source, key) { true })
        assertEquals(PhoneDraftSave.CONFLICT, store.fillVoiceTranscript(source, beforeEdit, "late", key) { true })
        assertEquals(emptyList<PhoneDraft>(), store.list(key).drafts)
        assertEquals(PhoneDraftSave.SAVED, store.save(empty, key) { true })
        assertEquals(PhoneDraftSave.CONFLICT, store.fillVoiceTranscript(source, beforeEdit, "late", key) { true })
    }

    @Test fun transcriptCannotBypassDraftLimitsOrReplaceNonemptyText() {
        val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        val source = PhoneOfflineSource("aa".repeat(32), PhoneControlTarget("11".repeat(16), "22".repeat(16)), "session", "agent", "project")
        val store = PhoneDraftStore(temporary.newFolder())
        val draft = PhoneDraft(source, "", emptyList())
        assertEquals(PhoneDraftSave.SAVED, store.save(draft, key) { true })
        val revision = requireNotNull(store.revision(draft, key))
        assertEquals(PhoneDraftSave.BYTE_LIMIT, store.fillVoiceTranscript(source, revision, "x".repeat(8 * 1024 * 1024), key) { true })
        assertEquals(listOf(draft), store.list(key).drafts)
        val edited = draft.copy(text = "existing")
        assertEquals(PhoneDraftSave.SAVED, store.save(edited, key) { true })
        val editedRevision = requireNotNull(store.revision(edited, key))
        assertEquals(PhoneDraftSave.CONFLICT, store.fillVoiceTranscript(source, editedRevision, "transcript", key) { true })
        assertEquals(listOf(edited), store.list(key).drafts)
    }

    @Test fun transcriptFillPreservesSelectedContext() {
        val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
        val source = PhoneOfflineSource("aa".repeat(32), PhoneControlTarget("11".repeat(16), "22".repeat(16)), "session", "agent", "project")
        val page = PhoneControlReply.SessionPage(source.target.controllerId, source.target.subjectId, "33".repeat(16), 4, "ready",
            listOf(PhoneControlEntry.Message(1, "assistant", "frozen context", false)), 0, true,
            PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false))
        val draft = PhoneDraft(source, "", listOf(PhoneDraftSelection(source, page)))
        val store = PhoneDraftStore(temporary.newFolder())
        assertEquals(PhoneDraftSave.SAVED, store.save(draft, key) { true })
        val revision = requireNotNull(store.revision(draft, key))
        assertEquals(PhoneDraftSave.SAVED, store.fillVoiceTranscript(source, revision, "review me", key) { true })
        assertEquals(listOf(draft.copy(text = "review me")), store.list(key).drafts)
    }
}
