package dev.louiselm.capture

import android.content.Context
import android.view.View
import android.widget.Button
import android.widget.TextView

internal fun phoneSavedReviewViews(context: Context, review: PhoneSavedReviewPayload, next: (PhoneSavedCursor) -> Unit): List<View> {
    fun text(value: String) = TextView(context).apply { text = value; setTextIsSelectable(true) }
    val page = when (review) {
        is PhoneSavedReview -> review
        is PhoneSavedStatus -> return listOf(
            text(context.getString(R.string.phone_saved_title, review.resultId)),
            text(context.getString(R.string.phone_saved_run, review.runId)),
            text(context.getString(when (review.state) {
                PhoneSavedState.ACCEPTED -> R.string.phone_saved_status_accepted
                PhoneSavedState.DISCARDED -> R.string.phone_saved_status_discarded
                PhoneSavedState.APPLICATION_UNKNOWN -> R.string.phone_saved_status_unknown
                PhoneSavedState.APPLICATION_COMPLETED -> R.string.phone_saved_status_completed
            })),
        )
    }
    val views = mutableListOf<View>(
        text(context.getString(R.string.phone_saved_title, page.resultId)),
        text(context.getString(R.string.phone_saved_run, page.runId)),
        text(context.getString(R.string.phone_saved_destination, page.destination, page.baseCommit)),
        text(context.getString(R.string.phone_saved_agents, page.producer, page.agents.joinToString(", "))),
        text(context.getString(R.string.phone_saved_commands, page.commandsPassed)),
        text(context.getString(when (page.verification) {
            PhoneSavedVerification.MATCHING -> R.string.phone_saved_verification_matching
            PhoneSavedVerification.CHANGED -> R.string.phone_saved_verification_changed
            PhoneSavedVerification.QUARANTINED -> R.string.phone_saved_verification_quarantined
            PhoneSavedVerification.UNAVAILABLE -> R.string.phone_saved_verification_unavailable
        })),
        text(context.getString(when (page.provenance) {
            PhoneSavedProvenance.UNTAINTED -> R.string.phone_saved_provenance_untainted
            PhoneSavedProvenance.SESSION_OUTPUT_TAINTED -> R.string.phone_saved_provenance_tainted
            PhoneSavedProvenance.UNKNOWN -> R.string.phone_saved_provenance_unknown
        })),
        text(context.getString(R.string.phone_saved_historical)),
        text(context.getString(R.string.phone_saved_evidence, page.verificationDigest, page.planDigest,
            page.verifierInputsDigest, page.resultDigest)),
    )
    val file = page.file
    if (file == null) views.add(text(context.getString(R.string.phone_saved_empty)))
    else {
        views.add(text(context.getString(R.string.phone_saved_file, file.path, file.change.name.lowercase(),
            file.side.name.lowercase(), file.offset)))
        if (file.executable) views.add(text(context.getString(R.string.phone_saved_executable)))
        views.add(text(if (file.binary) context.getString(R.string.phone_saved_binary) else file.text))
    }
    page.next?.let { cursor ->
        views.add(Button(context).apply {
            text = context.getString(R.string.phone_saved_next)
            setOnClickListener { next(cursor) }
        })
    }
    return views
}
