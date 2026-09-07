package dev.louiselm.capture

import android.Manifest
import android.annotation.SuppressLint
import android.os.Build
import androidx.work.Configuration
import androidx.work.Data
import androidx.work.ListenableWorker
import androidx.work.WorkerParameters
import androidx.work.impl.utils.taskexecutor.WorkManagerTaskExecutor
import androidx.work.workDataOf
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config
import java.util.UUID
import java.util.concurrent.Executor
import kotlin.coroutines.EmptyCoroutineContext

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28, 34, 37])
class ReceiverNetworkTest {
    @Test
    @Config(sdk = [37])
    @SuppressLint("RestrictedApi") // Construct real Worker parameters without a scheduler or receiver.
    fun backgroundUploadReportsMissingPermissionBeforeReadingPairing() {
        val application = RuntimeEnvironment.getApplication()
        val executor = Executor { it.run() }
        val parameters = WorkerParameters(
            UUID.randomUUID(), Data.EMPTY, emptyList(), WorkerParameters.RuntimeExtras(), 0, 0,
            executor, EmptyCoroutineContext, WorkManagerTaskExecutor(executor),
            Configuration.Builder().build().workerFactory,
            { _, _, _ -> throw AssertionError("Permission refusal must not publish progress") },
            { _, _, _ -> throw AssertionError("Permission refusal must not start foreground work") },
        )
        assertEquals(
            ListenableWorker.Result.failure(workDataOf("error" to application.getString(R.string.local_network_denied))),
            UploadWorker(application, parameters).doWork(),
        )
    }

    @Test
    fun receiverChecksFollowCurrentPermissionIncludingRevocation() {
        val application = RuntimeEnvironment.getApplication()
        if (Build.VERSION.SDK_INT < 37) {
            assertTrue(hasReceiverNetworkAccess(application))
            requireReceiverNetworkAccess(application)
            return
        }
        assertFalse(hasReceiverNetworkAccess(application))
        assertThrows(SecurityException::class.java) { requireReceiverNetworkAccess(application) }
        shadowOf(application).grantPermissions(Manifest.permission.ACCESS_LOCAL_NETWORK)
        assertTrue(hasReceiverNetworkAccess(application))
        requireReceiverNetworkAccess(application)
        shadowOf(application).denyPermissions(Manifest.permission.ACCESS_LOCAL_NETWORK)
        assertFalse(hasReceiverNetworkAccess(application))
        assertThrows(SecurityException::class.java) { requireReceiverNetworkAccess(application) }
    }
}
