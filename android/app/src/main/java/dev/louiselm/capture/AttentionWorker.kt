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
import com.google.firebase.messaging.FirebaseMessaging
import org.json.JSONException
import java.io.IOException
import java.util.concurrent.ExecutionException
import java.util.concurrent.TimeUnit
import java.util.concurrent.TimeoutException

/** Durable, network-constrained token registration and private inbox refresh; never displays remote text. */
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
                currentReceiverToken(reset)
            }, { token ->
                if (isStopped) UploadAttempt.Retry("work was cancelled") else PinnedHttps.registerAttentionToken(pairing, token)
            }, {
                if (isStopped) AttentionFetch.Retry("work was cancelled") else PinnedHttps.fetchAttention(pairing)
            })
            if (PairingStore(applicationContext).binding() == expected) {
                // Only a successful paired registration authorizes SDK background token maintenance.
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
            return Result.retry() // Firebase token transport failed; do not log credential-bearing causes.
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

        /** Caller is background-threaded; a missing opt-in/pairing/configuration queues nothing. */
        internal fun enqueue(
            context: Context,
            state: AttentionState = AttentionState(context),
            replaceExisting: Boolean = false,
        ) {
            if (!BuildConfig.FIREBASE_ENABLED) return
            val owner = state.owner() ?: return
            val request = OneTimeWorkRequestBuilder<AttentionWorker>()
                .setInputData(Data.Builder().putString(OWNER, owner).build())
                .setConstraints(Constraints.Builder().setRequiredNetworkType(NetworkType.CONNECTED).build())
                .setBackoffCriteria(BackoffPolicy.EXPONENTIAL, 30, TimeUnit.SECONDS)
                .build()
            // Append so token refresh during an in-flight fetch cannot cancel/lose its registration.
            val policy = if (replaceExisting) ExistingWorkPolicy.REPLACE else ExistingWorkPolicy.APPEND_OR_REPLACE
            WorkManager.getInstance(context).enqueueUniqueWork(UNIQUE_WORK, policy, request)
        }
    }
}

// The existing receiver's HTTP v1 contract addresses FCM tokens, not Firebase Installation IDs.
// Keep the supported token mode explicit until client and sender migrate together.
@Suppress("DEPRECATION")
private fun currentReceiverToken(reset: Boolean): String {
    val messaging = FirebaseMessaging.getInstance()
    if (reset) {
        messaging.isAutoInitEnabled = false
        Tasks.await(messaging.deleteToken(), 30, TimeUnit.SECONDS)
    }
    return Tasks.await(messaging.token, 30, TimeUnit.SECONDS)
}

/** Synchronous worker flow with owner checks at each asynchronous external boundary. */
internal fun syncAttention(
    state: AttentionState,
    expected: String,
    token: (Boolean) -> String,
    register: (String) -> UploadAttempt,
    fetch: () -> AttentionFetch,
): UploadAttempt {
    if (state.owner() != expected) return UploadAttempt.Success
    val currentToken = token(state.needsTokenReset(expected))
    if (!state.tokenPrepared(expected)) return UploadAttempt.Success
    when (val registration = register(currentToken)) {
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
