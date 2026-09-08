package dev.louiselm.capture

import androidx.work.ExistingWorkPolicy
import androidx.work.WorkInfo
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class UploadWorkerTest {
    @Test
    fun receiverMigrationReplacesExistingUploadWork() {
        assertTrue(shouldReplaceUploadWorkAfterPairing(PairingTransition.RECEIVER_MIGRATION))
        assertFalse(shouldReplaceUploadWorkAfterPairing(PairingTransition.FIRST_PAIR))
        assertFalse(shouldReplaceUploadWorkAfterPairing(PairingTransition.ENDPOINT_UPDATE))
    }

    @Test
    fun credentialRenewalRetriesPendingWorkImmediately() {
        assertTrue(shouldReplaceUploadWorkAfterPairing(PairingTransition.CREDENTIAL_RENEWAL))
    }

    @Test
    fun explicitReplacementReplacesBackedOffUploadWork() {
        assertEquals(ExistingWorkPolicy.REPLACE, uploadWorkPolicy(replaceExisting = true))
        assertEquals(ExistingWorkPolicy.APPEND_OR_REPLACE, uploadWorkPolicy(replaceExisting = false))
    }

    @Test
    fun uploadStatusRefreshesOnlyAfterWorkReachesATerminalState() {
        assertFalse(hasFinishedUploadState(emptyList()))
        assertFalse(hasFinishedUploadState(listOf(WorkInfo.State.ENQUEUED)))
        assertFalse(hasFinishedUploadState(listOf(WorkInfo.State.RUNNING)))
        assertTrue(hasFinishedUploadState(listOf(WorkInfo.State.SUCCEEDED)))
        assertTrue(hasFinishedUploadState(listOf(WorkInfo.State.FAILED)))
        assertTrue(hasFinishedUploadState(listOf(WorkInfo.State.CANCELLED)))
    }
}
