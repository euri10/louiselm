package dev.louiselm.capture

import android.Manifest
import android.app.Activity
import android.app.KeyguardManager
import android.content.BroadcastReceiver
import android.content.Context
import android.content.Intent
import android.content.IntentFilter
import android.content.pm.PackageManager
import android.os.Bundle
import android.os.CancellationSignal
import android.os.Handler
import android.os.Looper
import android.os.PowerManager
import android.os.SystemClock
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import java.security.GeneralSecurityException
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference

private data class PendingPreparation(val token: Long, val value: PreparedPhoneControlUnlock)
private data class PendingPeriod(val token: Long, val value: PhoneControlPeriod)

/** Owns the temporary working key and all callbacks that can expose the workspace. */
class PhoneControlActivity : Activity() {
    private val networkExecutor = Executors.newSingleThreadExecutor()
    private val generation = AtomicLong()
    private val pendingPreparation = AtomicReference<PendingPreparation?>()
    private val pendingPeriod = AtomicReference<PendingPeriod?>()
    private val handler = Handler(Looper.getMainLooper())
    private lateinit var client: PhoneControlClient
    private lateinit var statusView: TextView
    private lateinit var enableButton: Button
    private lateinit var checkButton: Button
    private lateinit var unlockButton: Button
    private lateinit var lockButton: Button
    private lateinit var refreshButton: Button
    private lateinit var sessionsButton: Button
    private lateinit var moreSessionsButton: Button
    private lateinit var olderButton: Button
    private lateinit var conversation: LinearLayout
    private var target: PhoneControlTarget? = null
    private var attentionNavigation = false
    private var currentStatus: PhoneControlStatus = PhoneControlStatus.Disabled
    private var period: PhoneControlPeriod? = null
    private var preparedUnlock: PreparedPhoneControlUnlock? = null
    private var cancellation: CancellationSignal? = null
    private var pendingNetworkAction: (() -> Unit)? = null
    private var busy = false
    @Volatile private var foreground = false
    private var sessionPage: PhoneControlReply.SessionPage? = null
    private var loadedOlder = false
    private var newerOmitted = false
    private var historyChanged = false
    private var browsePage: PhoneBrowsePage? = null
    private val expiryCheck = Runnable { scheduleExpiry() }
    private val refreshPoll = Runnable {
        if (foreground && !busy && period != null && target != null && hasReceiverNetworkAccess(this)) {
            readTarget(interaction = false)
        }
    }
    private val screenOff = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            if (intent?.action == Intent.ACTION_SCREEN_OFF) lockWorkspace()
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        attentionNavigation = intent.getBooleanExtra(ATTENTION, false) || intent.hasExtra(CONTROLLER_ID) || intent.hasExtra(SUBJECT_ID)
        target = try {
            val controller = intent.getStringExtra(CONTROLLER_ID)
            val subject = intent.getStringExtra(SUBJECT_ID)
            if (controller != null && subject != null) PhoneControlTarget(controller, subject) else null
        } catch (_: IllegalArgumentException) {
            null
        }
        client = PhoneControlClient(applicationContext)
        val page = contentView()
        WindowCompat.setDecorFitsSystemWindows(window, false)
        ViewCompat.setOnApplyWindowInsetsListener(page) { view, insets ->
            val safe = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
            view.setPadding(safe.left, safe.top, safe.right, safe.bottom)
            insets
        }
        setContentView(page)
        ContextCompat.registerReceiver(this, screenOff, IntentFilter(Intent.ACTION_SCREEN_OFF), ContextCompat.RECEIVER_NOT_EXPORTED)
        task({ client.status() }) { status, _ ->
            currentStatus = status
            busy = false
            render(if (attentionNavigation && target == null) R.string.phone_stale else null)
        }
    }

    override fun onResume() {
        super.onResume()
        foreground = true
        if (!deviceUsable()) {
            lockWorkspace()
            return
        }
        val current = period ?: run {
            conversation.visibility = View.VISIBLE
            return
        }
        conversation.visibility = View.GONE
        task({ client.status() }, { outcome ->
            lockWorkspace()
            showFailure(outcome)
        }) { status, _ ->
            busy = false
            currentStatus = status
            if (period !== current || status !is PhoneControlStatus.Granted || status.value != current.grant ||
                !current.isActive(SystemClock.elapsedRealtime())) {
                lockWorkspace()
                showFailure(PhoneControlOutcome.REVOKED)
            } else {
                render()
                if (target == null && hasReceiverNetworkAccess(this)) readSessions(interaction = false)
                else if (target == null) render(R.string.phone_unavailable)
                else if (hasReceiverNetworkAccess(this)) readTarget(interaction = false)
                else render(R.string.phone_unavailable)
            }
        }
    }

    override fun onPause() {
        foreground = false
        handler.removeCallbacks(refreshPoll)
        conversation.visibility = View.GONE
        super.onPause()
    }

    override fun onDestroy() {
        generation.incrementAndGet()
        pendingNetworkAction = null
        pendingPreparation.getAndSet(null)?.value?.discard()
        pendingPeriod.getAndSet(null)?.value?.lock()
        preparedUnlock?.discard()
        preparedUnlock = null
        cancellation?.cancel()
        cancellation = null
        period?.lock()
        period = null
        conversation.removeAllViews()
        handler.removeCallbacksAndMessages(null)
        unregisterReceiver(screenOff)
        networkExecutor.shutdown()
        super.onDestroy()
    }

    private fun contentView(): ScrollView {
        val padding = (24 * resources.displayMetrics.density).toInt()
        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setPadding(padding, padding, padding, padding)
        }
        fun add(view: View) = column.addView(view, LinearLayout.LayoutParams(
            ViewGroup.LayoutParams.MATCH_PARENT, ViewGroup.LayoutParams.WRAP_CONTENT,
        ))
        add(TextView(this).apply { text = getString(R.string.phone_title); textSize = 24f })
        add(TextView(this).apply { text = getString(R.string.phone_intro); textSize = 16f })
        statusView = TextView(this).apply { textSize = 18f; setPadding(0, padding, 0, padding) }
        add(statusView)
        enableButton = Button(this).apply {
            text = getString(R.string.phone_enable)
            setOnClickListener { withNetwork { beginEnrollment() } }
        }
        add(enableButton)
        checkButton = Button(this).apply {
            text = getString(R.string.phone_check)
            setOnClickListener { withNetwork { checkEnrollment() } }
        }
        add(checkButton)
        unlockButton = Button(this).apply {
            text = getString(R.string.phone_unlock)
            setOnClickListener { withNetwork { prepareUnlock() } }
        }
        add(unlockButton)
        lockButton = Button(this).apply { text = getString(R.string.phone_lock); setOnClickListener { lockWorkspace() } }
        add(lockButton)
        sessionsButton = Button(this).apply { text = getString(R.string.phone_sessions); setOnClickListener { withNetwork { showSessions() } } }
        add(sessionsButton)
        refreshButton = Button(this).apply {
            text = getString(R.string.phone_refresh)
            setOnClickListener { withNetwork { if (target == null) readSessions() else readTarget() } }
        }
        add(refreshButton)
        moreSessionsButton = Button(this).apply {
            text = getString(R.string.phone_more_sessions)
            setOnClickListener { withNetwork { readSessions(browsePage?.next) } }
        }
        add(moreSessionsButton)
        olderButton = Button(this).apply { text = getString(R.string.phone_load_older); setOnClickListener { withNetwork { readTarget(older = true) } } }
        add(olderButton)
        conversation = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        add(conversation)
        add(TextView(this).apply { text = getString(R.string.phone_admin) })
        return ScrollView(this).apply { addView(column) }
    }

    private fun render(message: Int? = null) {
        statusView.text = when {
            message != null -> getString(message)
            busy -> getString(R.string.phone_busy)
            period?.isActive(SystemClock.elapsedRealtime()) == true -> getString(R.string.phone_unlocked)
            currentStatus is PhoneControlStatus.Pending -> getString(R.string.phone_pending, (currentStatus as PhoneControlStatus.Pending).value.matchingCode)
            currentStatus is PhoneControlStatus.Granted -> getString(R.string.phone_locked)
            else -> getString(R.string.phone_disabled)
        }
        val unlocked = period?.isActive(SystemClock.elapsedRealtime()) == true
        enableButton.visibility = if (currentStatus is PhoneControlStatus.Granted) View.GONE else View.VISIBLE
        checkButton.visibility = if (currentStatus is PhoneControlStatus.Pending) View.VISIBLE else View.GONE
        unlockButton.visibility = if (currentStatus is PhoneControlStatus.Granted && !unlocked) View.VISIBLE else View.GONE
        lockButton.visibility = if (unlocked) View.VISIBLE else View.GONE
        sessionsButton.visibility = if (unlocked && target != null) View.VISIBLE else View.GONE
        sessionsButton.isEnabled = !busy
        refreshButton.visibility = if (unlocked) View.VISIBLE else View.GONE
        refreshButton.isEnabled = !busy
        moreSessionsButton.visibility = if (unlocked && target == null && browsePage?.next != null) View.VISIBLE else View.GONE
        moreSessionsButton.isEnabled = !busy
        olderButton.visibility = if (unlocked && (sessionPage?.nextBefore ?: 0) != 0L) View.VISIBLE else View.GONE
        olderButton.isEnabled = !busy
        enableButton.isEnabled = !busy
        checkButton.isEnabled = !busy
        unlockButton.isEnabled = !busy
    }

    private fun beginEnrollment() {
        task({ maySend -> client.enroll(maySend) }, { outcome ->
            task({ client.status() }) { status, _ ->
                currentStatus = status
                busy = false
                showFailure(outcome)
            }
        }) { pending, _ ->
            currentStatus = PhoneControlStatus.Pending(pending)
            busy = false
            render()
        }
    }

    private fun checkEnrollment() {
        task({ maySend -> client.checkEnrollment(maySend) }) { grant, _ ->
            if (grant != null) currentStatus = PhoneControlStatus.Granted(grant)
            busy = false
            render()
        }
    }

    private fun prepareUnlock() {
        task({ maySend -> client.prepareUnlock(maySend) }) { prepared, token ->
            try {
                cancellation = PhoneControlAuthenticator(this).start(prepared.signing, { signature ->
                    if (!active(token)) return@start
                    cancellation = null
                    if (signature == null || signature !== prepared.signing) {
                        preparedUnlock?.discard()
                        preparedUnlock = null
                        busy = false
                        render(R.string.phone_auth_denied)
                    } else finishUnlock(prepared, signature, token)
                }, { cancelled ->
                    if (!active(token)) return@start
                    cancellation = null
                    preparedUnlock?.discard()
                    preparedUnlock = null
                    busy = false
                    render(if (cancelled) R.string.phone_auth_cancelled else R.string.phone_auth_denied)
                })
            } catch (_: IllegalArgumentException) {
                preparedUnlock?.discard()
                preparedUnlock = null
                busy = false
                render(R.string.phone_auth_denied)
            } catch (_: SecurityException) {
                preparedUnlock?.discard()
                preparedUnlock = null
                busy = false
                render(R.string.phone_auth_denied)
            }
        }
    }

    private fun finishUnlock(prepared: PreparedPhoneControlUnlock, signature: java.security.Signature, token: Long) {
        networkExecutor.execute {
            val startedAt = SystemClock.elapsedRealtime()
            val result = try {
                if (!active(token)) PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
                else {
                    signature.update(prepared.proof.transcript())
                    client.finishUnlock(prepared, signature.sign(), startedAt) { active(token) }
                }
            } catch (_: GeneralSecurityException) {
                PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
            }
            if (result is PhoneControlResult.Value) {
                val pending = PendingPeriod(token, result.value)
                if (!active(token) || !pendingPeriod.compareAndSet(null, pending)) result.value.lock()
                else if (!active(token) && pendingPeriod.compareAndSet(pending, null)) result.value.lock()
            } else prepared.discard()
            runOnUiThread {
                if (!active(token)) {
                    takePeriod(token)?.lock()
                    return@runOnUiThread
                }
                preparedUnlock = null
                busy = false
                when (result) {
                    is PhoneControlResult.Value -> {
                        period = takePeriod(token) ?: return@runOnUiThread
                        scheduleExpiry()
                        render()
                        if (target != null) withNetwork { readTarget() }
                        else if (!attentionNavigation) withNetwork { readSessions() }
                    }
                    is PhoneControlResult.Failure -> showFailure(result.outcome)
                }
            }
        }
    }

    private fun <T> task(
        operation: (() -> Boolean) -> PhoneControlResult<T>,
        failure: ((PhoneControlOutcome) -> Unit)? = null,
        complete: (T, Long) -> Unit,
    ) {
        val token = generation.incrementAndGet()
        busy = true
        render()
        networkExecutor.execute {
            val result = operation { active(token) }
            val preparation = (result as? PhoneControlResult.Value)?.value as? PreparedPhoneControlUnlock
            if (preparation != null) {
                val pending = PendingPreparation(token, preparation)
                if (!active(token) || !pendingPreparation.compareAndSet(null, pending)) preparation.discard()
                else if (!active(token) && pendingPreparation.compareAndSet(pending, null)) preparation.discard()
            }
            runOnUiThread {
                if (!active(token)) {
                    takePreparation(token)?.discard()
                    return@runOnUiThread
                }
                if (preparation != null) {
                    preparedUnlock = takePreparation(token) ?: return@runOnUiThread
                }
                when (result) {
                    is PhoneControlResult.Value -> complete(result.value, token)
                    is PhoneControlResult.Failure -> {
                        busy = false
                        if (failure == null) showFailure(result.outcome) else failure(result.outcome)
                    }
                }
            }
        }
    }

    private fun active(token: Long): Boolean = generation.get() == token && !isDestroyed && !isFinishing && deviceUsable()

    private fun showSessions() {
        handler.removeCallbacks(refreshPoll)
        target = null
        sessionPage = null
        loadedOlder = false
        newerOmitted = false
        historyChanged = false
        conversation.removeAllViews()
        readSessions()
    }

    private fun readSessions(cursor: PhoneBrowseCursor? = null, interaction: Boolean = true) {
        val current = period ?: return
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding()
        if (binding == null) {
            lockWorkspace()
            render(R.string.phone_unavailable)
            return
        }
        task({ maySend ->
            PhoneControlBrowser(client).page(current, cursor, interaction) {
                foreground && maySend() && pairing.binding() == binding && current.isActive(SystemClock.elapsedRealtime())
            }
        }, { outcome -> if (foreground) showFailure(outcome) }) { page, _ ->
            busy = false
            if (!foreground) return@task
            if (pairing.binding() != binding) {
                lockWorkspace()
                render(R.string.phone_unavailable)
                return@task
            }
            val now = SystemClock.elapsedRealtime()
            if (period !== current || !(if (interaction) current.confirmInteraction(now) else current.isActive(now))) {
                lockWorkspace()
                return@task
            }
            browsePage = page
            scheduleExpiry()
            render()
            conversation.removeAllViews()
            conversation.visibility = View.VISIBLE
            if (page.rows.isEmpty()) {
                conversation.addView(TextView(this).apply { text = getString(R.string.phone_no_sessions) })
            }
            for (row in page.rows) {
                conversation.addView(Button(this).apply {
                    text = getString(R.string.phone_session_row, row.summary.name, row.summary.agent,
                        row.summary.project, sessionState(row.summary.status), row.target.controllerId, row.target.subjectId,
                        if (row.summary.truncated) getString(R.string.phone_truncated) else "")
                    setOnClickListener {
                        target = row.target
                        sessionPage = null
                        loadedOlder = false
                        newerOmitted = false
                        historyChanged = false
                        conversation.removeAllViews()
                        withNetwork { readTarget() }
                    }
                })
            }
        }
    }

    private fun readTarget(older: Boolean = false, interaction: Boolean = true) {
        val current = period ?: return
        val requested = target ?: return
        val previous = sessionPage
        val before = if (older) previous?.nextBefore ?: 0 else 0
        if (older && before == 0L) return
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding()
        if (binding == null) {
            lockWorkspace()
            render(R.string.phone_unavailable)
            return
        }
        task({ maySend ->
            client.readSession(current, requested, SystemClock.elapsedRealtime(), before = before,
                expectedRevision = if (older) previous?.revision ?: 0 else 0, interaction = interaction) {
                foreground && maySend() && pairing.binding() == binding && current.isActive(SystemClock.elapsedRealtime())
            }
        }, { outcome ->
            if (!foreground) return@task
            if (older && outcome == PhoneControlOutcome.STALE_SUBJECT) {
                historyChanged = true
                readTarget(interaction = false)
            } else {
                showFailure(outcome)
                if (outcome == PhoneControlOutcome.UNAVAILABLE) scheduleRefresh()
            }
        }) { page, _ ->
            busy = false
            if (!foreground) return@task
            if (pairing.binding() != binding) {
                lockWorkspace()
                render(R.string.phone_unavailable)
                return@task
            }
            val now = SystemClock.elapsedRealtime()
            if (period !== current || !(if (interaction) current.confirmInteraction(now) else current.isActive(now))) {
                lockWorkspace()
                return@task
            }
            val unchanged = !older && !interaction && previous != null && page.revision == previous.revision
            if (older) {
                if (previous == null || page.revision != previous.revision || page.entries.isEmpty() ||
                    previous.entries.isEmpty() || page.entries.last().seq >= previous.entries.first().seq) {
                    showFailure(PhoneControlOutcome.MALFORMED)
                    return@task
                }
                val combined = page.entries + previous.entries
                newerOmitted = newerOmitted || combined.size > HISTORY_WINDOW
                sessionPage = page.copy(entries = combined.take(HISTORY_WINDOW), complete = page.complete && previous.complete)
                loadedOlder = true
                historyChanged = false
            } else if (interaction || previous == null || page.revision != previous.revision) {
                historyChanged = if (interaction) false
                    else historyChanged || (previous != null && loadedOlder && page.revision != previous.revision)
                sessionPage = page
                loadedOlder = false
                newerOmitted = false
            }
            scheduleExpiry()
            render()
            conversation.visibility = View.VISIBLE
            if (!unchanged) displayPage(requireNotNull(sessionPage))
            scheduleRefresh()
        }
    }

    private fun displayPage(page: PhoneControlReply.SessionPage) {
        conversation.removeAllViews()
        fun line(value: String) {
            conversation.addView(TextView(this).apply { text = value; textSize = 16f })
        }
        line(getString(R.string.phone_session_state, sessionState(page.status)))
        if (historyChanged) line(getString(R.string.phone_history_changed))
        if (newerOmitted) line(getString(R.string.phone_newer_omitted))
        if (!page.complete) line(getString(R.string.phone_incomplete))
        for (entry in page.entries) {
            when (entry) {
                is PhoneControlEntry.Message -> line(getString(if (entry.role == "user") R.string.phone_user_message else R.string.phone_agent_message, entry.text))
                is PhoneControlEntry.Tool -> line(getString(R.string.phone_tool, entry.title, entry.status, entry.content))
                is PhoneControlEntry.Question -> {
                    line(getString(R.string.phone_question, entry.question))
                    for (option in entry.options) line(getString(R.string.phone_option, option.label, option.description, option.preview))
                }
                is PhoneControlEntry.Plan -> line(getString(R.string.phone_plan, entry.text))
            }
            if (entry.truncated) line(getString(R.string.phone_truncated))
        }
    }

    private fun sessionState(status: String): String {
        val state = when (status) {
            "starting" -> R.string.phone_state_starting
            "ready" -> R.string.phone_state_ready
            "admitting" -> R.string.phone_state_admitting
            "configuring" -> R.string.phone_state_configuring
            "preparing" -> R.string.phone_state_preparing
            "prompting" -> R.string.phone_state_prompting
            "running" -> R.string.phone_state_running
            "waiting_permission" -> R.string.phone_state_waiting_permission
            "cancelling" -> R.string.phone_state_cancelling
            else -> R.string.phone_state_error
        }
        return getString(state)
    }

    private fun deviceUsable(): Boolean = getSystemService(PowerManager::class.java).isInteractive &&
        !getSystemService(KeyguardManager::class.java).isDeviceLocked

    private fun showFailure(outcome: PhoneControlOutcome) {
        browsePage = null
        sessionPage = null
        loadedOlder = false
        newerOmitted = false
        handler.removeCallbacks(refreshPoll)
        conversation.removeAllViews()
        if (outcome == PhoneControlOutcome.EXPIRED) {
            period?.lock()
            period = null
        }
        if (outcome == PhoneControlOutcome.REVOKED || outcome == PhoneControlOutcome.UNAUTHENTICATED) {
            period?.lock()
            period = null
            currentStatus = PhoneControlStatus.Disabled
        }
        render(when (outcome) {
            PhoneControlOutcome.EXPIRED -> R.string.phone_expired
            PhoneControlOutcome.REVOKED -> R.string.phone_revoked
            PhoneControlOutcome.OUT_OF_SCOPE -> R.string.phone_out_of_scope
            PhoneControlOutcome.UNSUPPORTED -> R.string.phone_unsupported
            PhoneControlOutcome.UNAUTHENTICATED, PhoneControlOutcome.MALFORMED -> R.string.phone_invalid
            PhoneControlOutcome.STALE_SUBJECT -> R.string.phone_stale
            PhoneControlOutcome.UNAVAILABLE -> R.string.phone_unavailable
        })
    }

    private fun lockWorkspace() {
        generation.incrementAndGet()
        pendingNetworkAction = null
        pendingPreparation.getAndSet(null)?.value?.discard()
        pendingPeriod.getAndSet(null)?.value?.lock()
        preparedUnlock?.discard()
        preparedUnlock = null
        cancellation?.cancel()
        cancellation = null
        period?.lock()
        period = null
        sessionPage = null
        loadedOlder = false
        newerOmitted = false
        historyChanged = false
        browsePage = null
        conversation.removeAllViews()
        conversation.visibility = if (foreground) View.VISIBLE else View.GONE
        handler.removeCallbacksAndMessages(null)
        busy = false
        render(R.string.phone_locked)
    }

    private fun takePreparation(token: Long): PreparedPhoneControlUnlock? {
        val pending = pendingPreparation.get() ?: return null
        return if (pending.token == token && pendingPreparation.compareAndSet(pending, null)) pending.value else null
    }

    private fun takePeriod(token: Long): PhoneControlPeriod? {
        val pending = pendingPeriod.get() ?: return null
        return if (pending.token == token && pendingPeriod.compareAndSet(pending, null)) pending.value else null
    }

    private fun scheduleExpiry() {
        handler.removeCallbacks(expiryCheck)
        val remaining = period?.remainingMs(SystemClock.elapsedRealtime()) ?: return
        if (remaining == 0L) {
            lockWorkspace()
            return
        }
        handler.postDelayed(expiryCheck, remaining)
    }

    private fun scheduleRefresh() {
        handler.removeCallbacks(refreshPoll)
        if (foreground && period?.isActive(SystemClock.elapsedRealtime()) == true && target != null) {
            handler.postDelayed(refreshPoll, REFRESH_INTERVAL_MS)
        }
    }

    private fun withNetwork(action: () -> Unit) {
        if (hasReceiverNetworkAccess(this)) {
            action()
        } else {
            pendingNetworkAction = action
            requestPermissions(arrayOf(Manifest.permission.ACCESS_LOCAL_NETWORK), LOCAL_NETWORK_REQUEST)
        }
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, grantResults: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)
        if (requestCode == LOCAL_NETWORK_REQUEST) {
            val action = pendingNetworkAction
            pendingNetworkAction = null
            if (action == null || isDestroyed || isFinishing || !deviceUsable()) return
            if (grantResults.firstOrNull() == PackageManager.PERMISSION_GRANTED && hasReceiverNetworkAccess(this)) action()
            else render(R.string.local_network_denied)
        }
    }

    companion object {
        internal const val ATTENTION = "dev.louiselm.capture.ATTENTION_TARGET"
        internal const val CONTROLLER_ID = "dev.louiselm.capture.CONTROLLER_ID"
        internal const val SUBJECT_ID = "dev.louiselm.capture.SUBJECT_ID"
        private const val LOCAL_NETWORK_REQUEST = 1
        private const val HISTORY_WINDOW = 500
        private const val REFRESH_INTERVAL_MS = 5_000L
    }
}
