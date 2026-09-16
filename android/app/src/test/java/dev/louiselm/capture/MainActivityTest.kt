package dev.louiselm.capture

import android.Manifest
import android.annotation.SuppressLint
import android.content.Intent
import android.net.ConnectivityManager
import android.os.Looper
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.TextView
import androidx.work.Configuration
import androidx.work.WorkManager
import androidx.work.impl.WorkManagerImpl
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.android.controller.ActivityController
import org.robolectric.android.util.concurrent.PausedExecutorService
import org.robolectric.annotation.Config
import org.robolectric.annotation.LooperMode
import org.robolectric.util.ReflectionHelpers
import java.io.File
import java.util.concurrent.ExecutorService

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], qualifiers = "en")
@LooperMode(LooperMode.Mode.PAUSED)
class MainActivityTest {
    private lateinit var controller: ActivityController<MainActivity>
    private lateinit var activity: MainActivity
    private lateinit var executor: PausedExecutorService
    private lateinit var workExecutor: PausedExecutorService

    @Before
    fun setUp() {
        val application = RuntimeEnvironment.getApplication()
        shadowOf(application)
            .grantPermissions(Manifest.permission.RECORD_AUDIO)
        shadowOf(application.getSystemService(ConnectivityManager::class.java)).setActiveNetworkInfo(null)
        workExecutor = PausedExecutorService()
        WorkManager.initialize(application, Configuration.Builder()
            .setExecutor(workExecutor)
            .setTaskExecutor(workExecutor)
            .build())
        controller = Robolectric.buildActivity(MainActivity::class.java)
        activity = controller.get()
        // Control the existing executor before onCreate, without a production
        // test hook. Tasks still run on a real background thread; UI work stays
        // queued on the paused main Looper. Assertions use buttons/text/files.
        ReflectionHelpers.getField<ExecutorService>(activity, "networkExecutor").shutdown()
        executor = PausedExecutorService()
        ReflectionHelpers.setField(activity, "networkExecutor", executor)
        controller.setup()
        drainStatus()
    }

    @After
    @SuppressLint("RestrictedApi") // Fixture owns WorkManager's database and singleton lifetime.
    fun tearDown() {
        try {
            if (::activity.isInitialized && !activity.isDestroyed) controller.close()
            if (::executor.isInitialized) executor.runAll()
            shadowOf(Looper.getMainLooper()).idle()
        } finally {
            if (::executor.isInitialized) executor.shutdownNow()
            if (::workExecutor.isInitialized) workExecutor.shutdownNow()
            // Robolectric does not reset this library's static singleton. No
            // work tasks ran; close its database/scope before the next sandbox.
            if (WorkManagerImpl.isInitialized()) {
                WorkManagerImpl.getInstance(RuntimeEnvironment.getApplication()).closeDatabase()
            }
            WorkManagerImpl.setDelegate(null)
            ReflectionHelpers.setStaticField(WorkManagerImpl::class.java, "sDefaultInstance", null)
        }
    }

    @Test
    fun notificationTapRefreshesInboxAndIgnoresLateCompletionAfterDestruction() {
        val inbox = views(activity.window.decorView).filterIsInstance<TextView>()
            .single { it.text.toString() == activity.getString(R.string.attention_unpaired) }
        inbox.text = "stale screen"
        controller.newIntent(Intent(activity, MainActivity::class.java).setAction(ATTENTION_INBOX_ACTION))
        drainStatus()
        assertEquals(activity.getString(R.string.attention_unpaired), inbox.text.toString())
        controller.newIntent(Intent(activity, MainActivity::class.java).setAction(ATTENTION_INBOX_ACTION))
        controller.close()
        inbox.text = "destroyed screen"
        drainStatus()
        assertEquals("destroyed screen", inbox.text.toString())
    }

    @Test
    @Config(sdk = [28, 34, 37])
    fun notificationConfigurationAndPermissionNeverPreventRecording() {
        button(R.string.attention_enable).performClick()
        if (!BuildConfig.FIREBASE_ENABLED) {
            assertEquals(null, shadowOf(activity).lastRequestedPermission)
            assertTrue(views(activity.window.decorView).filterIsInstance<TextView>()
                .any { it.text.toString() == activity.getString(R.string.attention_not_configured) })
        } else if (android.os.Build.VERSION.SDK_INT >= 33) {
            val request = shadowOf(activity).lastRequestedPermission
            assertEquals(listOf(Manifest.permission.POST_NOTIFICATIONS), request.requestedPermissions.toList())
            activity.onRequestPermissionsResult(request.requestCode, request.requestedPermissions, intArrayOf(-1))
            assertTrue(views(activity.window.decorView).filterIsInstance<TextView>()
                .any { it.text.toString() == activity.getString(R.string.attention_notification_denied) })
        } else {
            assertEquals(null, shadowOf(activity).lastRequestedPermission)
            drainStatus()
            assertTrue(views(activity.window.decorView).filterIsInstance<TextView>()
                .any { it.text.toString() == activity.getString(R.string.attention_notification_failed) })
        }
        assertTrue(button(R.string.start_capture).isEnabled)
    }

    @Test
    @Config(sdk = [37])
    fun receiverPermissionCancellationLeavesControlsUsable() {
        val status = statusView()
        assertEquals(null, shadowOf(activity).lastRequestedPermission)
        for (label in listOf(R.string.pair_receiver, R.string.attention_retry)) {
            button(label).performClick()
            val request = shadowOf(activity).lastRequestedPermission
            assertTrue(request != null)
            assertEquals(listOf(Manifest.permission.ACCESS_LOCAL_NETWORK), request.requestedPermissions.toList())
            activity.onRequestPermissionsResult(request.requestCode, request.requestedPermissions, intArrayOf())
            assertEquals(activity.getString(R.string.local_network_denied), status.text.toString())
            assertTrue(button(label).isEnabled)
        }
    }

    @Test
    @Config(sdk = [37])
    fun attentionRevocationWhileQueuedStillAllowsRequestingPermission() {
        shadowOf(activity).grantPermissions(Manifest.permission.ACCESS_LOCAL_NETWORK)
        button(R.string.attention_retry).performClick()
        shadowOf(activity).denyPermissions(Manifest.permission.ACCESS_LOCAL_NETWORK)
        drainStatus()
        assertTrue(button(R.string.attention_retry).isEnabled)
    }

    @Test
    @Config(sdk = [28, 34])
    fun olderAndroidSyncDoesNotRequestReceiverPermission() {
        val status = statusView()
        button(R.string.sync_now).performClick()
        assertEquals(null, shadowOf(activity).lastRequestedPermission)
        assertEquals(activity.getString(R.string.sync_queued), status.text.toString())
    }

    @Test
    @Config(sdk = [37])
    fun syncRequestsLocalNetworkPermissionBeforeQueueingWork() {
        val status = statusView()
        button(R.string.sync_now).performClick()
        val request = shadowOf(activity).lastRequestedPermission
        assertTrue(request != null)
        assertEquals(listOf(Manifest.permission.ACCESS_LOCAL_NETWORK), request.requestedPermissions.toList())
        assertFalse(status.text.contains(activity.getString(R.string.sync_queued)))
        activity.onRequestPermissionsResult(request.requestCode, request.requestedPermissions, intArrayOf(-1))
        assertTrue(status.text.contains("Local network access"))
        assertTrue(button(R.string.start_capture).isEnabled)
    }

    @Test
    @Config(sdk = [37])
    fun grantedNetworkPermissionContinuesSyncOnce() {
        val status = statusView()
        button(R.string.sync_now).performClick()
        val request = shadowOf(activity).lastRequestedPermission
        assertTrue(request != null)
        shadowOf(activity).grantPermissions(Manifest.permission.ACCESS_LOCAL_NETWORK)
        activity.onRequestPermissionsResult(request.requestCode, request.requestedPermissions, intArrayOf(0))
        assertEquals(activity.getString(R.string.sync_queued), status.text.toString())
        status.text = "settled"
        activity.onRequestPermissionsResult(request.requestCode, request.requestedPermissions, intArrayOf(0))
        assertEquals("settled", status.text.toString())
    }

    @Test
    @Config(sdk = [37])
    fun disposedActivityIgnoresPendingNetworkPermission() {
        val status = statusView()
        button(R.string.sync_now).performClick()
        val request = shadowOf(activity).lastRequestedPermission
        assertTrue(request != null)
        val before = status.text.toString()
        controller.close()
        activity.onRequestPermissionsResult(request.requestCode, request.requestedPermissions, intArrayOf(0))
        assertEquals(before, status.text.toString())
    }

    @Test
    @Config(sdk = [34, 37])
    fun offlineCaptureShowsPendingAndAcknowledgementWithoutResuming() {
        // Observed on Android 14: capture ac7e076f-8def-487f-80bf-b5631a191c89,
        // codex/01a074f5-e592-72d1-9ccf-69e162d8cdfe, louiselm-qbr.1.15.
        val status = statusView()
        assertTrue(status.text.contains("Pending: 0"))
        val id = recordFixture()
        assertEquals(activity.getString(R.string.saving_status), status.text.toString())

        executor.runAll() // Store is durable; callback has not reached the UI.
        assertEquals(activity.getString(R.string.saving_status), status.text.toString())
        shadowOf(Looper.getMainLooper()).idle()
        drainStatus()

        assertTrue(status.text.toString(), status.text.contains("Pending: 1"))
        assertTrue(status.text.contains(activity.getString(R.string.saved_status, id)))
        assertFalse(File(activity.filesDir, "captures/$id/uploaded").exists())
    }

    @Test
    fun destroyedActivityIgnoresQueuedCaptureCompletion() {
        val status = statusView()
        val id = recordFixture()
        executor.runAll() // Queue successful completion, then destroy its owner.
        val before = status.text.toString()
        controller.close()
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals(before, status.text.toString())
        assertTrue(File(activity.filesDir, "captures/$id/capture.json").isFile)
    }

    @Test
    fun uploadCompletingBeforeQueueReadPreservesAcknowledgement() {
        val status = statusView()
        val id = recordFixture()
        executor.runAll()
        shadowOf(Looper.getMainLooper()).idle() // Queue refresh submitted, not read yet.

        // Model a fast upload's durable outcome, not a live HTTPS request or
        // WorkManager scheduler. The queue read must see the new state.
        val receiver = "a".repeat(64)
        val store = CaptureStore(activity)
        assertEquals(1, store.assignUnowned(receiver))
        assertTrue(store.markUploaded(id, receiver))
        drainStatus()

        assertTrue(status.text.toString(), status.text.contains("Pending: 0"))
        assertTrue(status.text.contains(activity.getString(R.string.saved_status, id)))
        assertTrue(File(activity.filesDir, "captures/$id/capture.json").isFile)
    }

    @Test
    fun destroyedActivityIgnoresQueuedStatusRender() {
        val status = statusView()
        recordFixture()
        executor.runAll()
        shadowOf(Looper.getMainLooper()).idle() // Completion requests queue refresh.
        executor.runAll() // Queue snapshot is ready, but its UI callback is pending.
        val before = status.text.toString()
        controller.close()
        shadowOf(Looper.getMainLooper()).idle()
        assertEquals(before, status.text.toString())
    }

    private fun recordFixture(): String {
        button(R.string.start_capture).performClick()
        // Robolectric shadows MediaRecorder: no microphone or real audio. Only
        // the app-private test file is populated so the real store can finalize.
        val metadata = File(activity.filesDir, "captures").listFiles().orEmpty()
            .single { it.name.startsWith(".recording-") && it.extension == "json" }
        val id = metadata.name.removePrefix(".recording-").removeSuffix(".json")
        File(metadata.parentFile, ".recording-$id.m4a").writeBytes(byteArrayOf(1, 2, 3, 4))
        button(R.string.stop_capture).performClick()
        return id
    }

    private fun drainStatus() {
        executor.runAll()
        shadowOf(Looper.getMainLooper()).idle()
        executor.runAll()
        shadowOf(Looper.getMainLooper()).idle()
    }

    private fun button(text: Int): Button = views(activity.window.decorView)
        .filterIsInstance<Button>().single { it.text == activity.getString(text) }

    private fun statusView(): TextView = views(activity.window.decorView)
        .filterIsInstance<TextView>().single { it.text.contains("Pending:") }

    private fun views(root: View): Sequence<View> = sequence {
        yield(root)
        if (root is ViewGroup) {
            for (index in 0 until root.childCount) yieldAll(views(root.getChildAt(index)))
        }
    }
}
