package dev.louiselm.capture

import android.content.Context
import android.widget.TextView

/** Display the protected snapshot only in the unlocked workspace; it is never saved offline. */
internal fun phonePreparedDetailViews(context: Context, summary: PhonePreparedSummary, draft: PhonePreparedDraft): List<TextView> {
    fun text(value: String) = TextView(context).apply { this.text = value; textSize = 16f }
    fun yes(value: Boolean) = context.getString(if (value) R.string.phone_yes else R.string.phone_no)
    val execution = context.getString(if (draft.execution.kind == "duration") R.string.phone_prepared_duration
        else R.string.phone_prepared_stop_by, draft.execution.value)
    val views = mutableListOf(
        text(context.getString(R.string.phone_prepared_inactive)),
        text(context.getString(R.string.phone_prepared_identity, summary.runId, summary.revision, summary.registrationId)),
        text(context.getString(R.string.phone_prepared_limits, draft.agent, execution, draft.maxSessions,
            draft.verificationPlanDigest)),
    )
    for (bead in draft.beads) views.add(text(context.getString(R.string.phone_prepared_bead, bead.id, bead.prompt)))
    views.add(text(context.getString(R.string.phone_prepared_bead_scope, draft.beadScope.projectDigest,
        draft.beadScope.issueIds.joinToString(", "), draft.beadScope.maxMutations)))
    val provider = draft.provider
    views.add(text(context.getString(R.string.phone_prepared_provider, provider.name, provider.upstream,
        provider.addresses.joinToString(", "), provider.models.joinToString(", "), provider.maxEffort,
        provider.maxRequests, provider.disclosureProfile)))
    val scope = draft.skillRequests
    views.add(text(context.getString(R.string.phone_prepared_skill_scope, yes(scope.enabled), scope.agents.joinToString(", "),
        yes(scope.allowRun), yes(scope.allowSupply))))
    views.add(text(context.getString(R.string.phone_prepared_skills, draft.skills.mode, draft.skills.viewDigest,
        draft.skills.generationDigest ?: context.getString(R.string.phone_not_applicable), draft.network)))
    return views
}
