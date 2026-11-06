package dev.louiselm.capture

import android.Manifest
import android.annotation.SuppressLint
import android.content.Context
import android.content.Intent
import android.net.ConnectivityManager
import android.os.Looper
import android.text.Spanned
import android.text.style.ClickableSpan
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.TextView
import androidx.work.Configuration
import androidx.work.WorkManager
import androidx.work.impl.WorkManagerImpl
import org.json.JSONArray
import org.json.JSONObject
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
        createActivity()
    }

    private fun createActivity() {
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
    @Config(
        shadows = [AttentionFixturePairing::class, AttentionFixtureReceiver::class],
        instrumentedPackages = ["dev.louiselm.capture"],
    )
    fun invalidatedEvidenceRendersFromPrivateInbox() {
        button(R.string.attention_retry).performClick()
        drainStatus()
        val text = views(activity.window.decorView).filterIsInstance<TextView>().map { it.text.toString() }
        assertTrue(text.contains("Skill supply is unverified"))
        assertTrue(text.contains(activity.getString(R.string.attention_session, "invalidated-session")))
        assertFalse(text.any { it.contains("invalid Attention inbox") })
        assertEquals(7L, AttentionState(activity).cached("fixture-pair")?.generation)
    }

    @Test
    @Config(
        sdk = [28, 34, 37],
        shadows = [AttentionFixturePairing::class, AttentionFixtureReceiver::class],
        instrumentedPackages = ["dev.louiselm.capture"],
    )
    fun supplyCardsAreAccessibleReadOnlyAndKeepLabelsInert() {
        showSupplyInbox()
        val labels = views(activity.window.decorView).filterIsInstance<TextView>().toList()
        assertEquals(2, labels.count { it.text.toString() == "Endorsement was withdrawn" })
        assertTrue(labels.any { it.text.toString() == "Package: $supplyPackageDigest" })
        assertTrue(labels.any { it.text.toString() == "Endorser: $SUPPLY_ENDORSER" })
        assertTrue(labels.any { it.text.toString() == "Admission relied on this Endorsement (relied_on)" })
        assertTrue(labels.any { it.text.toString() == "Endorsement names this package (subject_only)" })
        val approval = labels.single { it.text.toString() == "Skill approval is pending" }
        val approvalCard = approval.parent as ViewGroup
        assertEquals(
            listOf("Skill approval is pending", "Local supply", "Request: $SUPPLY_REQUEST"),
            views(approvalCard).filterIsInstance<TextView>().map { it.text.toString() }.toList(),
        )
        val headings = labels.filter { it.text.toString() in listOf("Endorsement was withdrawn", "Skill approval is pending") }
        for (heading in headings) {
            val card = heading.parent as ViewGroup
            assertTrue(heading.isAccessibilityHeading)
            assertTrue(card.isScreenReaderFocusable)
            assertFalse(views(card).any { it is Button || it.isClickable || it.isLongClickable })
            for (label in views(card).filterIsInstance<TextView>()) {
                assertEquals(label.text.toString(), label.createAccessibilityNodeInfo().text.toString())
                assertEquals(0, label.autoLinkMask)
                val text = label.text
                if (text is Spanned) assertTrue(text.getSpans(0, text.length, ClickableSpan::class.java).isEmpty())
            }
        }
        assertEquals(3, AttentionState(activity).cached("fixture-pair")?.items?.size)
    }

    @Test
    @Config(
        shadows = [AttentionFixturePairing::class, AttentionFixtureReceiver::class],
        instrumentedPackages = ["dev.louiselm.capture"],
    )
    fun supplyCardsSurviveOfflineRestartAndIgnoreQueuedRenderAfterDestruction() {
        showSupplyInbox()
        val before = views(activity.window.decorView).filterIsInstance<TextView>().map { it.text.toString() }.toList()
        assertTrue(before.contains("Endorser: $SUPPLY_ENDORSER"))
        val options = activity.getSharedPreferences("attention-fixture", Context.MODE_PRIVATE)
        options.edit().putBoolean("offline", true).commit()
        controller.close()
        drainStatus()
        createActivity()
        var labels = views(activity.window.decorView).filterIsInstance<TextView>().map { it.text.toString() }.toList()
        assertTrue(labels.contains("Endorser: $SUPPLY_ENDORSER"))
        assertTrue(labels.any { it.contains("receiver is offline") })
        options.edit().putBoolean("offline", false).putString("payload", "{\"generation\":10,\"items\":[]}").commit()
        controller.newIntent(Intent(activity, MainActivity::class.java).setAction(ATTENTION_INBOX_ACTION))
        executor.runAll() // Fetch completes off-thread; its UI render is still queued.
        controller.close()
        shadowOf(Looper.getMainLooper()).idle()
        labels = views(activity.window.decorView).filterIsInstance<TextView>().map { it.text.toString() }.toList()
        assertTrue(labels.contains("Endorser: $SUPPLY_ENDORSER"))
        assertEquals(9L, AttentionState(activity).cached("fixture-pair")?.generation)
    }

    @Test
    @Config(
        shadows = [AttentionFixturePairing::class, AttentionFixtureReceiver::class],
        instrumentedPackages = ["dev.louiselm.capture"],
    )
    fun supplyRefreshKeepsIneligibleCardsUpdatesMetadataAndAcceptsAuthoritativeClear() {
        showSupplyInbox()
        val options = activity.getSharedPreferences("attention-fixture", Context.MODE_PRIVATE)
        val withdrawal = supplyWithdrawal().put("eligible", false).apply {
            getJSONObject("withdrawal").put("endorser", "Renamed reviewer").put("urgency", "subject_only")
        }
        val session = JSONObject(invalidatedAttentionPayload).getJSONArray("items").getJSONObject(0)
        val run = JSONObject().put("subject_kind", "run").put("subject_id", SUPPLY_REQUEST)
            .put("kind", "run_parked").put("reason", "Run is Parked").put("source_operation_id", SUPPLY_REQUEST)
            .put("created_at_ms", 123).put("eligible", true).put("stage", "review/execute")
        val payload = JSONObject().put("generation", 10).put("items", JSONArray().put(withdrawal).put(session).put(run))
        options.edit().putString("payload", payload.toString()).commit()
        button(R.string.attention_retry).performClick()
        drainStatus()
        fun text(): List<String> = views(activity.window.decorView).filterIsInstance<TextView>().map { it.text.toString() }.toList()
        assertTrue(text().contains("Endorser: Renamed reviewer"))
        assertFalse(text().contains("Endorser: $SUPPLY_ENDORSER"))
        assertFalse(text().contains("Admission relied on this Endorsement (relied_on)"))
        assertTrue(text().contains("Endorsement names this package (subject_only)"))
        assertTrue(text().contains("Session: invalidated-session"))
        assertTrue(text().contains("Run: $SUPPLY_REQUEST"))
        assertTrue(text().contains("Stage: review/execute"))
        assertFalse(checkNotNull(AttentionState(activity).cached("fixture-pair")).items.first().eligible)
        options.edit().putString("payload", supplyAttentionPayload()).commit() // Older response cannot undo the update.
        button(R.string.attention_retry).performClick()
        drainStatus()
        assertTrue(text().contains("Endorser: Renamed reviewer"))
        assertEquals(10L, AttentionState(activity).cached("fixture-pair")?.generation)
        options.edit().putString("payload", "{\"generation\":11,\"items\":[]}").commit()
        button(R.string.attention_retry).performClick()
        drainStatus()
        assertTrue(text().contains(activity.getString(R.string.attention_empty)))
        assertFalse(text().contains("Endorsement was withdrawn"))
        assertFalse(text().contains("Run is Parked"))
        assertEquals(AttentionSnapshot(11, emptyList()), AttentionState(activity).cached("fixture-pair"))
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

    private fun showSupplyInbox() {
        activity.getSharedPreferences("attention-fixture", Context.MODE_PRIVATE).edit()
            .putString("payload", supplyAttentionPayload()).commit()
        shadowOf(activity).grantPermissions(Manifest.permission.ACCESS_LOCAL_NETWORK)
        button(R.string.attention_retry).performClick()
        drainStatus()
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
