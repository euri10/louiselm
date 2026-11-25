package dev.louiselm.capture

import android.content.Context
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
