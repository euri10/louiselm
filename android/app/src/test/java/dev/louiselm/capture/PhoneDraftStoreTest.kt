package dev.louiselm.capture

import javax.crypto.spec.SecretKeySpec
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneDraftStoreTest {
    @get:Rule val temporary = TemporaryFolder()
    private val key = SecretKeySpec(ByteArray(32) { 7 }, "AES")
    private val source = PhoneOfflineSource("aa".repeat(32), PhoneControlTarget("11".repeat(16), "22".repeat(16)), "session", "agent", "project")
    private val page = PhoneControlReply.SessionPage(source.target.controllerId, source.target.subjectId, "33".repeat(16), 4, "ready",
        listOf(PhoneControlEntry.Message(1, "assistant", "frozen private context", false)), 0, true,
        PhoneHistorySource("not_applicable", "unavailable", "unknown", false, false))

    @Test fun encryptedDraftSurvivesRestartAndFailedReplacement() {
        val root = temporary.newFolder()
        val original = PhoneDraft(source, "private instruction", listOf(PhoneDraftSelection(source, page)))
        assertEquals(PhoneDraftSave.SAVED, PhoneDraftStore(root).save(original, key) { true })
        assertFalse(root.walk().filter { it.isFile }.any { it.readBytes().toString(Charsets.ISO_8859_1).contains("private") })
        assertEquals(PhoneDraftSave.CANCELLED, PhoneDraftStore(root).save(original.copy(text = "new"), key) { false })
        assertEquals(listOf(original), PhoneDraftStore(root).list(key).drafts)
        assertEquals("frozen private context", (PhoneDraftStore(root).list(key).drafts.single().context.single().page.entries.single() as PhoneControlEntry.Message).text)
    }

    @Test fun confirmationConsumesOnlyTheExactStoredVersion() {
        val root = temporary.newFolder()
        val original = PhoneDraft(source, "reviewed", listOf(PhoneDraftSelection(source, page)))
        val store = PhoneDraftStore(root)
        assertEquals(PhoneDraftSave.SAVED, store.save(original, key) { true })
        val reference = PhoneSubmittedDraft(store.id(source), requireNotNull(store.revision(original, key)))
        assertFalse(store.consume(reference, key) { false })
        val newer = original.copy(text = "reviewed and edited")
        assertEquals(PhoneDraftSave.SAVED, store.save(newer, key) { true })
        assertFalse(PhoneDraftStore(root).consume(reference, key) { true })
        assertEquals(newer, store.list(key).drafts.single())
        assertTrue(store.discard(source, key) { true })
        assertEquals(PhoneDraftSave.SAVED, store.save(original, key) { true })
        assertFalse(store.consume(reference, key) { true })
        val replacement = PhoneSubmittedDraft(store.id(source), requireNotNull(store.revision(original, key)))
        assertTrue(store.consume(replacement, key) { true })
        assertTrue(store.list(key).drafts.isEmpty())
    }

    @Test fun limitsAndCorruptionNeverEvictOtherDrafts() {
        val root = temporary.newFolder()
        val store = PhoneDraftStore(root)
        val first = PhoneDraft(source, "retained", emptyList())
        assertEquals(PhoneDraftSave.SAVED, store.save(first, key) { true })
        assertEquals(PhoneDraftSave.BYTE_LIMIT, store.save(first.copy(text = "x".repeat(8 * 1024 * 1024)), key) { true })
        assertEquals(listOf(first), store.list(key).drafts)
        val another = first.copy(source = source.copy(target = PhoneControlTarget("11".repeat(16), "44".repeat(16))))
        assertEquals(PhoneDraftSave.SAVED, store.save(another, key) { true })
        val oldFile = root.listFiles()!!.single { it.name == "${store.id(first.source)}.draft" }
        oldFile.writeBytes(byteArrayOf(1, 2, 3))
        val listing = store.list(key)
        assertEquals(listOf(another), listing.drafts)
        assertEquals(1, listing.damaged.size)
        assertEquals(PhoneDraftSave.DAMAGED, store.save(first, key) { true })
        assertTrue(oldFile.exists())
        assertEquals(listOf(another), store.list(key).drafts)
    }
    @Test fun exactDraftAndTotalByteLimitsKeepAllSavedRecords() {
        val root = temporary.newFolder()
        val store = PhoneDraftStore(root)
        val first = PhoneDraft(source, "", emptyList())
        assertEquals(PhoneDraftSave.SAVED, store.save(first, key) { true })
        val file = java.io.File(root, "${store.id(source)}.draft")
        val length = 8 * 1024 * 1024 - file.length().toInt()
        val full = "x".repeat(length)
        assertEquals(PhoneDraftSave.SAVED, store.save(first.copy(text = full), key) { true })
        assertEquals(8L * 1024 * 1024, file.length())
        assertEquals(PhoneDraftSave.BYTE_LIMIT, store.save(first.copy(text = full + "x"), key) { true })
        assertEquals(8L * 1024 * 1024, file.length())
        for (index in 1..7) {
            val next = first.copy(source = source.copy(target = PhoneControlTarget(source.target.controllerId, "%032x".format(index))))
            assertEquals(PhoneDraftSave.SAVED, store.save(next.copy(text = full), key) { true })
        }
        val extra = first.copy(source = source.copy(target = PhoneControlTarget(source.target.controllerId, "%032x".format(8))))
        assertEquals(PhoneDraftSave.BYTE_LIMIT, store.save(extra.copy(text = "more"), key) { true })
        assertEquals(8, root.listFiles()!!.count { it.name.endsWith(".draft") })
        assertEquals(8, store.list(key).drafts.size)
        assertEquals(PhoneDraftSave.SAVED, store.save(first.copy(text = "smaller"), key) { true })
        assertEquals(PhoneDraftSave.SAVED, store.save(extra.copy(text = "more"), key) { true })
        assertEquals(9, root.listFiles()!!.count { it.name.endsWith(".draft") })
    }

    @Test fun exactCountLimitRequiresExplicitDiscard() {
        val root = temporary.newFolder()
        val store = PhoneDraftStore(root)
        fun draft(index: Int) = PhoneDraft(source.copy(target = PhoneControlTarget(source.target.controllerId, "%032x".format(index))), "work", emptyList())
        for (index in 1..128) assertEquals(PhoneDraftSave.SAVED, store.save(draft(index), key) { true })
        assertEquals(PhoneDraftSave.COUNT_LIMIT, store.save(draft(129), key) { true })
        assertEquals(128, store.list(key).drafts.size)
        assertTrue(store.discard(draft(1).source, key) { true })
        assertEquals(PhoneDraftSave.SAVED, store.save(draft(129), key) { true })
        assertEquals(128, store.list(key).drafts.size)
    }

    @Test fun malformedUnicodeCannotReplaceTheLastSavedVersion() {
        val store = PhoneDraftStore(temporary.newFolder())
        val original = PhoneDraft(source, "safe", emptyList())
        assertEquals(PhoneDraftSave.SAVED, store.save(original, key) { true })
        assertEquals(PhoneDraftSave.INVALID, store.save(original.copy(text = "bad\uD800"), key) { true })
        assertEquals(listOf(original), store.list(key).drafts)
    }

    @Test fun selectionCountDoesNotImposeASeparateHiddenLimit() {
        val store = PhoneDraftStore(temporary.newFolder())
        val selections = List(1025) { PhoneDraftSelection(source, page) }
        val draft = PhoneDraft(source, "work", selections)
        assertEquals(PhoneDraftSave.SAVED, store.save(draft, key) { true })
        assertEquals(draft, store.list(key).drafts.single())
    }

}
