package dev.louiselm.capture

import android.os.SystemClock

internal data class PhoneSessionRow(val target: PhoneControlTarget, val summary: PhoneControlSessionSummary)
internal data class PhoneBrowseCursor(
    val controllers: List<String>, val index: Int, val nextController: String, val afterSubject: String?,
)
internal data class PhoneBrowsePage(val rows: List<PhoneSessionRow>, val next: PhoneBrowseCursor?)

/** Reads one bounded page on the screen's background executor. */
internal class PhoneControlBrowser(private val client: PhoneControlClient) {
    fun page(period: PhoneControlPeriod, start: PhoneBrowseCursor?, interaction: Boolean,
        maySend: () -> Boolean): PhoneControlResult<PhoneBrowsePage> {
        var cursor = start
        var interact = interaction
        var requests = 0
        val rows = mutableListOf<PhoneSessionRow>()
        while (rows.size < PAGE_SIZE && requests < REQUEST_LIMIT) {
            if (cursor == null || cursor.index >= cursor.controllers.size) {
                if (cursor != null && cursor.nextController.isEmpty()) break
                val response = client.listControllers(period, SystemClock.elapsedRealtime(),
                    after = cursor?.nextController, interaction = interact, maySend = maySend)
                requests++
                interact = false
                when (response) {
                    is PhoneControlResult.Failure -> return response
                    is PhoneControlResult.Value -> cursor = PhoneBrowseCursor(
                        response.value.controllers, 0, response.value.nextAfter, null,
                    )
                }
                if (cursor.controllers.isEmpty() && cursor.nextController.isEmpty()) break
                continue
            }
            val controller = cursor.controllers[cursor.index]
            val response = client.listSessions(period, SystemClock.elapsedRealtime(), controller,
                after = cursor.afterSubject, limit = PAGE_SIZE - rows.size, interaction = interact, maySend = maySend)
            requests++
            interact = false
            when (response) {
                is PhoneControlResult.Failure -> return response
                is PhoneControlResult.Value -> {
                    rows.addAll(response.value.sessions.map { PhoneSessionRow(PhoneControlTarget(controller, it.subjectId), it) })
                    cursor = if (response.value.nextAfter.isNotEmpty()) cursor.copy(afterSubject = response.value.nextAfter)
                    else cursor.copy(index = cursor.index + 1, afterSubject = null)
                }
            }
        }
        val next = cursor?.takeUnless { it.index >= it.controllers.size && it.nextController.isEmpty() }
        return PhoneControlResult.Value(PhoneBrowsePage(rows, next))
    }

    private companion object {
        const val PAGE_SIZE = 50
        const val REQUEST_LIMIT = 10
    }
}
