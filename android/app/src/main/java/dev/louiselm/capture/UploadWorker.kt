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
        val store = CaptureStore(applicationContext)
        val captures = runCatching {
            store.run {
                recover(pairing.receiverIdentitySha256)
                pendingFor(pairing.receiverIdentitySha256)
            }
        }
            .getOrElse { return Result.failure(workDataOf("error" to "capture queue needs attention")) }
        for (capture in captures) {
            when (val attempt = PinnedHttps.upload(pairing, capture)) {
                UploadAttempt.Success -> {
                    if (!store.markUploaded(capture.id, pairing.receiverIdentitySha256)) return Result.success()
                }
                is UploadAttempt.Retry -> return Result.retry()
                is UploadAttempt.OperatorAction -> {
                    if (!store.markUploadError(capture.id, pairing.receiverIdentitySha256, attempt.message)) {
                        return Result.success()
                    }
                    return Result.failure(workDataOf("error" to attempt.message))
                }
            }
        }
        return Result.success()
    }

    companion object {
        internal const val UNIQUE_WORK = "capture-upload"

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

internal fun hasFinishedUpload(workInfos: List<androidx.work.WorkInfo>): Boolean =
    hasFinishedUploadState(workInfos.map { it.state })

internal fun hasFinishedUploadState(states: List<androidx.work.WorkInfo.State>): Boolean =
    states.any { it.isFinished }

internal fun uploadWorkPolicy(manual: Boolean): ExistingWorkPolicy = if (manual) {
    ExistingWorkPolicy.REPLACE
} else {
    ExistingWorkPolicy.APPEND_OR_REPLACE
}
