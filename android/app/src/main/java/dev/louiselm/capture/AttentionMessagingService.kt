package dev.louiselm.capture

import android.Manifest
import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.content.Context
import android.content.Intent
import android.content.pm.PackageManager
import android.os.Build
import com.google.firebase.messaging.FirebaseMessagingService
import com.google.firebase.messaging.RemoteMessage
import java.io.IOException

internal const val ATTENTION_CHANNEL = "louiselm-attention"
internal const val ATTENTION_INBOX_ACTION = "dev.louiselm.capture.ATTENTION_INBOX"

/** Firebase callbacks run off the main thread. Network work belongs to WorkManager, not this service. */
class AttentionMessagingService : FirebaseMessagingService() {
    @Suppress("OVERRIDE_DEPRECATION") // Paired receiver still addresses supported FCM tokens, not Installation IDs.
    override fun onNewToken(token: String) {
        // The worker obtains the current SDK token; never persist/log a possibly superseded callback token.
        try {
            AttentionWorker.enqueue(applicationContext)
        } catch (_: IOException) {
            // Durable state was not readable; opening the app retries registration. No token is acknowledged.
        }
    }

    override fun onMessageReceived(message: RemoteMessage) {
        if (message.notification != null) return
        val generation = attentionWakeGeneration(message.data) ?: return
        val state = AttentionState(applicationContext)
        try {
            val owner = state.readyOwner() ?: return
            state.deliver(owner, generation) { showAttentionNotification(applicationContext) }
            AttentionWorker.enqueue(applicationContext, state)
        } catch (_: IOException) {
            // Fail closed on durable-state failure. Opening the inbox still fetches authoritative state.
        }
    }
}

/** Fixed app-owned notification, without inbox content, identifiers, or remote Intent extras. */
internal fun showAttentionNotification(context: Context): Boolean {
    if (Build.VERSION.SDK_INT >= 33 &&
        context.checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
    ) return false
    val manager = context.getSystemService(NotificationManager::class.java)
    manager.createNotificationChannel(NotificationChannel(
        ATTENTION_CHANNEL,
        context.getString(R.string.attention_channel),
        NotificationManager.IMPORTANCE_DEFAULT,
    ).apply { lockscreenVisibility = Notification.VISIBILITY_PRIVATE })
    if (!manager.areNotificationsEnabled() || manager.getNotificationChannel(ATTENTION_CHANNEL).importance == NotificationManager.IMPORTANCE_NONE) {
        return false
    }
    val intent = Intent(context, MainActivity::class.java)
        .setAction(ATTENTION_INBOX_ACTION)
        .addFlags(Intent.FLAG_ACTIVITY_CLEAR_TOP or Intent.FLAG_ACTIVITY_SINGLE_TOP)
    val notification = Notification.Builder(context, ATTENTION_CHANNEL)
        .setSmallIcon(R.drawable.ic_attention)
        .setContentTitle(context.getString(R.string.attention_notification_title))
        .setContentText(context.getString(R.string.attention_notification_body))
        .setVisibility(Notification.VISIBILITY_PRIVATE)
        .setAutoCancel(true)
        .setContentIntent(PendingIntent.getActivity(context, 0, intent, PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT))
        .build()
    return try {
        manager.notify(ATTENTION_CHANNEL, 1, notification)
        true
    } catch (_: SecurityException) {
        false // Permission can be revoked between the check and notify; do not consume the generation.
    }
}
