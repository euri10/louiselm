package dev.louiselm.capture

import android.Manifest
import android.app.Notification
import android.app.NotificationManager
import android.content.Context
import com.google.firebase.messaging.RemoteMessage
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28, 34, 37], qualifiers = "en")
class AttentionNotificationTest {
    @Test
    @Config(sdk = [34, 37])
    fun deniedRuntimePermissionNeverDisplaysANotification() {
        val context = RuntimeEnvironment.getApplication()
        shadowOf(context).denyPermissions(Manifest.permission.POST_NOTIFICATIONS)
        assertFalse(showAttentionNotification(context))
        assertEquals(0, shadowOf(context.getSystemService(NotificationManager::class.java)).size())
        shadowOf(context).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
        assertTrue(showAttentionNotification(context))
    }

    @Test
    fun fixedPrivateNotificationRoutesToInboxAndChannelRespectsDenial() {
        val context = RuntimeEnvironment.getApplication()
        shadowOf(context).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
        val manager = context.getSystemService(NotificationManager::class.java)
        assertTrue(showAttentionNotification(context))
        val notification = shadowOf(manager).allNotifications.single()
        assertEquals("LouiseLM needs your attention", notification.extras.getString(Notification.EXTRA_TITLE))
        assertEquals("Open the private inbox to see what needs attention.", notification.extras.getString(Notification.EXTRA_TEXT))
        assertEquals(Notification.VISIBILITY_PRIVATE, notification.visibility)
        val intent = shadowOf(notification.contentIntent).savedIntent
        assertEquals(MainActivity::class.java.name, intent.component?.className)
        assertEquals(ATTENTION_INBOX_ACTION, intent.action)
        assertEquals(null, intent.extras)
        assertTrue(shadowOf(notification.contentIntent).isImmutable)
        manager.getNotificationChannel(ATTENTION_CHANNEL).also {
            it.importance = NotificationManager.IMPORTANCE_NONE
            manager.createNotificationChannel(it)
        }
        assertFalse(showAttentionNotification(context))
    }

    @Test
    fun serviceRejectsUnpairedUnsetMalformedAndDuplicateWakeups() {
        val context = RuntimeEnvironment.getApplication()
        shadowOf(context).grantPermissions(Manifest.permission.POST_NOTIFICATIONS)
        val manager = context.getSystemService(NotificationManager::class.java)
        val controller = Robolectric.buildService(AttentionMessagingService::class.java).create()
        val service = controller.get()
        fun wake(vararg fields: Pair<String, String>) = service.onMessageReceived(
            RemoteMessage.Builder("fixture").setData(mapOf(*fields)).build(),
        )
        try {
            wake("generation" to "1")
            assertEquals(0, shadowOf(manager).size())
            // Seed only encrypted-envelope presence, not a decryptable or live credential.
            context.getSharedPreferences("pairing-v2", Context.MODE_PRIVATE).edit().putString("ciphertext", "fixture").commit()
            val state = AttentionState(context)
            wake("generation" to "1")
            assertEquals(0, shadowOf(manager).size())
            state.enable()
            val owner = checkNotNull(state.owner())
            state.registered(owner)
            wake("generation" to "1", "title" to "remote text")
            wake("generation" to "-1")
            assertEquals(0, shadowOf(manager).size())
            wake("generation" to "4")
            assertEquals(1, shadowOf(manager).size())
            manager.cancelAll()
            wake("generation" to "4")
            wake("generation" to "3")
            assertEquals(0, shadowOf(manager).size())
            wake("generation" to "5")
            assertEquals(1, shadowOf(manager).size())
            state.block(owner)
            manager.cancelAll()
            wake("generation" to "6")
            assertEquals(0, shadowOf(manager).size())
        } finally {
            controller.destroy()
        }
    }
}
