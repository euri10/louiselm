package dev.louiselm.capture

import android.content.Context
import androidx.work.BackoffPolicy
import androidx.work.Constraints
import androidx.work.Data
import androidx.work.ExistingWorkPolicy
import androidx.work.NetworkType
import androidx.work.OneTimeWorkRequestBuilder
import androidx.work.WorkManager
import androidx.work.Worker
import androidx.work.WorkerParameters
import com.google.android.gms.tasks.Tasks
import com.google.firebase.installations.FirebaseInstallations
import com.google.firebase.messaging.FirebaseMessaging
import org.json.JSONException
import java.io.IOException
import java.util.concurrent.ExecutionException
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/** Durable, network-constrained installation registration and private inbox refresh. */
class AttentionWorker(context: Context, parameters: WorkerParameters) : Worker(context, parameters) {
    override fun doWork(): Result {
        if (!BuildConfig.FIREBASE_ENABLED) return Result.success()
        val expected = inputData.getString(OWNER) ?: return Result.failure()
        val state = AttentionState(applicationContext)
        try {
            if (state.owner() != expected || isStopped) return Result.success()
            if (!hasReceiverNetworkAccess(applicationContext)) return Result.failure()
            val pairing = PairingStore(applicationContext).load() ?: return Result.success()
            val outcome = syncAttention(state, expected, { reset ->
                currentReceiverInstallation(reset, inputData.getString(REGISTERED_FID))
            }, { fid ->
                if (isStopped) UploadAttempt.Retry("work was cancelled") else PinnedHttps.registerAttentionInstallation(pairing, fid)
            }, {
                if (isStopped) AttentionFetch.Retry("work was cancelled") else PinnedHttps.fetchAttention(pairing)
            })
            if (PairingStore(applicationContext).binding() == expected) {
                // Only a successful paired registration authorizes SDK background maintenance.
                FirebaseMessaging.getInstance().isAutoInitEnabled = state.readyOwner() == expected
            }
            return when (outcome) {
                UploadAttempt.Success -> Result.success()
                is UploadAttempt.Retry -> Result.retry()
                is UploadAttempt.OperatorAction -> Result.failure()
            }
        } catch (_: InterruptedException) {
            Thread.currentThread().interrupt()
            return Result.retry()
        } catch (_: ExecutionException) {
            return Result.retry() // Firebase transport failed; do not log credential-bearing causes.
        } catch (_: TimeoutException) {
            return Result.retry()
        } catch (_: IOException) {
            return Result.retry()
        } catch (_: SecurityException) {
            return Result.failure()
        } catch (_: IllegalArgumentException) {
            return Result.failure()
        } catch (_: JSONException) {
            return Result.failure()
        }
    }

    companion object {
        internal const val UNIQUE_WORK = "attention-refresh"
        private const val OWNER = "pairing-owner"
        private const val REGISTERED_FID = "registered-fid"

        /** Caller is background-threaded; a missing opt-in/pairing/configuration queues nothing. */
        internal fun enqueue(
            context: Context,
            state: AttentionState = AttentionState(context),
            replaceExisting: Boolean = false,
            registeredFid: String? = null,
        ) {
            if (!BuildConfig.FIREBASE_ENABLED) return
            val owner = state.owner() ?: return
            val request = OneTimeWorkRequestBuilder<AttentionWorker>()
                .setInputData(Data.Builder().putString(OWNER, owner).putString(REGISTERED_FID, registeredFid).build())
                .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
                .build()
            // Append so SDK refresh during an in-flight fetch cannot cancel/lose its registration.
            val policy = if (replaceExisting) ExistingWorkPolicy.REPLACE else ExistingWorkPolicy.APPEND_OR_REPLACE
            WorkManager.getInstance(context).enqueueUniqueWork(UNIQUE_WORK, policy, request)
        }
    }
}

/** Background SDK boundary. Callback work skips register only for the exact current FID. */
internal fun currentReceiverInstallation(reset: Boolean, registeredFid: String?): String {
    val messaging = FirebaseMessaging.getInstance()
    val installations = FirebaseInstallations.getInstance()
    if (reset) {
        messaging.isAutoInitEnabled = false
        Tasks.await(messaging.unregister(), 30, TimeUnit.SECONDS)
        // unregister retains the FID. Delete it so the previous pairing cannot address this one.
        Tasks.await(installations.delete(), 30, TimeUnit.SECONDS)
    }
    val fid = Tasks.await(installations.id, 30, TimeUnit.SECONDS)
    require(validAttentionInstallation(fid)) { "invalid notification installation ID" }
    if (reset || registeredFid != fid) {
        // register always invokes onRegistered, including for an unchanged FID.
        Tasks.await(messaging.register(), 30, TimeUnit.SECONDS)
    }
    if (Tasks.await(installations.id, 30, TimeUnit.SECONDS) != fid) {
        throw IOException("notification installation changed during registration")
    }
    return fid
}

/** Synchronous worker flow with owner checks at each asynchronous external boundary. */
internal fun syncAttention(
    state: AttentionState,
    expected: String,
    installation: (Boolean) -> String,
    register: (String) -> UploadAttempt,
    fetch: () -> AttentionFetch,
): UploadAttempt {
    if (state.owner() != expected) return UploadAttempt.Success
    val fid = installation(state.needsInstallationReset(expected))
    if (!state.installationPrepared(expected)) return UploadAttempt.Success
    when (val registration = register(fid)) {
        is UploadAttempt.Retry -> return registration
        is UploadAttempt.OperatorAction -> {
            state.block(expected)
            return registration
        }
        UploadAttempt.Success -> if (!state.registered(expected)) return UploadAttempt.Success
    }
    return when (val inbox = fetch()) {
        is AttentionFetch.Success -> {
            state.cache(expected, inbox.snapshot)
            UploadAttempt.Success
        }
        is AttentionFetch.Retry -> UploadAttempt.Retry(inbox.message)
        is AttentionFetch.OperatorAction -> {
            state.block(expected)
            UploadAttempt.OperatorAction(inbox.message)
        }
    }
}
