package dev.louiselm.capture

import android.content.Context
import androidx.work.Constraints
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import androidx.work.workDataOf

internal class UploadWorker(context: Context, parameters: WorkerParameters) : Worker(context, parameters) {
    override fun doWork(): Result {
        val pairing = runCatching { PairingStore(applicationContext).load() }
            .getOrElse { return Result.failure(workDataOf("error" to "pairing credential needs attention")) }
            ?: return Result.success()
        val captures = runCatching {
            CaptureStore(applicationContext).run {
                recover()
                pending()
            }
        }
            .getOrElse { return Result.failure(workDataOf("error" to "capture queue needs attention")) }
        val store = CaptureStore(applicationContext)
        for (capture in captures) {
            when (val attempt = PinnedHttps.upload(pairing, capture)) {
                UploadAttempt.Success -> store.markUploaded(capture.id)
                is UploadAttempt.Retry -> return Result.retry()
                is UploadAttempt.OperatorAction -> {
                    store.markUploadError(capture.id, attempt.message)
                    return Result.failure(workDataOf("error" to attempt.message))
                }
            }
        }
        return Result.success()
    }

    companion object {
        private const val UNIQUE_WORK = "capture-upload"

        fun enqueue(context: Context, manual: Boolean = false) {
            val constraints = Constraints.Builder()
                .setRequiredNetworkType(NetworkType.CONNECTED)
                .build()
            val request = OneTimeWorkRequestBuilder<UploadWorker>()
                .setConstraints(constraints)
                .build()
            WorkManager.getInstance(context.applicationContext)
                .enqueueUniqueWork(UNIQUE_WORK, uploadWorkPolicy(manual), request)
        }
    }
}

internal fun uploadWorkPolicy(manual: Boolean): ExistingWorkPolicy = if (manual) {
    ExistingWorkPolicy.REPLACE
} else {
    ExistingWorkPolicy.APPEND_OR_REPLACE
}
