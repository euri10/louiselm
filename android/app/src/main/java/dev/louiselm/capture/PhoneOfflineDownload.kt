package dev.louiselm.capture

import javax.crypto.SecretKey

internal data class PhoneOfflineDownload(val save: PhoneOfflineSave, val interruption: PhoneControlOutcome?)
private class DownloadRefusal(val outcome: PhoneControlOutcome) : Exception()

/** Fetch bounded owner pages for one snapshot; only an initial interrupted download may be partial. */
internal fun downloadPhoneOffline(
    store: PhoneOfflineStore,
    source: PhoneOfflineSource,
    key: SecretKey,
    budget: Long,
    nowMs: Long,
    interaction: Boolean,
    mayContinue: () -> Boolean,
    progress: (Int) -> Unit,
    read: (Long, Long, Boolean) -> PhoneControlResult<PhoneControlReply.SessionPage>,
): PhoneControlResult<PhoneOfflineDownload> {
    val existing = store.list(source.receiver, key).copies.any { it.source.target == source.target }
    var interruption: PhoneControlOutcome? = null
    var count = 0
    val pages = sequence {
        var cursor = 0L
        var revision = 0L
        do {
            if (!mayContinue()) throw DownloadRefusal(PhoneControlOutcome.UNAVAILABLE)
            when (val response = read(cursor, revision, interaction && count == 0)) {
                is PhoneControlResult.Failure -> {
                    if (response.outcome == PhoneControlOutcome.UNAVAILABLE && count > 0 && !existing && mayContinue()) {
                        interruption = response.outcome
                        break
                    }
                    throw DownloadRefusal(response.outcome)
                }
                is PhoneControlResult.Value -> {
                    val page = response.value
                    yield(page)
                    count++
                    progress(count)
                    cursor = page.nextBefore
                    revision = page.revision
                }
            }
        } while (cursor != 0L)
    }
    return try {
        val saved = store.save(source, pages, true, if (interaction) nowMs else null, nowMs, budget, key, mayContinue)
        PhoneControlResult.Value(PhoneOfflineDownload(saved, interruption))
    } catch (failure: DownloadRefusal) {
        PhoneControlResult.Failure(failure.outcome)
    }
}
