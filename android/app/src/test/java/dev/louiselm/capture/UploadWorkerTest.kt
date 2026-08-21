package dev.louiselm.capture

import androidx.work.ExistingWorkPolicy
import org.junit.Assert.assertEquals
import org.junit.Test

class UploadWorkerTest {
    @Test
    fun manualSyncReplacesBackedOffUploadWork() {
        assertEquals(ExistingWorkPolicy.REPLACE, uploadWorkPolicy(manual = true))
        assertEquals(ExistingWorkPolicy.APPEND_OR_REPLACE, uploadWorkPolicy(manual = false))
    }
}
