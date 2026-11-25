package dev.louiselm.capture

import android.content.Context
import android.view.View
import android.widget.Button
import android.widget.TextView

internal fun phoneSavedReviewViews(context: Context, review: PhoneSavedReview, next: (PhoneSavedCursor) -> Unit): List<View> {
    fun text(value: String) = TextView(context).apply { text = value; setTextIsSelectable(true) }
    val views = mutableListOf<View>(
        text(context.getString(R.string.phone_saved_title, review.resultId)),
        text(context.getString(R.string.phone_saved_run, review.runId)),
        text(context.getString(R.string.phone_saved_destination, review.destination, review.baseCommit)),
        text(context.getString(R.string.phone_saved_agents, review.producer, review.agents.joinToString(", "))),
        text(context.getString(R.string.phone_saved_commands, review.commandsPassed)),
        text(context.getString(when (review.verification) {
            PhoneSavedVerification.MATCHING -> R.string.phone_saved_verification_matching
            PhoneSavedVerification.CHANGED -> R.string.phone_saved_verification_changed
            PhoneSavedVerification.QUARANTINED -> R.string.phone_saved_verification_quarantined
            PhoneSavedVerification.UNAVAILABLE -> R.string.phone_saved_verification_unavailable
        })),
        text(context.getString(when (review.provenance) {
            PhoneSavedProvenance.UNTAINTED -> R.string.phone_saved_provenance_untainted
            PhoneSavedProvenance.SESSION_OUTPUT_TAINTED -> R.string.phone_saved_provenance_tainted
            PhoneSavedProvenance.UNKNOWN -> R.string.phone_saved_provenance_unknown
        })),
        text(context.getString(R.string.phone_saved_historical)),
        text(context.getString(R.string.phone_saved_evidence, review.verificationDigest, review.planDigest,
            review.verifierInputsDigest, review.resultDigest)),
    )
    val file = review.file
    if (file == null) views.add(text(context.getString(R.string.phone_saved_empty)))
    else {
        views.add(text(context.getString(R.string.phone_saved_file, file.path, file.change.name.lowercase(),
            file.side.name.lowercase(), file.offset)))
        if (file.executable) views.add(text(context.getString(R.string.phone_saved_executable)))
        views.add(text(if (file.binary) context.getString(R.string.phone_saved_binary) else file.text))
    }
    review.next?.let { cursor ->
        views.add(Button(context).apply {
            text = context.getString(R.string.phone_saved_next)
            setOnClickListener { next(cursor) }
        })
    }
    return views
}
