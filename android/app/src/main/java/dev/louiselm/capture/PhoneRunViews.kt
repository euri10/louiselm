package dev.louiselm.capture

import android.content.Context
import android.view.View
import android.widget.Button
import android.widget.TextView
import java.text.DateFormat
import java.util.Date

internal fun phoneRunState(context: Context, state: PhoneRunState): String = context.getString(when (state) {
    PhoneRunState.ADMITTED -> R.string.phone_run_admitted
    PhoneRunState.ACTIVE -> R.string.phone_run_active
    PhoneRunState.PARKED -> R.string.phone_run_parked
    PhoneRunState.COLD_PARKED -> R.string.phone_run_cold_parked
    PhoneRunState.RESUMING -> R.string.phone_run_resuming
    PhoneRunState.DISPOSED -> R.string.phone_run_disposed
})

/** Render only a freshly authorized durable snapshot; this is not live controller state. */
internal fun phoneRunDetailViews(context: Context, run: PhoneRunView): List<TextView> {
    fun text(value: String) = TextView(context).apply { this.text = value; textSize = 16f }
    fun time(value: Long) = DateFormat.getDateTimeInstance().format(Date(value))
    val load = when (run.loadSession) {
        PhoneRunLoad.UNKNOWN -> R.string.phone_run_load_unknown
        PhoneRunLoad.SUPPORTED -> R.string.phone_run_load_supported
        PhoneRunLoad.UNSUPPORTED -> R.string.phone_run_load_unsupported
    }
    val views = mutableListOf(
        text(context.getString(R.string.phone_run_freshness)),
        text(context.getString(R.string.phone_run_identity, run.summary.runId, run.summary.revision,
            phoneRunState(context, run.summary.state))),
        text(context.getString(R.string.phone_run_work, run.work.consumed, run.work.reserved,
            run.work.remaining, run.work.ceiling, run.pendingMutations)),
        text(context.getString(if (run.hasSession) R.string.phone_run_session_recorded else R.string.phone_run_no_session)),
        text(context.getString(load)),
    )
    if (run.parkExpiresAtMs > 0 && run.summary.state in setOf(PhoneRunState.PARKED, PhoneRunState.COLD_PARKED)) {
        views.add(text(context.getString(if (run.parkExpired) R.string.phone_run_park_expired else R.string.phone_run_park_expires,
            time(run.parkExpiresAtMs))))
    }
    if (run.resumePending) views.add(text(context.getString(R.string.phone_run_resume_deadline, time(run.resumeDeadlineMs))))
    return views
}

/** Display a separately checked live observation; Session targets retain every owner identity. */
internal fun phoneRunLiveViews(
    context: Context,
    reply: PhoneControlReply.RunLive?,
    open: (PhoneRunSessionTarget) -> Unit,
): List<View> {
    fun text(value: String) = TextView(context).apply { this.text = value; textSize = 16f }
    if (reply == null || reply.controller == PhoneRunController.UNREACHABLE) {
        return listOf(text(context.getString(R.string.phone_run_live_unreachable)))
    }
    if (reply.controller == PhoneRunController.ABSENT) {
        return listOf(text(context.getString(R.string.phone_run_live_absent)))
    }
    val live = reply.live ?: return listOf(text(context.getString(R.string.phone_run_live_unreachable)))
    val views = mutableListOf<View>(
        text(context.getString(R.string.phone_run_live_observed)),
        text(context.getString(R.string.phone_run_live_phase, live.status.name.lowercase().replace('_', ' '),
            live.phase.name.lowercase().replace('_', ' '))),
    )
    if (live.phase != PhoneRunPhase.NONE) {
        views.add(text(context.getString(R.string.phone_run_live_bead, live.beadIndex, live.beadTotal, live.beadId)))
    }
    for (session in live.sessions) {
        val target = PhoneRunSessionTarget(reply.runId, reply.durableRevision, live.ownerId,
            PhoneControlTarget(live.controllerId, session.subjectId))
        views.add(Button(context).apply {
            text = context.getString(R.string.phone_run_live_session, session.agent, session.status.replace('_', ' '))
            setOnClickListener { open(target) }
        })
    }
    if (live.sessionsTruncated) views.add(text(context.getString(R.string.phone_run_live_truncated)))
    return views
}
