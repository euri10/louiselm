package dev.louiselm.capture

import android.Manifest
import android.app.Activity
import android.app.AlertDialog
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
import android.text.Editable
import android.text.InputFilter
import android.text.InputType
import android.text.TextWatcher
import android.view.View
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.core.content.ContextCompat
import androidx.core.view.ViewCompat
import androidx.core.view.WindowCompat
import androidx.core.view.WindowInsetsCompat
import java.io.File
import java.io.IOException
import java.security.GeneralSecurityException
import java.security.MessageDigest
import java.security.ProviderException
import java.text.DateFormat
import java.util.Date
import java.util.Locale
import java.util.concurrent.Executors
import java.util.concurrent.atomic.AtomicLong
import java.util.concurrent.atomic.AtomicReference
import org.json.JSONException

private data class PendingPreparation(val token: Long, val value: PreparedPhoneControlUnlock)
private data class PendingPeriod(val token: Long, val value: PhoneControlPeriod)
private data class PendingOfflinePreparation(val token: Long, val value: PreparedPhoneOfflineUnlock)
private data class PendingOfflinePeriod(val token: Long, val value: PhoneOfflinePeriod)
private data class PhoneTargetRead(val refresh: PhoneSessionRefresh, val pages: List<PhoneControlReply.SessionPage>, val saveMessage: Int?)
private data class PhoneReconciliation(
    val pending: List<PendingPhoneAction>?,
    val records: List<PhoneActionRecord>,
    val nextAfter: String?,
    val failure: PhoneControlOutcome?,
)

/** Owns the temporary working key and all callbacks that can expose the workspace. */
class PhoneControlActivity : Activity() {
    private val networkExecutor = Executors.newSingleThreadExecutor()
    private val generation = AtomicLong()
    private val pendingPreparation = AtomicReference<PendingPreparation?>()
    private val pendingPeriod = AtomicReference<PendingPeriod?>()
    private val pendingOfflinePreparation = AtomicReference<PendingOfflinePreparation?>()
    private val pendingOfflinePeriod = AtomicReference<PendingOfflinePeriod?>()
    private val handler = Handler(Looper.getMainLooper())
    private lateinit var client: PhoneControlClient
    private lateinit var pendingActionsStore: PendingPhoneActionStore
    private lateinit var offlineStore: PhoneOfflineStore
    private lateinit var offlineUnlockStore: PhoneOfflineUnlockStore
    private lateinit var draftStore: PhoneDraftStore
    private lateinit var draftUnlockStore: PhoneOfflineUnlockStore
    private lateinit var statusView: TextView
    private lateinit var enableButton: Button
    private lateinit var checkButton: Button
    private lateinit var unlockButton: Button
    private lateinit var lockButton: Button
    private lateinit var refreshButton: Button
    private lateinit var sessionsButton: Button
    private lateinit var moreSessionsButton: Button
    private lateinit var runsButton: Button
    private lateinit var moreRunsButton: Button
    private lateinit var preparedButton: Button
    private lateinit var morePreparedButton: Button
    private lateinit var olderButton: Button
    private lateinit var actionHistoryButton: Button
    private lateinit var moreActionHistoryButton: Button
    private lateinit var offlineUnlockButton: Button
    private lateinit var draftUnlockButton: Button
    private lateinit var draftListButton: Button
    private lateinit var draftDiscardButton: Button
    private lateinit var draftContextView: LinearLayout
    private lateinit var offlineCopiesButton: Button
    private lateinit var offlineKeepButton: Button
    private lateinit var offlineOlderButton: Button
    private lateinit var offlineNewerButton: Button
    private lateinit var offlineUnpinButton: Button
    private lateinit var offlineRemoveButton: Button
    private lateinit var offlineBudgetButton: Button
    private lateinit var conversation: LinearLayout
    private lateinit var sendInput: EditText
    private lateinit var sendButton: Button
    private lateinit var sendStatus: TextView
    private var target: PhoneControlTarget? = null
    private var attentionNavigation = false
    private var attentionRunId: String? = null
    private var attentionResultId: String? = null
    private var currentStatus: PhoneControlStatus = PhoneControlStatus.Disabled
    private var period: PhoneControlPeriod? = null
    private var offlinePeriod: PhoneOfflinePeriod? = null
    private var draftPeriod: PhoneOfflinePeriod? = null
    private var draftsReady = false
    private var draftsDamaged = false
    private var showingDrafts = false
    private var selectedDraft: PhoneOfflineSource? = null
    private var visibleOfflinePage: PhoneControlReply.SessionPage? = null
    private var preparedOfflineUnlock: PreparedPhoneOfflineUnlock? = null
    private var showingOffline = false
    private var selectedOffline: PhoneOfflineCopy? = null
    private var offlinePageIndex = 0
    private var selectedSource: PhoneOfflineSource? = null
    private var openedPages = emptyList<PhoneControlReply.SessionPage>()
    private var sessionOpenedAtMs: Long? = null
    private var preparedUnlock: PreparedPhoneControlUnlock? = null
    private var cancellation: CancellationSignal? = null
    private var pendingNetworkAction: (() -> Unit)? = null
    private var busy = false
    @Volatile private var foreground = false
    private var sessionPage: PhoneControlReply.SessionPage? = null
    private var newerOmitted = false
    private var historyChanged = false
    private var browsePage: PhoneBrowsePage? = null
    private var actionHistoryPage: PhoneControlReply.ActionHistoryPage? = null
    private var showingActionHistory = false
    private var showingPrepared = false
    private var preparedPage: PhoneControlReply.PreparedPage? = null
    private var preparedAfter: String? = null
    private var selectedPrepared: PhonePreparedSummary? = null
    private var showingRuns = false
    private var runPage: PhoneControlReply.RunPage? = null
    private var runAfter: String? = null
    private var selectedRunId: String? = null
    private var selectedSavedResult: String? = null
    private var linkedRunSession: PhoneRunSessionTarget? = null
    private val drafts = mutableMapOf<Pair<String, PhoneControlTarget>, String>()
    private var draftTarget: PhoneControlTarget? = null
    private var draftReceiver: String? = null
    private var changingDraft = false
    private var draftSource: PhoneOfflineSource? = null
    private var draftContextOffset = 0
    private val draftSources = mutableMapOf<Pair<String, PhoneControlTarget>, PhoneOfflineSource>()
    private val draftContexts = mutableMapOf<Pair<String, PhoneControlTarget>, List<PhoneDraftSelection>>()
    private val savedDrafts = mutableMapOf<Pair<String, PhoneControlTarget>, PhoneDraft>()
    private val dirtyDrafts = linkedMapOf<Pair<String, PhoneControlTarget>, PhoneDraft>()
    private val draftGeneration = AtomicLong()
    private var savingDraft = false
    private var pendingDraftSave: AtomicReference<PhoneDraft?>? = null
    private var discardingDraft: Pair<String, PhoneControlTarget>? = null
    private var draftSaveNotice: Int? = null
    private var pendingActions = emptyList<PendingPhoneAction>()
    private val actionEvidence = mutableMapOf<String, PhoneActionRecord>()
    private val reviewedFences = mutableSetOf<String>()
    private var reconciliationLoaded = false
    private var reconciliationCursor: String? = null
    private var sendNotice: Int? = null
    private var pinRefreshIndex = 0
    private val expiryCheck = Runnable { scheduleExpiry() }
    private val offlineExpiryCheck = Runnable { scheduleOfflineExpiry() }
    private val draftExpiryCheck = Runnable { scheduleDraftExpiry() }
    private val pinRefresh = Runnable { refreshPinnedCopy() }
    private val reconcilePoll = Runnable { reconcilePendingActions() }
    private val refreshPoll = Runnable {
        if (foreground && !busy && !showingActionHistory && period != null && target != null && hasReceiverNetworkAccess(this)) {
            readTarget(interaction = false)
        }
    }
    private val screenOff = object : BroadcastReceiver() {
        override fun onReceive(context: Context?, intent: Intent?) {
            if (intent?.action == Intent.ACTION_SCREEN_OFF || intent?.action == PHONE_OFFLINE_REVOKED) {
                lockWorkspace()
                if (intent.action == PHONE_OFFLINE_REVOKED) render(R.string.phone_revoked)
            }
        }
    }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        window.addFlags(WindowManager.LayoutParams.FLAG_SECURE)
        attentionNavigation = intent.getBooleanExtra(ATTENTION, false) || intent.hasExtra(CONTROLLER_ID) ||
            intent.hasExtra(SUBJECT_ID) || intent.hasExtra(RUN_ID) || intent.hasExtra(RESULT_ID)
        target = try {
            val controller = intent.getStringExtra(CONTROLLER_ID)
            val subject = intent.getStringExtra(SUBJECT_ID)
            if (controller != null && subject != null) PhoneControlTarget(controller, subject) else null
        } catch (_: IllegalArgumentException) {
            null
        }
        attentionRunId = try {
            intent.getStringExtra(RUN_ID)?.also { phoneControlRunIdBytes(it) }
        } catch (_: IllegalArgumentException) {
            null
        }
        if (intent.hasExtra(RUN_ID) && (intent.hasExtra(CONTROLLER_ID) || intent.hasExtra(SUBJECT_ID))) {
            target = null
            attentionRunId = null
        }
        attentionResultId = intent.getStringExtra(RESULT_ID)?.takeIf(::validSavedResultId)
        if (intent.hasExtra(RESULT_ID) && (attentionResultId == null || attentionRunId == null)) {
            target = null
            attentionRunId = null
            attentionResultId = null
        }
        showingRuns = attentionRunId != null
        selectedRunId = attentionRunId
        selectedSavedResult = attentionResultId
        client = PhoneControlClient(applicationContext)
        pendingActionsStore = PendingPhoneActionStore(applicationContext)
        val downloadRoot = File(filesDir, "phone-offline-v1")
        offlineStore = PhoneOfflineStore(downloadRoot)
        offlineUnlockStore = PhoneOfflineUnlockStore(applicationContext, downloadRoot)
        val draftRoot = File(filesDir, "phone-drafts-v1")
        draftStore = PhoneDraftStore(draftRoot)
        draftUnlockStore = PhoneOfflineUnlockStore(applicationContext, draftRoot, "phone-draft-key-v1", draft = true)
        val page = contentView()
        WindowCompat.setDecorFitsSystemWindows(window, false)
        ViewCompat.setOnApplyWindowInsetsListener(page) { view, insets ->
            val safe = insets.getInsets(WindowInsetsCompat.Type.systemBars() or WindowInsetsCompat.Type.displayCutout())
            view.setPadding(safe.left, safe.top, safe.right, safe.bottom)
            insets
        }
        setContentView(page)
        val lockEvents = IntentFilter(Intent.ACTION_SCREEN_OFF).apply { addAction(PHONE_OFFLINE_REVOKED) }
        ContextCompat.registerReceiver(this, screenOff, lockEvents, ContextCompat.RECEIVER_NOT_EXPORTED)
        task({ client.status() }) { status, _ ->
            currentStatus = status
            busy = false
            render(if (attentionNavigation && target == null && attentionRunId == null) R.string.phone_stale else null)
        }
    }

    override fun onResume() {
        super.onResume()
        foreground = true
        if (!deviceUsable()) {
            lockWorkspace()
            return
        }
        schedulePinRefresh()
        if (showingDrafts && draftPeriod?.isActive(SystemClock.elapsedRealtime()) == true) {
            conversation.visibility = View.VISIBLE
            render()
            return
        }
        if (showingOffline) {
            if (offlinePeriod?.isActive(SystemClock.elapsedRealtime()) == true) {
                conversation.visibility = View.VISIBLE
                scheduleOfflineExpiry()
                scheduleReconciliation(0)
                render()
                return
            }
            lockOffline()
        }
        val current = period ?: run {
            conversation.visibility = View.VISIBLE
            render()
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
                scheduleReconciliation(0)
                if (showingRuns && hasReceiverNetworkAccess(this)) {
                    readSelectedRun(interaction = false)
                }
                else if (showingRuns) render(R.string.phone_unavailable)
                else if (showingPrepared && hasReceiverNetworkAccess(this)) {
                    val selected = selectedPrepared
                    if (selected == null) readPreparedPage(preparedAfter, interaction = false) else readPreparedDetail(selected, interaction = false)
                }
                else if (showingPrepared) render(R.string.phone_unavailable)
                else if (showingActionHistory && hasReceiverNetworkAccess(this)) readActionHistory()
                else if (target == null && hasReceiverNetworkAccess(this)) readSessions(interaction = false)
                else if (target == null) render(R.string.phone_unavailable)
                else if (hasReceiverNetworkAccess(this)) readTarget(interaction = false)
                else render(R.string.phone_unavailable)
            }
        }
    }

    override fun onPause() {
        foreground = false
        reconciliationLoaded = false
        handler.removeCallbacks(refreshPoll)
        handler.removeCallbacks(pinRefresh)
        handler.removeCallbacks(reconcilePoll)
        conversation.visibility = View.GONE
        sendInput.visibility = View.GONE
        sendButton.visibility = View.GONE
        sendStatus.visibility = View.GONE
        draftContextView.visibility = View.GONE
        draftDiscardButton.visibility = View.GONE
        super.onPause()
    }

    override fun onUserInteraction() {
        super.onUserInteraction()
        val now = SystemClock.elapsedRealtime()
        offlinePeriod?.let { if (it.confirmInteraction(now)) scheduleOfflineExpiry() else lockOffline() }
        draftPeriod?.let { if (it.confirmInteraction(now)) scheduleDraftExpiry() else lockDrafts() }
    }

    override fun onDestroy() {
        generation.incrementAndGet()
        pendingNetworkAction = null
        pendingPreparation.getAndSet(null)?.value?.discard()
        pendingPeriod.getAndSet(null)?.value?.lock()
        pendingOfflinePreparation.getAndSet(null)?.value?.discard()
        pendingOfflinePeriod.getAndSet(null)?.value?.lock()
        preparedUnlock?.discard()
        preparedUnlock = null
        preparedOfflineUnlock?.discard()
        preparedOfflineUnlock = null
        cancellation?.cancel()
        cancellation = null
        period?.lock()
        period = null
        offlinePeriod?.lock()
        offlinePeriod = null
        selectedOffline = null
        visibleOfflinePage = null
        selectedPrepared = null
        preparedPage = null
        selectedRunId = null
        selectedSavedResult = null
        linkedRunSession = null
        runPage = null
        clearDrafts()
        changingDraft = true
        sendInput.text.clear()
        changingDraft = false
        openedPages = emptyList()
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
        offlineUnlockButton = Button(this).apply {
            text = getString(R.string.phone_offline_unlock)
            setOnClickListener { unlockOffline { unlocked -> if (unlocked) showOfflineCopies() } }
        }
        add(offlineUnlockButton)
        draftUnlockButton = Button(this).apply {
            text = getString(R.string.phone_draft_unlock)
            setOnClickListener { unlockOffline(forDraft = true) { unlocked -> if (unlocked) loadDrafts() } }
        }
        add(draftUnlockButton)
        draftListButton = Button(this).apply { text = getString(R.string.phone_draft_list); setOnClickListener { showDrafts() } }
        add(draftListButton)
        lockButton = Button(this).apply { text = getString(R.string.phone_lock); setOnClickListener { lockWorkspace() } }
        add(lockButton)
        offlineCopiesButton = Button(this).apply {
            text = getString(R.string.phone_offline_copies)
            setOnClickListener { showOfflineCopies() }
        }
        add(offlineCopiesButton)
        offlineKeepButton = Button(this).apply {
            text = getString(R.string.phone_offline_keep)
            setOnClickListener { withNetwork { keepOffline() } }
        }
        add(offlineKeepButton)
        offlineOlderButton = Button(this).apply {
            text = getString(R.string.phone_offline_next)
            setOnClickListener { openOfflinePage(offlinePageIndex + 1, false) }
        }
        add(offlineOlderButton)
        offlineNewerButton = Button(this).apply {
            text = getString(R.string.phone_offline_previous)
            setOnClickListener { openOfflinePage(offlinePageIndex - 1, false) }
        }
        add(offlineNewerButton)
        offlineUnpinButton = Button(this).apply {
            text = getString(R.string.phone_offline_unpin)
            setOnClickListener { updateOfflineCopy(remove = false) }
        }
        add(offlineUnpinButton)
        offlineRemoveButton = Button(this).apply {
            text = getString(R.string.phone_offline_remove)
            setOnClickListener { updateOfflineCopy(remove = true) }
        }
        add(offlineRemoveButton)
        offlineBudgetButton = Button(this).apply {
            text = getString(R.string.phone_offline_budget)
            setOnClickListener { changeOfflineBudget() }
        }
        add(offlineBudgetButton)
        sessionsButton = Button(this).apply { text = getString(R.string.phone_sessions); setOnClickListener { withNetwork { showSessions() } } }
        add(sessionsButton)
        runsButton = Button(this).apply { text = getString(R.string.phone_runs); setOnClickListener { withNetwork { showRuns() } } }
        add(runsButton)
        preparedButton = Button(this).apply { text = getString(R.string.phone_prepared); setOnClickListener { withNetwork { showPrepared() } } }
        add(preparedButton)
        actionHistoryButton = Button(this).apply {
            text = getString(R.string.phone_action_history)
            setOnClickListener { withNetwork { readActionHistory() } }
        }
        add(actionHistoryButton)
        refreshButton = Button(this).apply {
            text = getString(R.string.phone_refresh)
            setOnClickListener { withNetwork {
                val selected = selectedPrepared
                if (showingRuns) readSelectedRun()
                else if (showingPrepared && selected != null) readPreparedDetail(selected)
                else if (showingPrepared) readPreparedPage()
                else if (showingActionHistory) readActionHistory()
                else if (target == null) readSessions() else readTarget()
            } }
        }
        add(refreshButton)
        moreSessionsButton = Button(this).apply {
            text = getString(R.string.phone_more_sessions)
            setOnClickListener { withNetwork { readSessions(browsePage?.next) } }
        }
        add(moreSessionsButton)
        moreRunsButton = Button(this).apply {
            text = getString(R.string.phone_more_runs)
            setOnClickListener { withNetwork { readRunPage(runPage?.nextAfter) } }
        }
        add(moreRunsButton)
        morePreparedButton = Button(this).apply {
            text = getString(R.string.phone_more_prepared)
            setOnClickListener { withNetwork { readPreparedPage(preparedPage?.nextAfter) } }
        }
        add(morePreparedButton)
        olderButton = Button(this).apply { text = getString(R.string.phone_load_older); setOnClickListener { withNetwork { readTarget(older = true) } } }
        add(olderButton)
        moreActionHistoryButton = Button(this).apply {
            text = getString(R.string.phone_more_actions)
            setOnClickListener { withNetwork { readActionHistory(actionHistoryPage?.nextAfter) } }
        }
        add(moreActionHistoryButton)
        conversation = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        add(conversation)
        sendStatus = TextView(this)
        add(sendStatus)
        sendInput = EditText(this).apply {
            hint = getString(R.string.phone_send_hint)
            isSaveEnabled = false
            inputType = InputType.TYPE_CLASS_TEXT or InputType.TYPE_TEXT_FLAG_MULTI_LINE or InputType.TYPE_TEXT_FLAG_CAP_SENTENCES
            filters = arrayOf(InputFilter.LengthFilter(8 * 1024 * 1024), InputFilter { _, start, end, _, _, _ ->
                val receiver = draftReceiver
                val selected = draftTarget
                if (!changingDraft && start < end && receiver != null && selected != null &&
                    drafts.size >= MAX_DRAFTS && (receiver to selected) !in drafts) {
                    sendNotice = R.string.phone_draft_limit
                    render()
                    ""
                } else null
            })
            minLines = 3
        }
        add(sendInput)
        draftContextView = LinearLayout(this).apply { orientation = LinearLayout.VERTICAL }
        add(draftContextView)
        draftDiscardButton = Button(this).apply { text = getString(R.string.phone_draft_discard); setOnClickListener { confirmDiscardDraft() } }
        add(draftDiscardButton)
        sendButton = Button(this).apply {
            text = getString(R.string.phone_send)
            setOnClickListener { withNetwork { sendPrompt() } }
        }
        add(sendButton)
        sendInput.addTextChangedListener(object : TextWatcher {
            override fun beforeTextChanged(s: CharSequence?, start: Int, count: Int, after: Int) = Unit
            override fun onTextChanged(s: CharSequence?, start: Int, before: Int, count: Int) = Unit
            override fun afterTextChanged(s: Editable?) {
                if (changingDraft) return
                val receiver = draftReceiver
                val selected = draftTarget
                if (receiver != null && selected != null) {
                    val key = receiver to selected
                    drafts[key] = s?.toString().orEmpty()
                    draftSource?.let { draftSources[key] = it }
                    queueDraftSave(key)
                }
                if (sendNotice == R.string.phone_send_invalid && sendDigest() != null) sendNotice = null
                if (sendNotice == R.string.phone_draft_limit && !s.isNullOrEmpty()) sendNotice = null
                render()
            }
        })
        add(TextView(this).apply { text = getString(R.string.phone_admin) })
        return ScrollView(this).apply { addView(column) }
    }

    private fun loadDrafts(after: () -> Unit = {}) {
        val current = draftPeriod ?: return
        task({ mayContinue -> offlineResult {
            val key = current.key(SystemClock.elapsedRealtime()) ?: return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
            if (!mayContinue()) return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
            PhoneControlResult.Value(draftStore.list(key))
        } }, { if (foreground) render(R.string.phone_draft_save_failed) }) { listing, _ ->
            busy = false
            if (!foreground || draftPeriod !== current || !current.isActive(SystemClock.elapsedRealtime())) return@task
            drafts.clear()
            draftSources.clear()
            draftContexts.clear()
            savedDrafts.clear()
            for (draft in listing.drafts) {
                val key = draft.source.receiver to draft.source.target
                drafts[key] = draft.text
                draftSources[key] = draft.source
                draftContexts[key] = draft.context
                savedDrafts[key] = draft
            }
            draftsReady = true
            draftsDamaged = listing.damaged.isNotEmpty()
            draftTarget = null
            draftReceiver = null
            render(if (listing.damaged.isNotEmpty()) R.string.phone_draft_damaged else null)
            if (showingOffline && visibleOfflinePage != null) displayPage(requireNotNull(visibleOfflinePage))
            else if (target == null || period == null) showDrafts()
            else sessionPage?.let { displayPage(it) }
            after()
        }
    }

    private fun queueDraftSave(key: Pair<String, PhoneControlTarget>) {
        val source = draftSources[key] ?: return
        dirtyDrafts[key] = PhoneDraft(source, drafts[key].orEmpty(), draftContexts[key].orEmpty().toList())
        draftSaveNotice = R.string.phone_draft_saving
        saveNextDraft()
    }

    private fun saveNextDraft() {
        if (savingDraft || dirtyDrafts.isEmpty()) return
        val current = draftPeriod ?: return
        val item = dirtyDrafts.entries.first()
        val key = item.key
        val request = AtomicReference<PhoneDraft?>(item.value)
        dirtyDrafts.remove(key)
        pendingDraftSave = request
        savingDraft = true
        val token = draftGeneration.get()
        networkExecutor.execute {
            val snapshot = request.get() ?: return@execute
            val result = offlineResult {
                val secret = current.key(SystemClock.elapsedRealtime()) ?: return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
                PhoneControlResult.Value(draftStore.save(snapshot, secret) {
                    draftGeneration.get() == token && current.isActive(SystemClock.elapsedRealtime())
                })
            }
            runOnUiThread {
                if (draftGeneration.get() != token || draftPeriod !== current || isDestroyed || isFinishing) {
                    request.set(null)
                    return@runOnUiThread
                }
                if (!deviceUsable()) {
                    lockWorkspace()
                    return@runOnUiThread
                }
                if (!current.isActive(SystemClock.elapsedRealtime())) {
                    lockDrafts()
                    return@runOnUiThread
                }
                val completed = request.getAndSet(null) ?: return@runOnUiThread
                pendingDraftSave = null
                savingDraft = false
                draftSaveNotice = if (result is PhoneControlResult.Value && result.value == PhoneDraftSave.SAVED) {
                    savedDrafts[key] = completed
                    if (dirtyDrafts.isEmpty()) null else R.string.phone_draft_saving
                } else if (result is PhoneControlResult.Value && result.value in setOf(PhoneDraftSave.BYTE_LIMIT, PhoneDraftSave.COUNT_LIMIT)) {
                    if (dirtyDrafts[key] == null && drafts[key] == completed.text && draftContexts[key].orEmpty() == completed.context) {
                        val previous = savedDrafts[key]
                        if (previous == null) {
                            drafts.remove(key)
                            draftSources.remove(key)
                            draftContexts.remove(key)
                        } else {
                            drafts[key] = previous.text
                            draftContexts[key] = previous.context
                        }
                        if (draftReceiver == key.first && draftTarget == key.second) {
                            changingDraft = true
                            sendInput.setText(previous?.text.orEmpty())
                            changingDraft = false
                        }
                    }
                    R.string.phone_draft_limit
                } else R.string.phone_draft_save_failed
                saveNextDraft()
                if (foreground) render()
            }
        }
    }

    private fun showDrafts() {
        if (!draftsReady) return
        showingDrafts = true
        showingOffline = false
        selectedOffline = null
        selectedDraft = null
        visibleOfflinePage = null
        conversation.removeAllViews()
        conversation.visibility = View.VISIBLE
        if (drafts.isEmpty()) conversation.addView(TextView(this).apply { text = getString(R.string.phone_draft_empty) })
        if (draftsDamaged) conversation.addView(TextView(this).apply { text = getString(R.string.phone_draft_damaged) })
        for ((key, _) in drafts) {
            val source = draftSources[key] ?: continue
            conversation.addView(Button(this).apply {
                text = getString(R.string.phone_draft_row, source.name, source.agent, source.project,
                    source.receiver, source.target.controllerId, source.target.subjectId)
                setOnClickListener {
                    selectedDraft = source
                    conversation.removeAllViews()
                    render()
                }
            })
        }
        render()
    }

    private fun addDraftContext(source: PhoneOfflineSource, page: PhoneControlReply.SessionPage, entry: PhoneControlEntry) {
        val receiver = draftReceiver ?: return
        val target = draftTarget ?: return
        val draft = draftSource ?: return
        val key = receiver to target
        if (discardingDraft == key) return
        if (key !in drafts && drafts.size >= MAX_DRAFTS) {
            sendNotice = R.string.phone_draft_limit
            render()
            return
        }
        draftSources[key] = draft
        drafts.putIfAbsent(key, "")
        val selectedPage = page.copy(entries = listOf(entry), nextBefore = 0)
        draftContexts[key] = draftContexts[key].orEmpty() + PhoneDraftSelection(source, selectedPage)
        draftContextOffset = ((draftContexts[key].orEmpty().size - 1) / DRAFT_CONTEXT_PAGE_SIZE) * DRAFT_CONTEXT_PAGE_SIZE
        queueDraftSave(key)
        render()
    }

    private fun renderDraftContext() {
        draftContextView.removeAllViews()
        val receiver = draftReceiver ?: return
        val target = draftTarget ?: return
        val key = receiver to target
        if (showingOffline || showingDrafts || period == null || sessionPage == null) {
            draftContextView.addView(TextView(this).apply { text = getString(R.string.phone_draft_detached) })
        }
        draftContextView.addView(TextView(this).apply { text = getString(R.string.phone_offline_copy_notice) })
        val selections = draftContexts[key].orEmpty()
        val lastOffset = ((selections.size - 1).coerceAtLeast(0) / DRAFT_CONTEXT_PAGE_SIZE) * DRAFT_CONTEXT_PAGE_SIZE
        draftContextOffset = minOf(draftContextOffset, lastOffset)
        val end = minOf(draftContextOffset + DRAFT_CONTEXT_PAGE_SIZE, selections.size)
        if (selections.isNotEmpty()) draftContextView.addView(TextView(this).apply {
            text = getString(R.string.phone_draft_context_range, draftContextOffset + 1, end, selections.size)
        })
        for (index in draftContextOffset until end) {
            val selected = selections[index]
            val entry = selected.page.entries.single()
            val content = when (entry) {
                is PhoneControlEntry.Message -> entry.text
                is PhoneControlEntry.Tool -> "${entry.title}\n${entry.status}\n${entry.content}"
                is PhoneControlEntry.Plan -> entry.text
                is PhoneControlEntry.Question -> entry.question + "\n" + entry.options.joinToString("\n") { "${it.label} ${it.description} ${it.preview}" }
            }
            draftContextView.addView(TextView(this).apply {
                text = getString(R.string.phone_draft_context, selected.source.name, selected.source.receiver,
                    selected.source.target.controllerId, selected.source.target.subjectId, selected.page.revision,
                    if (entry.truncated) "$content\n${getString(R.string.phone_truncated)}" else content)
                setTextIsSelectable(true)
            })
            draftContextView.addView(Button(this).apply {
                text = getString(R.string.phone_draft_remove_context)
                isEnabled = discardingDraft != key
                setOnClickListener {
                    draftContexts[key] = draftContexts[key].orEmpty().filterIndexed { number, _ -> number != index }
                    if (draftContextOffset >= draftContexts[key].orEmpty().size && draftContextOffset > 0) draftContextOffset -= DRAFT_CONTEXT_PAGE_SIZE
                    queueDraftSave(key)
                    render()
                }
            })
        }
        if (draftContextOffset > 0) draftContextView.addView(Button(this).apply {
            text = getString(R.string.phone_draft_context_previous)
            setOnClickListener { draftContextOffset -= DRAFT_CONTEXT_PAGE_SIZE; render() }
        })
        if (end < selections.size) draftContextView.addView(Button(this).apply {
            text = getString(R.string.phone_draft_context_next)
            setOnClickListener { draftContextOffset += DRAFT_CONTEXT_PAGE_SIZE; render() }
        })
    }

    private fun confirmDiscardDraft() {
        val source = draftSource ?: return
        val current = draftPeriod ?: return
        val key = source.receiver to source.target
        AlertDialog.Builder(this).setTitle(R.string.phone_draft_discard_title).setMessage(R.string.phone_draft_discard_warning)
            .setNegativeButton(android.R.string.cancel, null)
            .setPositiveButton(R.string.phone_draft_discard) { _, _ ->
                dirtyDrafts.remove(key)
                discardingDraft = key
                task({ mayContinue -> offlineResult {
                    val secret = current.key(SystemClock.elapsedRealtime()) ?: return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
                    draftStore.discard(source, secret) { mayContinue() && current.isActive(SystemClock.elapsedRealtime()) }
                    PhoneControlResult.Value(Unit)
                } }, {
                    discardingDraft = null
                    if (foreground) render(R.string.phone_draft_save_failed)
                }) { _, _ ->
                    busy = false
                    discardingDraft = null
                    if (draftPeriod !== current || !current.isActive(SystemClock.elapsedRealtime())) return@task
                    drafts.remove(key)
                    draftSources.remove(key)
                    draftContexts.remove(key)
                    savedDrafts.remove(key)
                    draftTarget = null
                    draftReceiver = null
                    draftSaveNotice = null
                    if (showingDrafts) showDrafts() else render()
                }
            }.show()
    }

    private fun clearDrafts() {
        draftGeneration.incrementAndGet()
        draftPeriod?.lock()
        draftPeriod = null
        draftsReady = false
        draftsDamaged = false
        savingDraft = false
        pendingDraftSave?.set(null)
        pendingDraftSave = null
        discardingDraft = null
        dirtyDrafts.clear()
        drafts.clear()
        savedDrafts.clear()
        draftSources.clear()
        draftContexts.clear()
        draftSource = null
        draftContextOffset = 0
        draftTarget = null
        draftReceiver = null
        selectedDraft = null
        showingDrafts = false
        draftSaveNotice = null
        handler.removeCallbacks(draftExpiryCheck)
        changingDraft = true
        sendInput.text.clear()
        changingDraft = false
        draftContextView.removeAllViews()
    }

    private fun lockDrafts() {
        clearDrafts()
        if (showingOffline && visibleOfflinePage != null) displayPage(requireNotNull(visibleOfflinePage))
        else conversation.removeAllViews()
        render(R.string.phone_locked)
    }

    private fun scheduleDraftExpiry() {
        handler.removeCallbacks(draftExpiryCheck)
        val remaining = draftPeriod?.remainingMs(SystemClock.elapsedRealtime()) ?: return
        if (remaining == 0L) lockDrafts() else handler.postDelayed(draftExpiryCheck, remaining)
    }

    private fun selectDraft() {
        val current = period
        val source = when {
            !draftsReady || draftPeriod?.isActive(SystemClock.elapsedRealtime()) != true -> null
            showingDrafts -> selectedDraft
            showingOffline -> selectedOffline?.source
            current != null && target != null && linkedRunSession == null && !showingActionHistory && !showingPrepared && !showingRuns ->
                selectedSource?.takeIf { it.target == target && it.receiver == current.grant.receiverIdentity }
                    ?: PhoneOfflineSource(current.grant.receiverIdentity, requireNotNull(target), getString(R.string.phone_open_session), "", "")
            else -> null
        }
        val selected = source?.target
        val receiver = source?.receiver
        draftSource = if (source == null) null else draftSources[source.receiver to source.target] ?: source
        if (draftTarget == selected && draftReceiver == receiver && (selected != null || sendInput.text.isEmpty())) return
        draftTarget = selected
        draftReceiver = receiver
        draftContextOffset = 0
        sendNotice = null
        changingDraft = true
        sendInput.setText(if (selected != null && receiver != null) drafts[receiver to selected].orEmpty() else "")
        changingDraft = false
    }

    private fun render(message: Int? = null) {
        selectDraft()
        statusView.text = when {
            message != null -> getString(message)
            busy -> getString(R.string.phone_busy)
            period?.isActive(SystemClock.elapsedRealtime()) == true -> getString(R.string.phone_unlocked)
            currentStatus is PhoneControlStatus.Pending -> getString(R.string.phone_pending, (currentStatus as PhoneControlStatus.Pending).value.matchingCode)
            offlinePeriod?.isActive(SystemClock.elapsedRealtime()) == true -> getString(R.string.phone_offline_unlocked)
            currentStatus is PhoneControlStatus.Granted -> getString(R.string.phone_locked)
            else -> getString(R.string.phone_disabled)
        }
        val unlocked = period?.isActive(SystemClock.elapsedRealtime()) == true
        val offlineUnlocked = offlinePeriod?.isActive(SystemClock.elapsedRealtime()) == true
        val draftUnlocked = draftPeriod?.isActive(SystemClock.elapsedRealtime()) == true && draftsReady
        val showComposer = foreground && draftUnlocked && draftSource != null
        sendInput.visibility = if (showComposer) View.VISIBLE else View.GONE
        val currentDraftKey = if (draftReceiver != null && draftTarget != null) requireNotNull(draftReceiver) to requireNotNull(draftTarget) else null
        sendInput.isEnabled = currentDraftKey == null || currentDraftKey != discardingDraft
        val saved = currentDraftKey != null && draftSource != null && savedDrafts[currentDraftKey] ==
            PhoneDraft(requireNotNull(draftSource), drafts[currentDraftKey].orEmpty(), draftContexts[currentDraftKey].orEmpty())
        sendButton.visibility = if (showComposer) View.VISIBLE else View.GONE
        sendStatus.visibility = if (showComposer) View.VISIBLE else View.GONE
        draftContextView.visibility = if (showComposer) View.VISIBLE else View.GONE
        draftDiscardButton.visibility = if (showComposer && draftReceiver != null && draftTarget != null && (draftReceiver to draftTarget) in drafts) View.VISIBLE else View.GONE
        draftDiscardButton.isEnabled = !busy
        renderDraftContext()
        val related = relatedPendingActions()
        val acceptedStates = setOf(PhoneActionState.ACCEPTED, PhoneActionState.DISPATCHED, PhoneActionState.COMPLETED)
        val resolvedStates = acceptedStates + PhoneActionState.FENCED_NO_EFFECT
        val unresolved = related.any { actionEvidence[it.operationId]?.state !in resolvedStates }
        val reviewNeeded = related.any { actionEvidence[it.operationId]?.state == PhoneActionState.FENCED_NO_EFFECT && it.operationId !in reviewedFences }
        val acceptedDraft = related.any { actionEvidence[it.operationId]?.state in acceptedStates && it.payloadHash == sendDigest() }
        val wrongTarget = draftTarget != null &&
            (draftTarget != target || draftReceiver != period?.grant?.receiverIdentity)
        sendStatus.text = getString(when {
            sendNotice == R.string.phone_draft_limit -> R.string.phone_draft_limit
            draftSaveNotice != null -> requireNotNull(draftSaveNotice)
            currentDraftKey != null && currentDraftKey in drafts && !saved -> R.string.phone_draft_save_failed
            draftReceiver != null && draftTarget != null && !draftContexts[draftReceiver to draftTarget].isNullOrEmpty() -> R.string.phone_draft_context_send_unavailable
            wrongTarget -> R.string.phone_send_other_session
            reviewNeeded -> R.string.phone_send_review
            unresolved && related.any { actionEvidence[it.operationId]?.state == PhoneActionState.REJECTED } -> R.string.phone_send_rejected
            unresolved -> sendNotice ?: R.string.phone_send_unknown
            related.any { actionEvidence[it.operationId]?.state == PhoneActionState.COMPLETED } -> R.string.phone_action_completed
            related.any { actionEvidence[it.operationId]?.state == PhoneActionState.DISPATCHED } -> R.string.phone_action_dispatched
            related.any { actionEvidence[it.operationId]?.state == PhoneActionState.ACCEPTED } -> R.string.phone_action_accepted
            related.any { actionEvidence[it.operationId]?.state == PhoneActionState.FENCED_NO_EFFECT } -> R.string.phone_send_fenced
            sendNotice in setOf(R.string.phone_send_pending, R.string.phone_send_unknown, R.string.phone_send_unsupported) -> R.string.phone_send_ready
            else -> sendNotice ?: if (saved) R.string.phone_draft_saved else R.string.phone_send_ready
        })
        sendButton.isEnabled = saved && unlocked && draftUnlocked && !showingDrafts && !busy && !savingDraft && dirtyDrafts.isEmpty() && draftSaveNotice == null &&
            draftContexts[draftReceiver to draftTarget].isNullOrEmpty() && reconciliationLoaded && !wrongTarget && !unresolved && !reviewNeeded && !acceptedDraft &&
            linkedRunSession == null && sendInput.text.isNotEmpty() && sessionPage?.status == "ready" && target != null && !showingOffline && !showingActionHistory
        enableButton.visibility = if (currentStatus is PhoneControlStatus.Granted) View.GONE else View.VISIBLE
        checkButton.visibility = if (currentStatus is PhoneControlStatus.Pending) View.VISIBLE else View.GONE
        unlockButton.visibility = if (currentStatus is PhoneControlStatus.Granted && !unlocked) View.VISIBLE else View.GONE
        offlineUnlockButton.visibility = if (!offlineUnlocked) View.VISIBLE else View.GONE
        offlineUnlockButton.isEnabled = !busy
        draftUnlockButton.visibility = if (!draftUnlocked) View.VISIBLE else View.GONE
        draftUnlockButton.isEnabled = !busy
        draftListButton.visibility = if (draftUnlocked) View.VISIBLE else View.GONE
        draftListButton.isEnabled = !busy
        offlineCopiesButton.visibility = if (offlineUnlocked) View.VISIBLE else View.GONE
        offlineCopiesButton.isEnabled = !busy
        offlineKeepButton.visibility = if (unlocked && offlineUnlocked && linkedRunSession == null && target != null && !showingOffline && !showingActionHistory) View.VISIBLE else View.GONE
        offlineKeepButton.isEnabled = !busy
        offlineOlderButton.visibility = if (offlineUnlocked && showingOffline && selectedOffline != null &&
            offlinePageIndex + 1 < (selectedOffline?.pages ?: 0)) View.VISIBLE else View.GONE
        offlineOlderButton.isEnabled = !busy
        offlineNewerButton.visibility = if (offlineUnlocked && showingOffline && selectedOffline != null && offlinePageIndex > 0) View.VISIBLE else View.GONE
        offlineNewerButton.isEnabled = !busy
        offlineUnpinButton.visibility = if (offlineUnlocked && showingOffline && selectedOffline?.pinned == true) View.VISIBLE else View.GONE
        offlineUnpinButton.isEnabled = !busy
        offlineRemoveButton.visibility = if (offlineUnlocked && showingOffline && selectedOffline != null) View.VISIBLE else View.GONE
        offlineRemoveButton.isEnabled = !busy
        offlineBudgetButton.visibility = if (offlineUnlocked && showingOffline) View.VISIBLE else View.GONE
        offlineBudgetButton.isEnabled = !busy
        lockButton.visibility = if (unlocked || offlineUnlocked || draftUnlocked) View.VISIBLE else View.GONE
        sessionsButton.visibility = if (unlocked && (target != null || showingActionHistory || showingOffline || showingDrafts || showingPrepared || showingRuns)) View.VISIBLE else View.GONE
        sessionsButton.isEnabled = !busy
        runsButton.visibility = if (unlocked && !showingOffline) View.VISIBLE else View.GONE
        runsButton.isEnabled = !busy
        preparedButton.visibility = if (unlocked && !showingOffline) View.VISIBLE else View.GONE
        preparedButton.isEnabled = !busy
        actionHistoryButton.visibility = if (unlocked && !showingOffline) View.VISIBLE else View.GONE
        actionHistoryButton.isEnabled = !busy
        refreshButton.visibility = if (unlocked && !showingOffline) View.VISIBLE else View.GONE
        refreshButton.isEnabled = !busy
        moreSessionsButton.visibility = if (unlocked && !showingOffline && !showingActionHistory && !showingPrepared && !showingRuns && target == null && browsePage?.next != null) View.VISIBLE else View.GONE
        moreSessionsButton.isEnabled = !busy
        moreRunsButton.visibility = if (unlocked && showingRuns && selectedRunId == null && !runPage?.nextAfter.isNullOrEmpty()) View.VISIBLE else View.GONE
        moreRunsButton.isEnabled = !busy
        morePreparedButton.visibility = if (unlocked && showingPrepared && selectedPrepared == null && !preparedPage?.nextAfter.isNullOrEmpty()) View.VISIBLE else View.GONE
        morePreparedButton.isEnabled = !busy
        olderButton.visibility = if (unlocked && !showingOffline && !showingActionHistory && (sessionPage?.nextBefore ?: 0) != 0L) View.VISIBLE else View.GONE
        olderButton.isEnabled = !busy
        moreActionHistoryButton.visibility = if (unlocked && !showingOffline && showingActionHistory && !actionHistoryPage?.nextAfter.isNullOrEmpty()) View.VISIBLE else View.GONE
        moreActionHistoryButton.isEnabled = !busy
        enableButton.isEnabled = !busy
        checkButton.isEnabled = !busy
        unlockButton.isEnabled = !busy
    }

    private fun unlockOffline(forDraft: Boolean = false, after: (Boolean) -> Unit) {
        val store = if (forDraft) draftUnlockStore else offlineUnlockStore
        val token = generation.incrementAndGet()
        busy = true
        render()
        networkExecutor.execute {
            val result = offlineResult { PhoneControlResult.Value(store.prepare()) }
            if (result is PhoneControlResult.Value) {
                val pending = PendingOfflinePreparation(token, result.value)
                if (!active(token) || !pendingOfflinePreparation.compareAndSet(null, pending)) result.value.discard()
                else if (!active(token) && pendingOfflinePreparation.compareAndSet(pending, null)) result.value.discard()
            }
            runOnUiThread {
                val pending = pendingOfflinePreparation.get()
                val prepared = if (pending?.token == token && pendingOfflinePreparation.compareAndSet(pending, null)) pending.value else null
                if (!active(token)) {
                    prepared?.discard()
                    return@runOnUiThread
                }
                if (result is PhoneControlResult.Failure || prepared == null) {
                    busy = false
                    render(R.string.phone_offline_failed)
                    if (foreground && forDraft && store.hasProvenDraftKeyLoss()) confirmLostDraftKey()
                    if (foreground) after(false)
                    return@runOnUiThread
                }
                preparedOfflineUnlock = prepared
                try {
                    cancellation = PhoneOfflineAuthenticator(this).start(prepared.operation, { cipher ->
                        if (!active(token)) return@start
                        cancellation = null
                        if (cipher == null || cipher !== prepared.operation) {
                            prepared.discard()
                            preparedOfflineUnlock = null
                            busy = false
                            render(R.string.phone_offline_failed)
                            if (foreground) after(false)
                        } else finishOfflineUnlock(prepared, cipher, token, after, forDraft)
                    }, {
                        if (!active(token)) return@start
                        cancellation = null
                        prepared.discard()
                        preparedOfflineUnlock = null
                        busy = false
                        render(R.string.phone_offline_not_saved)
                        if (foreground) after(false)
                    })
                } catch (_: IllegalArgumentException) {
                    prepared.discard()
                    preparedOfflineUnlock = null
                    busy = false
                    render(R.string.phone_offline_failed)
                    if (foreground) after(false)
                } catch (_: SecurityException) {
                    prepared.discard()
                    preparedOfflineUnlock = null
                    busy = false
                    render(R.string.phone_offline_failed)
                    if (foreground) after(false)
                }
            }
        }
    }

    private fun finishOfflineUnlock(prepared: PreparedPhoneOfflineUnlock, cipher: javax.crypto.Cipher, token: Long, after: (Boolean) -> Unit, forDraft: Boolean) {
        val store = if (forDraft) draftUnlockStore else offlineUnlockStore
        networkExecutor.execute {
            val result = offlineResult {
                if (!active(token)) PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
                else PhoneControlResult.Value(store.finish(prepared, cipher, SystemClock.elapsedRealtime()))
            }
            if (result is PhoneControlResult.Value) {
                val pending = PendingOfflinePeriod(token, result.value)
                if (!active(token) || !pendingOfflinePeriod.compareAndSet(null, pending)) result.value.lock()
                else if (!active(token) && pendingOfflinePeriod.compareAndSet(pending, null)) result.value.lock()
            }
            runOnUiThread {
                val pending = pendingOfflinePeriod.get()
                val unlocked = if (pending?.token == token && pendingOfflinePeriod.compareAndSet(pending, null)) pending.value else null
                if (!active(token)) {
                    unlocked?.lock()
                    return@runOnUiThread
                }
                preparedOfflineUnlock = null
                busy = false
                if (result is PhoneControlResult.Failure || unlocked == null) {
                    render(R.string.phone_offline_failed)
                    if (foreground && forDraft && store.hasProvenDraftKeyLoss()) confirmLostDraftKey()
                    if (foreground) after(false)
                    return@runOnUiThread
                }
                if (forDraft) {
                    draftPeriod?.lock()
                    draftPeriod = unlocked
                    scheduleDraftExpiry()
                } else {
                    offlinePeriod?.lock()
                    offlinePeriod = unlocked
                    scheduleOfflineExpiry()
                    schedulePinRefresh()
                }
                render()
                if (foreground) after(true)
            }
        }
    }

    private fun confirmLostDraftKey() {
        AlertDialog.Builder(this).setTitle(R.string.phone_draft_key_lost_title).setMessage(R.string.phone_draft_key_lost_message)
            .setNegativeButton(android.R.string.cancel, null)
            .setPositiveButton(R.string.phone_draft_key_delete) { _, _ ->
                task({ mayContinue -> offlineResult {
                    if (!mayContinue()) return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
                    draftUnlockStore.resetLostDraftKey()
                    PhoneControlResult.Value(Unit)
                } }, { if (foreground) render(R.string.phone_draft_save_failed) }) { _, _ ->
                    busy = false
                    if (foreground) render(R.string.phone_draft_key_reset)
                }
            }.show()
    }

    private fun showOfflineCopies() {
        val current = offlinePeriod ?: return
        clearRunSession()
        attentionRunId = null
        attentionResultId = null
        clearRuns()
        clearPrepared()
        showingOffline = true
        showingDrafts = false
        selectedDraft = null
        selectedOffline = null
        handler.removeCallbacks(refreshPoll)
        task({ mayContinue -> offlineResult {
            val key = current.key(SystemClock.elapsedRealtime()) ?: return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
            if (!foreground || !mayContinue()) return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
            val maintenance = offlineStore.maintainAll(offlineBudget(), System.currentTimeMillis(), key)
            val listing = offlineStore.listAll(key)
            PhoneControlResult.Value(maintenance to listing)
        } }, { if (foreground) render(R.string.phone_offline_failed) }) { result, _ ->
            busy = false
            if (!foreground || offlinePeriod !== current || !current.isActive(SystemClock.elapsedRealtime())) return@task
            val (maintenance, listing) = result
            render(if (listing.damaged.isNotEmpty()) R.string.phone_offline_failed else offlineSaveMessage(maintenance))
            conversation.removeAllViews()
            conversation.visibility = View.VISIBLE
            if (listing.copies.isEmpty()) conversation.addView(TextView(this).apply { text = getString(R.string.phone_offline_empty) })
            for (copy in listing.copies) {
                conversation.addView(Button(this).apply {
                    text = getString(R.string.phone_offline_row, copy.source.name, copy.source.agent, copy.source.project,
                        DateFormat.getDateTimeInstance().format(Date(copy.refreshedAtMs)),
                        if (copy.pinned) getString(R.string.phone_offline_pinned) else "",
                        if (copy.incomplete || copy.refreshFailed) getString(R.string.phone_offline_incomplete) else "")
                    setOnClickListener {
                        selectedOffline = copy
                        offlinePageIndex = 0
                        openOfflinePage(0, true)
                    }
                })
            }
            for (id in listing.damaged.distinct()) {
                conversation.addView(Button(this).apply {
                    text = getString(R.string.phone_offline_remove_damaged, id.take(12))
                    setOnClickListener { confirmRemoveDamaged(id) }
                })
            }
            conversation.addView(TextView(this).apply { text = getString(R.string.phone_offline_unsupported) })
        }
    }

    private fun confirmRemoveDamaged(id: String) {
        AlertDialog.Builder(this).setTitle(R.string.phone_offline_remove_damaged_title)
            .setMessage(R.string.phone_offline_remove_damaged_warning)
            .setNegativeButton(android.R.string.cancel, null)
            .setPositiveButton(R.string.phone_offline_remove) { _, _ ->
                val current = offlinePeriod ?: return@setPositiveButton
                task({ mayContinue -> offlineResult {
                    val key = current.key(SystemClock.elapsedRealtime())
                        ?: return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
                    if (!foreground || !mayContinue()) return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
                    offlineStore.removeDamaged(id, key)
                    PhoneControlResult.Value(Unit)
                } }, { if (foreground) render(R.string.phone_offline_failed) }) { _, _ ->
                    busy = false
                    if (foreground && offlinePeriod === current) showOfflineCopies()
                }
            }.show()
    }

    private fun openOfflinePage(index: Int, markOpened: Boolean) {
        val current = offlinePeriod ?: return
        val copy = selectedOffline ?: return
        if (index !in 0 until copy.pages) return
        task({ mayContinue -> offlineResult {
            val key = current.key(SystemClock.elapsedRealtime()) ?: return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
            if (!foreground || !mayContinue()) return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
            val page = offlineStore.readPage(copy.source, index, key)
            if (!foreground || !mayContinue() || !current.isActive(SystemClock.elapsedRealtime())) {
                return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
            }
            if (markOpened) offlineStore.markOpened(copy.source, System.currentTimeMillis(), key)
            PhoneControlResult.Value(page)
        } }, { if (foreground) render(R.string.phone_offline_failed) }) { page, _ ->
            busy = false
            if (!foreground || offlinePeriod !== current || !current.isActive(SystemClock.elapsedRealtime())) return@task
            offlinePageIndex = index
            visibleOfflinePage = page
            render()
            displayPage(page)
            conversation.addView(TextView(this).apply { text = getString(R.string.phone_offline_stale) }, 0)
            conversation.addView(TextView(this).apply {
                text = getString(R.string.phone_offline_source, copy.source.name, copy.source.agent, copy.source.project,
                    copy.source.receiver, copy.source.target.controllerId, copy.source.target.subjectId,
                    DateFormat.getDateTimeInstance().format(Date(copy.refreshedAtMs)))
            }, 1)
            if (copy.incomplete || copy.refreshFailed) conversation.addView(TextView(this).apply { text = getString(R.string.phone_offline_incomplete) }, 2)
            conversation.addView(TextView(this).apply { text = getString(R.string.phone_offline_copy_notice) })
            conversation.visibility = View.VISIBLE
        }
    }

    private fun updateOfflineCopy(remove: Boolean) {
        val current = offlinePeriod ?: return
        val copy = selectedOffline ?: return
        task({ mayContinue -> offlineResult {
            val key = current.key(SystemClock.elapsedRealtime()) ?: return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
            if (!foreground || !mayContinue()) return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
            if (remove) offlineStore.remove(copy.source) else offlineStore.setPinned(copy.source, false, key)
            PhoneControlResult.Value(Unit)
        } }, { if (foreground) render(R.string.phone_offline_failed) }) { _, _ ->
            busy = false
            if (foreground && offlinePeriod === current) showOfflineCopies()
        }
    }

    private fun offlineBudget(): Long {
        val budget = getSharedPreferences("phone-offline-settings-v1", MODE_PRIVATE)
            .getLong("budget", PHONE_OFFLINE_DEFAULT_BUDGET)
        require(budget > 0)
        return budget
    }

    private fun changeOfflineBudget() {
        val input = EditText(this).apply {
            inputType = InputType.TYPE_CLASS_NUMBER
            setText(String.format(Locale.ROOT, "%d", offlineBudget() / MEBIBYTE))
        }
        AlertDialog.Builder(this).setTitle(R.string.phone_offline_budget_title).setView(input)
            .setNegativeButton(android.R.string.cancel, null)
            .setPositiveButton(R.string.phone_offline_budget_apply) { _, _ ->
                val amount = input.text.toString().toLongOrNull()
                if (amount == null || amount <= 0 || amount > Long.MAX_VALUE / MEBIBYTE) {
                    render(R.string.phone_offline_budget_invalid)
                } else saveOfflineBudget(amount * MEBIBYTE)
            }.show()
    }

    @android.annotation.SuppressLint("UseKtx") // A confirmed limit must be durably stored before it is reported.
    private fun saveOfflineBudget(budget: Long) {
        val current = offlinePeriod ?: return
        task({ mayContinue -> offlineResult {
            val key = current.key(SystemClock.elapsedRealtime()) ?: return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
            if (!foreground || !mayContinue()) return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
            val result = offlineStore.maintainAll(budget, System.currentTimeMillis(), key)
            if (result == PhoneOfflineSave.SAVED &&
                !getSharedPreferences("phone-offline-settings-v1", MODE_PRIVATE).edit().putLong("budget", budget).commit()) {
                throw IOException("offline limit could not be stored")
            }
            PhoneControlResult.Value(result)
        } }, { if (foreground) render(R.string.phone_offline_failed) }) { result, _ ->
            busy = false
            render(offlineSaveMessage(result))
        }
    }

    private fun offlineSaveMessage(result: PhoneOfflineSave): Int? = when (result) {
        PhoneOfflineSave.SAVED -> null
        PhoneOfflineSave.BYTE_LIMIT -> R.string.phone_offline_full
        PhoneOfflineSave.SESSION_LIMIT -> R.string.phone_offline_count
        PhoneOfflineSave.DAMAGED, PhoneOfflineSave.CANCELLED -> R.string.phone_offline_failed
        PhoneOfflineSave.KEEP_EXISTING -> R.string.phone_offline_incomplete
        PhoneOfflineSave.REVOKED -> R.string.phone_revoked
    }

    private fun keepOffline() {
        if (linkedRunSession != null) return
        val online = period ?: return
        val local = offlinePeriod ?: return
        val requested = target ?: return
        val source = selectedSource?.takeIf { it.target == requested && it.receiver == online.grant.receiverIdentity }
            ?: PhoneOfflineSource(online.grant.receiverIdentity, requested, getString(R.string.phone_open_session), "", "")
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding() ?: return
        task({ mayContinue -> offlineResult {
            val key = local.key(SystemClock.elapsedRealtime()) ?: return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
            downloadPhoneOffline(offlineStore, source, key, offlineBudget(), System.currentTimeMillis(), true,
                { foreground && mayContinue() && pairing.binding() == binding && online.isActive(SystemClock.elapsedRealtime()) &&
                    local.isActive(SystemClock.elapsedRealtime()) },
                { count -> runOnUiThread {
                    if (foreground && mayContinue() && local.isActive(SystemClock.elapsedRealtime())) {
                        statusView.text = resources.getQuantityString(R.plurals.phone_offline_progress, count, count)
                    }
                } },
            ) { cursor, revision, interaction ->
                client.readSession(online, requested, SystemClock.elapsedRealtime(), before = cursor, limit = 50,
                    expectedRevision = revision, interaction = interaction) {
                    foreground && mayContinue() && pairing.binding() == binding && online.isActive(SystemClock.elapsedRealtime()) &&
                        local.isActive(SystemClock.elapsedRealtime())
                }
            }
        } }, { outcome -> if (foreground) showFailure(outcome) }) { result, _ ->
            busy = false
            if (!foreground || offlinePeriod !== local || period !== online) return@task
            val message = when {
                result.save != PhoneOfflineSave.SAVED -> offlineSaveMessage(result.save)
                result.interruption != null -> R.string.phone_offline_partial
                else -> R.string.phone_offline_saved
            }
            render(message)
        }
    }

    private inline fun <T> offlineResult(block: () -> PhoneControlResult<T>): PhoneControlResult<T> = try {
        block()
    } catch (_: GeneralSecurityException) {
        PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
    } catch (_: IOException) {
        PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
    } catch (_: SecurityException) {
        PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
    } catch (_: ProviderException) {
        PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
    } catch (_: IllegalArgumentException) {
        PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED)
    } catch (_: IllegalStateException) {
        PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
    } catch (_: JSONException) {
        PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED)
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
                        resetReconciliation()
                        scheduleExpiry()
                        schedulePinRefresh()
                        scheduleReconciliation(0)
                        render()
                        val continueOnline = {
                            if (showingRuns && selectedRunId != null) withNetwork { readSelectedRun() }
                            else if (target != null) withNetwork { readTarget() }
                            else if (!attentionNavigation) withNetwork { readSessions() }
                        }
                        val openDrafts = {
                            if (draftPeriod?.isActive(SystemClock.elapsedRealtime()) == true && draftsReady) continueOnline()
                            else unlockOffline(forDraft = true) { unlocked ->
                                if (unlocked) loadDrafts { continueOnline() } else continueOnline()
                            }
                        }
                        if (offlinePeriod?.isActive(SystemClock.elapsedRealtime()) == true) openDrafts()
                        else unlockOffline { openDrafts() }
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

    private fun sendDigest(): String? = try {
            if (sendInput.text.isEmpty()) null else phoneControlHex(
                MessageDigest.getInstance("SHA-256").digest(phoneControlSendPayload(sendInput.text.toString())),
            )
        } catch (_: IllegalArgumentException) {
            null
        }

    private fun relatedPendingActions(): List<PendingPhoneAction> {
        val selected = target
        val digest = sendDigest()
        return pendingActions.filter {
            (selected != null && it.controllerId == selected.controllerId && it.subjectId == selected.subjectId) ||
                (digest != null && it.payloadHash == digest)
        }
    }

    private fun sendPrompt() {
        if (linkedRunSession != null) return
        if (!sendButton.isEnabled) return
        if (sendDigest() == null) {
            sendNotice = R.string.phone_send_invalid
            render()
            return
        }
        val current = period ?: return
        val requested = target ?: return
        val revision = sessionPage?.revision ?: return
        val text = sendInput.text.toString()
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding() ?: return
        reconciliationLoaded = false
        task({ maySend ->
            client.sendPrompt(current, requested, revision, text, pendingActionsStore, SystemClock.elapsedRealtime()) {
                foreground && maySend() && pairing.binding() == binding && current.isActive(SystemClock.elapsedRealtime())
            }
        }, { outcome ->
            if (!foreground) return@task
            if (outcome == PhoneControlOutcome.REVOKED || outcome == PhoneControlOutcome.UNAUTHENTICATED ||
                outcome == PhoneControlOutcome.EXPIRED) {
                lockWorkspace()
                showFailure(outcome)
                return@task
            }
            sendNotice = R.string.phone_send_unavailable
            render()
            scheduleReconciliation(0)
        }) { attempt, _ ->
            busy = false
            if (!foreground || period !== current || pairing.binding() != binding || !current.isActive(SystemClock.elapsedRealtime())) return@task
            pendingActions = (pendingActions.filterNot { it.operationId == attempt.pending.operationId } + attempt.pending)
            sendNotice = when (attempt.outcome) {
                PhoneControlOutcome.UNSUPPORTED -> R.string.phone_send_unsupported
                null -> R.string.phone_send_pending
                else -> R.string.phone_send_unknown
            }
            if (attempt.outcome == PhoneControlOutcome.REVOKED || attempt.outcome == PhoneControlOutcome.UNAUTHENTICATED ||
                attempt.outcome == PhoneControlOutcome.EXPIRED) {
                lockWorkspace()
                showFailure(attempt.outcome)
                return@task
            }
            render()
            scheduleReconciliation(0)
        }
    }

    private fun scheduleReconciliation(delay: Long = RECONCILE_INTERVAL_MS) {
        handler.removeCallbacks(reconcilePoll)
        if (foreground && period?.isActive(SystemClock.elapsedRealtime()) == true) {
            handler.postDelayed(reconcilePoll, delay)
        }
    }

    private fun resetReconciliation() {
        pendingActions = emptyList()
        actionEvidence.clear()
        reviewedFences.clear()
        reconciliationLoaded = false
        reconciliationCursor = null
    }

    /** Check a bounded rotating batch; it observes history and never transmits a Send. */
    private fun reconcilePendingActions() {
        val current = period ?: return
        if (!foreground || !current.isActive(SystemClock.elapsedRealtime())) return
        if (busy || !hasReceiverNetworkAccess(this)) {
            scheduleReconciliation()
            return
        }
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding() ?: return
        val token = generation.get()
        val prior = actionEvidence.toMap()
        val cursor = reconciliationCursor
        networkExecutor.execute {
            val mayContinue = {
                foreground && active(token) && pairing.binding() == binding && current.isActive(SystemClock.elapsedRealtime())
            }
            val pending = try {
                if (mayContinue()) pendingActionsStore.pending().filter { it.receiverIdentity == current.grant.receiverIdentity }
                    .sortedBy { it.operationId } else null
            } catch (_: PendingPhoneActionStoreException) {
                null
            }
            val candidates = pending.orEmpty().filter {
                prior[it.operationId]?.state != PhoneActionState.FENCED_NO_EFFECT
            }
            val selected = (candidates.filter { cursor == null || it.operationId > cursor } +
                candidates.filter { cursor != null && it.operationId <= cursor }).take(RECONCILE_BATCH)
            val records = mutableListOf<PhoneActionRecord>()
            var failure: PhoneControlOutcome? = null
            for (item in selected) {
                if (!mayContinue()) break
                when (val result = client.readActionHistory(current, item, SystemClock.elapsedRealtime(), mayContinue)) {
                    is PhoneControlResult.Value -> records.add(result.value)
                    is PhoneControlResult.Failure -> if (result.outcome in setOf(
                            PhoneControlOutcome.EXPIRED, PhoneControlOutcome.REVOKED, PhoneControlOutcome.UNAUTHENTICATED,
                        )) {
                        failure = result.outcome
                        break
                    }
                }
            }
            val result = PhoneReconciliation(pending, records, selected.lastOrNull()?.operationId, failure)
            runOnUiThread {
                if (!foreground || !active(token) || period !== current || pairing.binding() != binding ||
                    !current.isActive(SystemClock.elapsedRealtime())) {
                    scheduleReconciliation()
                    return@runOnUiThread
                }
                if (result.failure != null) {
                    lockWorkspace()
                    return@runOnUiThread
                }
                reconciliationLoaded = true
                if (result.pending != null) {
                    pendingActions = result.pending
                    reconciliationCursor = result.nextAfter
                    for (record in result.records) actionEvidence[record.operationId] = record
                }
                render()
                scheduleReconciliation()
            }
        }
    }

    private fun clearRunSession() {
        if (linkedRunSession != null) target = null
        linkedRunSession = null
    }

    private fun clearRuns() {
        if (showingRuns) conversation.removeAllViews()
        showingRuns = false
        runPage = null
        runAfter = null
        selectedRunId = null
        selectedSavedResult = null
    }

    private fun clearRunsForLock() {
        clearRuns()
        attentionRunId?.let { showingRuns = true; selectedRunId = it; selectedSavedResult = attentionResultId }
    }

    private fun showRuns() {
        clearRunSession()
        handler.removeCallbacks(refreshPoll)
        attentionRunId = null
        attentionResultId = null
        clearPrepared()
        showingOffline = false
        showingActionHistory = false
        actionHistoryPage = null
        target = null
        selectedSource = null
        openedPages = emptyList()
        sessionPage = null
        browsePage = null
        showingDrafts = false
        selectedDraft = null
        showingRuns = true
        runPage = null
        runAfter = null
        selectedRunId = null
        selectedSavedResult = null
        conversation.removeAllViews()
        readRunPage()
    }

    private fun readSelectedRun(interaction: Boolean = true) {
        val run = selectedRunId
        val result = selectedSavedResult
        if (run == null) readRunPage(runAfter, interaction)
        else if (result == null) readRun(run, interaction = interaction)
        else readSavedReview(run, result, interaction = interaction)
    }

    private fun readRunPage(after: String? = null, interaction: Boolean = true) {
        val current = period ?: return
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding()
        if (binding == null) {
            lockWorkspace()
            render(R.string.phone_unavailable)
            return
        }
        task({ maySend ->
            client.listRuns(current, SystemClock.elapsedRealtime(), after, interaction) {
                foreground && maySend() && pairing.binding() == binding && current.isActive(SystemClock.elapsedRealtime())
            }
        }, { outcome -> if (foreground) showFailure(outcome) }) { page, _ ->
            busy = false
            if (!foreground) return@task
            val now = SystemClock.elapsedRealtime()
            if (pairing.binding() != binding || period !== current ||
                !(if (interaction) current.confirmInteraction(now) else current.isActive(now))) {
                lockWorkspace()
                return@task
            }
            runPage = page
            runAfter = after
            selectedRunId = null
            selectedSavedResult = null
            scheduleExpiry()
            render()
            conversation.removeAllViews()
            conversation.visibility = View.VISIBLE
            if (page.items.isEmpty()) conversation.addView(TextView(this).apply { text = getString(R.string.phone_no_runs) })
            for (item in page.items) {
                conversation.addView(Button(this).apply {
                    text = getString(R.string.phone_run_row, item.runId, item.revision, phoneRunState(this@PhoneControlActivity, item.state))
                    setOnClickListener {
                        selectedRunId = item.runId
                        runPage = null
                        conversation.removeAllViews()
                        withNetwork { readRun(item.runId, item.revision) }
                    }
                })
            }
        }
    }

    private fun readRun(runId: String, expectedRevision: Long = 0, interaction: Boolean = true) {
        val current = period ?: return
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding()
        if (binding == null) {
            lockWorkspace()
            render(R.string.phone_unavailable)
            return
        }
        task({ maySend ->
            val permitted = { foreground && maySend() && pairing.binding() == binding && current.isActive(SystemClock.elapsedRealtime()) }
            when (val detail = client.readRun(current, runId, expectedRevision, SystemClock.elapsedRealtime(), interaction, permitted)) {
                is PhoneControlResult.Failure -> detail
                is PhoneControlResult.Value -> {
                    when (val live = client.readRunLive(current, runId, detail.value.run.summary.revision,
                        SystemClock.elapsedRealtime(), false, permitted)) {
                        is PhoneControlResult.Value -> PhoneControlResult.Value(detail.value to live.value)
                        is PhoneControlResult.Failure -> if (live.outcome == PhoneControlOutcome.UNAVAILABLE)
                            PhoneControlResult.Value(detail.value to null) else live
                    }
                }
            }
        }, { outcome -> if (foreground) showFailure(outcome) }) { result, _ ->
            busy = false
            if (!foreground || !showingRuns || selectedRunId != runId) return@task
            val now = SystemClock.elapsedRealtime()
            if (pairing.binding() != binding || period !== current ||
                !(if (interaction) current.confirmInteraction(now) else current.isActive(now))) {
                lockWorkspace()
                return@task
            }
            scheduleExpiry()
            render()
            conversation.removeAllViews()
            for (view in phoneRunDetailViews(this, result.first.run)) conversation.addView(view)
            for (view in phoneRunLiveViews(this, result.second) { openRunSession(it) }) conversation.addView(view)
            conversation.visibility = View.VISIBLE
        }
    }

    private fun readSavedReview(runId: String, resultId: String, revision: String? = null,
        cursor: PhoneSavedCursor? = null, interaction: Boolean = true) {
        val current = period ?: return
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding()
        if (binding == null) {
            lockWorkspace()
            render(R.string.phone_unavailable)
            return
        }
        conversation.removeAllViews()
        task({ maySend ->
            client.readSavedReview(current, runId, resultId, revision, cursor, SystemClock.elapsedRealtime(), interaction) {
                foreground && maySend() && pairing.binding() == binding && current.isActive(SystemClock.elapsedRealtime())
            }
        }, { outcome -> if (foreground) showFailure(outcome) }) { reply, _ ->
            busy = false
            if (!foreground || !showingRuns || selectedRunId != runId || selectedSavedResult != resultId) return@task
            val now = SystemClock.elapsedRealtime()
            if (pairing.binding() != binding || period !== current ||
                !(if (interaction) current.confirmInteraction(now) else current.isActive(now))) {
                lockWorkspace()
                return@task
            }
            scheduleExpiry()
            render()
            conversation.removeAllViews()
            for (view in phoneSavedReviewViews(this, reply.review) { next ->
                withNetwork { readSavedReview(runId, resultId, reply.review.revision, next) }
            }) conversation.addView(view)
            conversation.visibility = View.VISIBLE
        }
    }

    private fun openRunSession(selected: PhoneRunSessionTarget) {
        val current = period ?: return
        if (!foreground || !showingRuns || selectedRunId != selected.runId || !current.isActive(SystemClock.elapsedRealtime())) return
        clearRuns()
        linkedRunSession = selected
        target = selected.session
        selectedSource = null
        openedPages = emptyList()
        sessionOpenedAtMs = null
        sessionPage = null
        newerOmitted = false
        historyChanged = false
        conversation.removeAllViews()
        withNetwork { readTarget() }
    }

    private fun showSessions() {
        clearRunSession()
        handler.removeCallbacks(refreshPoll)
        attentionRunId = null
        attentionResultId = null
        clearRuns()
        clearPrepared()
        showingOffline = false
        showingDrafts = false
        selectedDraft = null
        selectedOffline = null
        showingActionHistory = false
        actionHistoryPage = null
        target = null
        selectedSource = null
        openedPages = emptyList()
        sessionOpenedAtMs = null
        sessionPage = null
        newerOmitted = false
        historyChanged = false
        conversation.removeAllViews()
        readSessions()
    }

    private fun clearPrepared() {
        if (showingPrepared) conversation.removeAllViews()
        showingPrepared = false
        preparedPage = null
        preparedAfter = null
        selectedPrepared = null
    }

    private fun showPrepared() {
        clearRunSession()
        handler.removeCallbacks(refreshPoll)
        attentionRunId = null
        attentionResultId = null
        clearRuns()
        showingOffline = false
        showingActionHistory = false
        actionHistoryPage = null
        target = null
        selectedSource = null
        openedPages = emptyList()
        sessionPage = null
        browsePage = null
        showingDrafts = false
        selectedDraft = null
        showingPrepared = true
        preparedPage = null
        preparedAfter = null
        selectedPrepared = null
        conversation.removeAllViews()
        readPreparedPage()
    }

    private fun readPreparedPage(after: String? = null, interaction: Boolean = true) {
        val current = period ?: return
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding()
        if (binding == null) {
            lockWorkspace()
            render(R.string.phone_unavailable)
            return
        }
        task({ maySend ->
            client.listPrepared(current, SystemClock.elapsedRealtime(), after, interaction) {
                foreground && maySend() && pairing.binding() == binding && current.isActive(SystemClock.elapsedRealtime())
            }
        }, { outcome -> if (foreground) showFailure(outcome) }) { page, _ ->
            busy = false
            if (!foreground) return@task
            val now = SystemClock.elapsedRealtime()
            if (pairing.binding() != binding || period !== current ||
                !(if (interaction) current.confirmInteraction(now) else current.isActive(now))) {
                lockWorkspace()
                return@task
            }
            preparedPage = page
            preparedAfter = after
            selectedPrepared = null
            scheduleExpiry()
            render()
            conversation.removeAllViews()
            conversation.visibility = View.VISIBLE
            if (page.items.isEmpty()) conversation.addView(TextView(this).apply { text = getString(R.string.phone_no_prepared) })
            for (item in page.items) {
                conversation.addView(Button(this).apply {
                    text = getString(R.string.phone_prepared_row, item.runId, item.revision)
                    setOnClickListener {
                        selectedPrepared = item
                        preparedPage = null
                        conversation.removeAllViews()
                        withNetwork { readPreparedDetail(item) }
                    }
                })
            }
        }
    }

    private fun readPreparedDetail(selected: PhonePreparedSummary, interaction: Boolean = true) {
        val current = period ?: return
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding()
        if (binding == null) {
            lockWorkspace()
            render(R.string.phone_unavailable)
            return
        }
        task({ maySend ->
            client.readPrepared(current, selected, SystemClock.elapsedRealtime(), interaction) {
                foreground && maySend() && pairing.binding() == binding && current.isActive(SystemClock.elapsedRealtime())
            }
        }, { outcome -> if (foreground) showFailure(outcome) }) { detail, _ ->
            busy = false
            if (!foreground) return@task
            val now = SystemClock.elapsedRealtime()
            if (pairing.binding() != binding || period !== current ||
                !(if (interaction) current.confirmInteraction(now) else current.isActive(now))) {
                lockWorkspace()
                return@task
            }
            scheduleExpiry()
            render()
            conversation.removeAllViews()
            for (view in phonePreparedDetailViews(this, selected, detail.detail)) conversation.addView(view)
            conversation.visibility = View.VISIBLE
        }
    }

    private fun readActionHistory(after: String? = null) {
        val current = period ?: return
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding()
        if (binding == null) {
            lockWorkspace()
            render(R.string.phone_unavailable)
            return
        }
        attentionRunId = null
        attentionResultId = null
        clearRunSession()
        clearRuns()
        clearPrepared()
        showingDrafts = false
        selectedDraft = null
        showingActionHistory = true
        handler.removeCallbacks(refreshPoll)
        task({ maySend ->
            client.readActionHistoryPage(current, SystemClock.elapsedRealtime(), after = after) {
                foreground && maySend() && pairing.binding() == binding && current.isActive(SystemClock.elapsedRealtime())
            }
        }, { outcome -> if (foreground) showFailure(outcome) }) { page, _ ->
            busy = false
            if (!foreground) return@task
            if (pairing.binding() != binding || period !== current || !current.isActive(SystemClock.elapsedRealtime())) {
                lockWorkspace()
                return@task
            }
            actionHistoryPage = page
            scheduleExpiry()
            render()
            conversation.removeAllViews()
            conversation.visibility = View.VISIBLE
            if (page.records.isEmpty()) {
                conversation.addView(TextView(this).apply { text = getString(R.string.phone_no_actions) })
            }
            for (record in page.records) {
                val state = when (record.state) {
                    PhoneActionState.UNKNOWN -> when {
                        record.dispatchedEvidence.isNotEmpty() -> R.string.phone_action_dispatched_unknown
                        record.acceptedEvidence.isNotEmpty() -> R.string.phone_action_accepted_unknown
                        else -> R.string.phone_action_unknown
                    }
                    PhoneActionState.ACCEPTED -> R.string.phone_action_accepted
                    PhoneActionState.DISPATCHED -> R.string.phone_action_dispatched
                    PhoneActionState.COMPLETED -> R.string.phone_action_completed
                    PhoneActionState.REJECTED -> R.string.phone_action_rejected
                    PhoneActionState.FENCED_NO_EFFECT -> R.string.phone_action_fenced
                }
                conversation.addView(TextView(this).apply {
                    text = getString(R.string.phone_action_row, record.action, getString(state), record.operationId)
                    textSize = 16f
                })
            }
        }
    }

    private fun readSessions(cursor: PhoneBrowseCursor? = null, interaction: Boolean = true) {
        val current = period ?: return
        showingDrafts = false
        selectedDraft = null
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
            if (!reconciliationLoaded || pendingActions.isNotEmpty()) scheduleReconciliation(0)
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
                        clearRunSession()
                        target = row.target
                        selectedSource = PhoneOfflineSource(current.grant.receiverIdentity, row.target,
                            row.summary.name, row.summary.agent, row.summary.project)
                        openedPages = emptyList()
                        sessionOpenedAtMs = null
                        sessionPage = null
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
        val linked = linkedRunSession
        if (linked != null && linked.session != requested) {
            showFailure(PhoneControlOutcome.STALE_SUBJECT)
            return
        }
        val local = if (linked == null) offlinePeriod else null
        val storedSource = selectedSource?.takeIf { it.target == requested && it.receiver == current.grant.receiverIdentity }
            ?: PhoneOfflineSource(current.grant.receiverIdentity, requested, getString(R.string.phone_open_session), "", "")
        if (interaction && !older) sessionOpenedAtMs = System.currentTimeMillis()
        val openedAt = sessionOpenedAtMs
        val priorPages = openedPages
        val previous = sessionPage
        val before = if (older) previous?.nextBefore ?: 0 else 0
        val rebuild = !older && !interaction && !historyChanged
        val hadGap = historyChanged
        if (older && before == 0L) return
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding()
        if (binding == null) {
            lockWorkspace()
            render(R.string.phone_unavailable)
            return
        }
        task({ maySend ->
            val fence = if (interaction && local != null) offlineResult {
                PhoneControlResult.Value(offlineStore.sourceFence(storedSource))
            } else null
            fun read(cursor: Long, revision: Long, limit: Int, userInteraction: Boolean): PhoneControlResult<PhoneControlReply.SessionPage> {
                val permitted = { foreground && maySend() && pairing.binding() == binding && current.isActive(SystemClock.elapsedRealtime()) }
                return if (linked == null) client.readSession(current, requested, SystemClock.elapsedRealtime(), before = cursor,
                    limit = limit, expectedRevision = revision, interaction = userInteraction, maySend = permitted)
                else client.readRunSession(current, linked, SystemClock.elapsedRealtime(), before = cursor,
                    limit = limit, expectedRevision = revision, interaction = userInteraction, maySend = permitted)
            }
            when (val first = read(before, if (older) previous?.revision ?: 0 else 0, 50, interaction)) {
                is PhoneControlResult.Failure -> first
                is PhoneControlResult.Value -> {
                    val refresh = if (rebuild) reconcilePhoneSessionPages(first.value, previous, HISTORY_WINDOW,
                        { cursor, revision, limit -> read(cursor, revision, limit, false) },
                        { read(0, 0, 50, false) })
                    else PhoneControlResult.Value(PhoneSessionRefresh(first.value, !interaction && hadGap))
                    when (refresh) {
                        is PhoneControlResult.Failure -> refresh
                        is PhoneControlResult.Value -> {
                            val pages = if (!interaction) priorPages else if (older) (priorPages + first.value).take(HISTORY_WINDOW / 50)
                                else listOf(first.value)
                            var message: Int? = null
                            if (interaction && (!older || priorPages.isNotEmpty())) {
                                if (linked != null) message = R.string.phone_run_session_read_only
                                else if (local == null || openedAt == null) message = R.string.phone_offline_not_saved
                                else {
                                    val save = offlineResult {
                                        val key = local.key(SystemClock.elapsedRealtime())
                                            ?: return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
                                        if (!foreground || !maySend() || fence !is PhoneControlResult.Value ||
                                            !offlineStore.allowSource(storedSource, fence.value)) {
                                            return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
                                        }
                                        PhoneControlResult.Value(offlineStore.save(storedSource, pages.asSequence(), false, openedAt,
                                            System.currentTimeMillis(), offlineBudget(), key) {
                                            foreground && maySend() && pairing.binding() == binding &&
                                                current.isActive(SystemClock.elapsedRealtime()) && local.isActive(SystemClock.elapsedRealtime())
                                        })
                                    }
                                    message = when (save) {
                                        is PhoneControlResult.Failure -> R.string.phone_offline_failed
                                        is PhoneControlResult.Value -> if (save.value == PhoneOfflineSave.KEEP_EXISTING) null else offlineSaveMessage(save.value)
                                    }
                                }
                            }
                            PhoneControlResult.Value(PhoneTargetRead(refresh.value, pages, message))
                        }
                    }
                }
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
        }) { result, _ ->
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
            val refresh = result.refresh
            val page = refresh.page
            val unchanged = !older && !interaction && !refresh.missingOlder && previous != null && page.revision == previous.revision
            if (older) {
                if (previous == null || page.revision != previous.revision || page.status != previous.status ||
                    page.entries.any { it.seq >= before } || (page.nextBefore != 0L && page.nextBefore >= before) ||
                    (page.entries.isNotEmpty() && previous.entries.isNotEmpty() &&
                        page.entries.last().seq >= previous.entries.first().seq)) {
                    showFailure(PhoneControlOutcome.MALFORMED)
                    return@task
                }
                val combined = page.entries + previous.entries
                val source = page.source.merge(previous.source)
                if (source == null) {
                    showFailure(PhoneControlOutcome.MALFORMED)
                    return@task
                }
                newerOmitted = newerOmitted || combined.size > HISTORY_WINDOW
                sessionPage = page.copy(entries = combined.take(HISTORY_WINDOW), complete = page.complete && previous.complete, source = source)
                historyChanged = false
            } else if (interaction || previous == null || refresh.missingOlder || page.revision != previous.revision) {
                historyChanged = if (interaction) false else refresh.missingOlder
                sessionPage = page
                newerOmitted = false
            }
            if (interaction) openedPages = result.pages
            if (interaction) {
                for (action in relatedPendingActions()) {
                    if (actionEvidence[action.operationId]?.state == PhoneActionState.FENCED_NO_EFFECT) reviewedFences.add(action.operationId)
                }
            }
            scheduleExpiry()
            if (!reconciliationLoaded || pendingActions.isNotEmpty()) scheduleReconciliation(0)
            render(result.saveMessage)
            conversation.visibility = View.VISIBLE
            if (!unchanged) displayPage(requireNotNull(sessionPage))
            scheduleRefresh()
        }
    }

    private fun displayPage(page: PhoneControlReply.SessionPage) {
        conversation.removeAllViews()
        fun line(value: String) {
            conversation.addView(TextView(this).apply { text = value; textSize = 16f; setTextIsSelectable(true) })
        }
        line(getString(R.string.phone_session_state, sessionState(page.status)))
        if (historyChanged) line(getString(R.string.phone_history_changed))
        if (newerOmitted) line(getString(R.string.phone_newer_omitted))
        if (!page.complete) line(getString(R.string.phone_incomplete))
        if (page.source.archive == "unavailable") line(getString(R.string.phone_archive_unavailable))
        if (page.source.replay == "unverified") line(getString(R.string.phone_replay_unverified))
        if (page.source.redaction == "present") line(getString(R.string.phone_redacted))
        if (page.source.missing) line(getString(R.string.phone_missing))
        if (page.source.truncated && page.entries.none { it.truncated }) line(getString(R.string.phone_truncated))
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
            val source = if (showingOffline) selectedOffline?.source else selectedSource ?: draftSource
            if (source != null && draftSource != null && draftsReady && draftPeriod?.isActive(SystemClock.elapsedRealtime()) == true) {
                conversation.addView(Button(this).apply {
                    text = getString(R.string.phone_draft_add_context)
                    setOnClickListener { addDraftContext(source, page, entry) }
                })
            }
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
        preparedPage = null
        runPage = null
        browsePage = null
        actionHistoryPage = null
        sessionPage = null
        newerOmitted = false
        handler.removeCallbacks(refreshPoll)
        conversation.removeAllViews()
        if (outcome == PhoneControlOutcome.EXPIRED) {
            clearRunsForLock()
            clearPrepared()
            period?.lock()
            period = null
            resetReconciliation()
        }
        if (outcome == PhoneControlOutcome.REVOKED || outcome == PhoneControlOutcome.UNAUTHENTICATED) {
            clearDrafts()
            clearRunsForLock()
            clearPrepared()
            period?.lock()
            period = null
            resetReconciliation()
            currentStatus = PhoneControlStatus.Disabled
        }
        render(when (outcome) {
            PhoneControlOutcome.EXPIRED -> R.string.phone_expired
            PhoneControlOutcome.REVOKED -> R.string.phone_revoked
            PhoneControlOutcome.OUT_OF_SCOPE -> R.string.phone_out_of_scope
            PhoneControlOutcome.UNSUPPORTED -> R.string.phone_unsupported
            PhoneControlOutcome.UNAUTHENTICATED, PhoneControlOutcome.MALFORMED -> R.string.phone_invalid
            PhoneControlOutcome.STALE_SUBJECT -> if (selectedSavedResult != null) R.string.phone_saved_unavailable
                else if (showingRuns) R.string.phone_run_stale else R.string.phone_stale
            PhoneControlOutcome.UNAVAILABLE -> if (selectedSavedResult != null) R.string.phone_saved_unavailable else R.string.phone_unavailable
        })
    }

    private fun lockWorkspace() {
        generation.incrementAndGet()
        pendingNetworkAction = null
        pendingPreparation.getAndSet(null)?.value?.discard()
        pendingPeriod.getAndSet(null)?.value?.lock()
        pendingOfflinePreparation.getAndSet(null)?.value?.discard()
        pendingOfflinePeriod.getAndSet(null)?.value?.lock()
        preparedUnlock?.discard()
        preparedUnlock = null
        preparedOfflineUnlock?.discard()
        preparedOfflineUnlock = null
        cancellation?.cancel()
        cancellation = null
        period?.lock()
        period = null
        offlinePeriod?.lock()
        offlinePeriod = null
        clearDrafts()
        clearRunSession()
        showingOffline = false
        selectedOffline = null
        visibleOfflinePage = null
        selectedSource = null
        openedPages = emptyList()
        showingActionHistory = false
        actionHistoryPage = null
        clearRunsForLock()
        clearPrepared()
        resetReconciliation()
        sessionPage = null
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
            period?.lock()
            period = null
            resetReconciliation()
            handler.removeCallbacks(refreshPoll)
            handler.removeCallbacks(reconcilePoll)
            if (!showingOffline) {
                clearRunsForLock()
                clearPrepared()
                sessionPage = null
                openedPages = emptyList()
                conversation.removeAllViews()
                render(R.string.phone_expired)
            } else render()
            return
        }
        handler.postDelayed(expiryCheck, remaining)
    }

    private fun scheduleOfflineExpiry() {
        handler.removeCallbacks(offlineExpiryCheck)
        val remaining = offlinePeriod?.remainingMs(SystemClock.elapsedRealtime()) ?: return
        if (remaining == 0L) {
            lockOffline()
            return
        }
        handler.postDelayed(offlineExpiryCheck, remaining)
    }

    private fun lockOffline() {
        generation.incrementAndGet()
        offlinePeriod?.lock()
        offlinePeriod = null
        selectedOffline = null
        visibleOfflinePage = null
        if (showingOffline) {
            showingOffline = false
            conversation.removeAllViews()
        }
        handler.removeCallbacks(offlineExpiryCheck)
        busy = false
        render(R.string.phone_locked)
    }

    private fun scheduleRefresh() {
        handler.removeCallbacks(refreshPoll)
        if (foreground && period?.isActive(SystemClock.elapsedRealtime()) == true && target != null) {
            handler.postDelayed(refreshPoll, REFRESH_INTERVAL_MS)
        }
    }

    private fun schedulePinRefresh() {
        handler.removeCallbacks(pinRefresh)
        val now = SystemClock.elapsedRealtime()
        if (foreground && period?.isActive(now) == true && offlinePeriod?.isActive(now) == true) {
            handler.postDelayed(pinRefresh, PIN_REFRESH_INTERVAL_MS)
        }
    }

    /** Refresh one pin per tick without renewing either authority or its actual-open timestamp. */
    private fun refreshPinnedCopy() {
        val online = period ?: return
        val local = offlinePeriod ?: return
        if (!foreground || busy || !hasReceiverNetworkAccess(this)) {
            schedulePinRefresh()
            return
        }
        val pairing = PairingStore(applicationContext)
        val binding = pairing.binding() ?: return
        val token = generation.get()
        val index = pinRefreshIndex
        pinRefreshIndex = (pinRefreshIndex + 1) % PHONE_OFFLINE_SESSION_LIMIT
        networkExecutor.execute {
            val result = offlineResult {
                val key = local.key(SystemClock.elapsedRealtime())
                    ?: return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
                val mayContinue = {
                    foreground && active(token) && pairing.binding() == binding &&
                        online.isActive(SystemClock.elapsedRealtime()) && local.isActive(SystemClock.elapsedRealtime())
                }
                if (!mayContinue()) return@offlineResult PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
                val pins = offlineStore.list(online.grant.receiverIdentity, key).copies.filter { it.pinned }
                if (pins.isEmpty()) return@offlineResult PhoneControlResult.Value(null)
                val copy = pins[index % pins.size]
                val refresh = downloadPhoneOffline(offlineStore, copy.source, key, offlineBudget(), System.currentTimeMillis(),
                    false, mayContinue, {}) { cursor, revision, interaction ->
                    client.readSession(online, copy.source.target, SystemClock.elapsedRealtime(), before = cursor,
                        expectedRevision = revision, interaction = interaction, maySend = mayContinue)
                }
                PhoneControlResult.Value(copy.source to refresh)
            }
            runOnUiThread {
                if (foreground && period === online && offlinePeriod === local && !isDestroyed && !isFinishing) {
                    if (active(token)) {
                        val refreshed = (result as? PhoneControlResult.Value)?.value?.second
                        val failed = result is PhoneControlResult.Failure || refreshed is PhoneControlResult.Failure ||
                            refreshed is PhoneControlResult.Value &&
                            (refreshed.value.save != PhoneOfflineSave.SAVED || refreshed.value.interruption != null)
                        if (failed) render(R.string.phone_offline_incomplete)
                    }
                    schedulePinRefresh()
                }
            }
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
        internal const val RUN_ID = "dev.louiselm.capture.RUN_ID"
        internal const val RESULT_ID = "dev.louiselm.capture.RESULT_ID"
        private const val LOCAL_NETWORK_REQUEST = 1
        private const val HISTORY_WINDOW = 500
        private const val REFRESH_INTERVAL_MS = 5_000L
        private const val PIN_REFRESH_INTERVAL_MS = 10_000L
        private const val RECONCILE_INTERVAL_MS = 10_000L
        private const val RECONCILE_BATCH = 4
        private const val MAX_DRAFTS = 128
        private const val DRAFT_CONTEXT_PAGE_SIZE = 8
        private const val MEBIBYTE = 1024L * 1024
    }
}
