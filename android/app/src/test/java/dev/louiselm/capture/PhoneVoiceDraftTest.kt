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
}
