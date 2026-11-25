package dev.louiselm.capture

internal data class PhoneSessionRefresh(val page: PhoneControlReply.SessionPage, val missingOlder: Boolean)

internal fun PhoneHistorySource.merge(other: PhoneHistorySource): PhoneHistorySource? {
    if (replay != other.replay || archive != other.archive) return null
    return copy(redaction = if (redaction == "present" || other.redaction == "present") "present" else "unknown",
        missing = missing || other.missing, truncated = truncated || other.truncated)
}

/** Rebuild the visible range from one revision; the caller owns the bounded network reads. */
internal fun reconcilePhoneSessionPages(
    newest: PhoneControlReply.SessionPage,
    previous: PhoneControlReply.SessionPage?,
    window: Int,
    readOlder: (Long, Long, Int) -> PhoneControlResult<PhoneControlReply.SessionPage>,
    readFresh: () -> PhoneControlResult<PhoneControlReply.SessionPage>,
): PhoneControlResult<PhoneSessionRefresh> {
    val oldest = previous?.entries?.firstOrNull()?.seq ?: return PhoneControlResult.Value(PhoneSessionRefresh(newest, false))
    if (previous.revision == newest.revision) return PhoneControlResult.Value(PhoneSessionRefresh(newest, false))
    if (newest.entries.isEmpty()) return PhoneControlResult.Value(PhoneSessionRefresh(newest, true))
    var entries = newest.entries
    var cursor = newest.nextBefore
    var complete = newest.complete
    var source = newest.source
    while (entries.first().seq > oldest && cursor != 0L && entries.size < window) {
        when (val result = readOlder(cursor, newest.revision, minOf(50, window - entries.size))) {
            is PhoneControlResult.Failure -> {
                if (result.outcome != PhoneControlOutcome.STALE_SUBJECT) return result
                return when (val fresh = readFresh()) {
                    is PhoneControlResult.Failure -> fresh
                    is PhoneControlResult.Value -> PhoneControlResult.Value(PhoneSessionRefresh(fresh.value, true))
                }
            }
            is PhoneControlResult.Value -> {
                val older = result.value
                if (older.controllerId != newest.controllerId || older.subjectId != newest.subjectId ||
                    older.revision != newest.revision || older.status != newest.status || older.entries.isEmpty() ||
                    older.entries.last().seq >= entries.first().seq || older.entries.last().seq >= cursor) {
                    return PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED)
                }
                entries = older.entries + entries
                cursor = older.nextBefore
                complete = complete && older.complete
                source = source.merge(older.source) ?: return PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED)
            }
        }
    }
    val missing = entries.first().seq > oldest
    return PhoneControlResult.Value(PhoneSessionRefresh(newest.copy(entries = entries, nextBefore = cursor, complete = complete, source = source), missing))
}
