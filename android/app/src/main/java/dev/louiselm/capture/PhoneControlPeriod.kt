package dev.louiselm.capture

import java.security.PrivateKey
import java.security.Signature

/** Process-only working authority. The owning screen must lock it on screen lock or destruction. */
internal class PhoneControlPeriod(
    val grant: PhoneControlGrant,
    val periodId: String,
    privateKey: PrivateKey,
    private val startedAtMs: Long,
) {
    private var key: PrivateKey? = privateKey
    private val absoluteDeadline = startedAtMs + 3_600_000L
    private var idleDeadline = startedAtMs + 300_000L

    @Synchronized
    fun isActive(nowMs: Long): Boolean {
        if (nowMs < startedAtMs || nowMs >= idleDeadline || nowMs >= absoluteDeadline) key = null
        return key != null
    }

    @Synchronized
    fun remainingMs(nowMs: Long): Long =
        if (isActive(nowMs)) minOf(idleDeadline, absoluteDeadline) - nowMs else 0

    /** Call only after an actual user interaction was accepted, never for background traffic. */
    @Synchronized
    fun confirmInteraction(nowMs: Long): Boolean {
        if (!isActive(nowMs)) return false
        idleDeadline = minOf(nowMs + 300_000L, absoluteDeadline)
        return true
    }

    /** Returns no signature after lock or expiry, or for a different grant, receiver or period. */
    @Synchronized
    fun sign(request: PhoneControlRequest, nowMs: Long): ByteArray? {
        if (!isActive(nowMs) || request.grantId != grant.grantId || request.grantRevision != grant.revision ||
            request.receiverIdentity != grant.receiverIdentity || request.periodId != periodId) return null
        val current = key ?: return null
        return Signature.getInstance("SHA256withECDSA").run {
            initSign(current)
            update(request.transcript())
            sign()
        }
    }

    @Synchronized
    fun lock() {
        key = null
    }
}
