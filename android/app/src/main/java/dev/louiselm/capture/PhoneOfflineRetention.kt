package dev.louiselm.capture

import java.math.BigInteger

internal const val PHONE_OFFLINE_DEFAULT_BUDGET = 512L * 1024 * 1024
internal const val PHONE_OFFLINE_SESSION_LIMIT = 200
private const val EXPIRY_MS = 30L * 24 * 60 * 60 * 1000

internal data class PhoneOfflineUsage(val id: String, val bytes: Long, val openedAtMs: Long, val pinned: Boolean)

internal sealed interface PhoneOfflinePlan {
    data class Accept(val remove: List<String>) : PhoneOfflinePlan
    enum class Refuse : PhoneOfflinePlan { BYTE_LIMIT, SESSION_LIMIT }
}

/** Plan removals before publishing a copy; a refusal leaves every existing copy intact. */
internal fun planPhoneOfflineRetention(
    current: List<PhoneOfflineUsage>, incoming: PhoneOfflineUsage?, budget: Long, nowMs: Long,
): PhoneOfflinePlan {
    require(budget > 0 && nowMs >= 0)
    require(current.map { it.id }.toSet().size == current.size)
    require((current + listOfNotNull(incoming)).all { it.id.isNotEmpty() && it.bytes >= 0 && it.openedAtMs >= 0 })
    val retained = current.filter { it.id != incoming?.id }.toMutableList()
    if (incoming != null) retained.add(incoming)
    val removable = retained.filter { !it.pinned && it.id != incoming?.id }
        .sortedWith(compareBy<PhoneOfflineUsage> { it.openedAtMs }.thenBy { it.id })
    var bytes = retained.fold(BigInteger.ZERO) { total, item -> total + BigInteger.valueOf(item.bytes) }
    var count = retained.size
    val removed = mutableListOf<String>()
    for (item in removable) {
        val expired = nowMs >= item.openedAtMs && nowMs - item.openedAtMs >= EXPIRY_MS
        if (expired || count > PHONE_OFFLINE_SESSION_LIMIT || bytes > BigInteger.valueOf(budget)) {
            removed.add(item.id)
            count--
            bytes -= BigInteger.valueOf(item.bytes)
        }
    }
    if (count > PHONE_OFFLINE_SESSION_LIMIT) return PhoneOfflinePlan.Refuse.SESSION_LIMIT
    if (bytes > BigInteger.valueOf(budget)) return PhoneOfflinePlan.Refuse.BYTE_LIMIT
    return PhoneOfflinePlan.Accept(removed)
}
