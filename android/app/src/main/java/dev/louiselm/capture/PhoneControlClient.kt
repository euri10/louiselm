package dev.louiselm.capture

import android.content.Context
import android.os.SystemClock
import java.io.File
import java.io.IOException
import java.security.GeneralSecurityException
import java.security.KeyPair
import java.security.MessageDigest
import java.security.PrivateKey
import java.security.ProviderException
import java.security.SecureRandom
import java.security.Signature
import org.json.JSONException
import org.json.JSONObject

internal typealias PhoneControlTransport = (PairingConfig, String, JSONObject, () -> Boolean) -> PhoneControlReply
private class LocalControlRevoked : Exception()

internal data class PhoneControlTarget(val controllerId: String, val subjectId: String) {
    init {
        phoneControlHex(controllerId, 16)
        phoneControlHex(subjectId, 16)
    }
}

internal data class PhoneRunSessionTarget(
    val runId: String, val runRevision: Long, val ownerId: String, val session: PhoneControlTarget,
) {
    init {
        require(session.controllerId != "00".repeat(16) && session.subjectId != "00".repeat(16))
        phoneControlRunSessionPayload(runId, ownerId, runRevision, 0, 1)
    }
}

internal data class PhoneSendAttempt(val pending: PendingPhoneAction, val outcome: PhoneControlOutcome?)

internal sealed class PhoneControlResult<out T> {
    data class Value<T>(val value: T) : PhoneControlResult<T>()
    data class Failure(val outcome: PhoneControlOutcome) : PhoneControlResult<Nothing>()
}

internal sealed class PhoneControlStatus {
    data object Disabled : PhoneControlStatus()
    data class Pending(val value: PhoneControlPending) : PhoneControlStatus()
    data class Granted(val value: PhoneControlGrant) : PhoneControlStatus()
}

internal class PreparedPhoneControlUnlock(
    val grant: PhoneControlGrant,
    val proof: PhoneControlProof,
    workingKey: KeyPair,
    val signing: Signature,
) {
    private var workingPrivate: PrivateKey? = workingKey.private

    @Synchronized
    fun available(): Boolean = workingPrivate != null

    @Synchronized
    fun takeWorkingKey(): PrivateKey? = workingPrivate.also { workingPrivate = null }

    @Synchronized
    fun discard() { workingPrivate = null }
}

/** Synchronous operations for a screen-owned background executor; no credentials or keys are logged. */
internal class PhoneControlClient(
    context: Context,
    private val keys: PhoneControlKeyAccess = PhoneControlKeys(),
    private val transport: PhoneControlTransport = { config, endpoint, body, maySend ->
        PinnedHttps.phoneControl(config, endpoint, body, maySend)
    },
) {
    private val app = context.applicationContext
    private val pairingStore = PairingStore(context)
    private val store = PhoneControlStore(context)
    private val offlineStore = PhoneOfflineStore(File(app.filesDir, "phone-offline-v1"))

    fun status(): PhoneControlResult<PhoneControlStatus> = guarded {
        val config = pairingStore.load() ?: run {
            store.clear()
            return@guarded PhoneControlResult.Value(PhoneControlStatus.Disabled)
        }
        val grant = store.grant()
        if (grant != null) {
            if (!matches(config, grant.receiverIdentity, grant.pairingBinding)) {
                if (config.receiverIdentitySha256.lowercase() != grant.receiverIdentity) revokePhoneOffline(app, grant.receiverIdentity)
                store.clear()
                return@guarded PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
            }
            return@guarded PhoneControlResult.Value(PhoneControlStatus.Granted(grant))
        }
        val pending = store.pending()
        if (pending != null) {
            if (!matches(config, pending.receiverIdentity, pending.pairingBinding)) {
                if (config.receiverIdentitySha256.lowercase() != pending.receiverIdentity) revokePhoneOffline(app, pending.receiverIdentity)
                store.clear()
                return@guarded PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
            }
            return@guarded PhoneControlResult.Value(PhoneControlStatus.Pending(pending))
        }
        PhoneControlResult.Value(PhoneControlStatus.Disabled)
    }

    fun enroll(maySend: () -> Boolean): PhoneControlResult<PhoneControlPending> = guarded {
        if (!maySend()) return@guarded unavailable()
        val config = pairingStore.load() ?: return@guarded unavailable()
        if (store.grant() != null) return@guarded PhoneControlResult.Failure(PhoneControlOutcome.OUT_OF_SCOPE)
        val offer = request(config, "enrollment/offer", "enrollment_offer", maySend)
        if (offer !is PhoneControlReply.Offer) return@guarded refusal(offer)
        if (offer.receiverIdentity != config.receiverIdentitySha256.lowercase()) return@guarded malformed()
        val publicKey = keys.enrollmentPublicKey()
        val pending = PhoneControlPending(offer.enrollmentId, offer.matchingCode, offer.receiverIdentity,
            phoneControlPairingBinding(config), hash(publicKey))
        if (!maySend()) return@guarded unavailable()
        store.savePending(pending)
        val body = message("enrollment_submit").put("enrollment_id", pending.enrollmentId)
            .put("matching_code", pending.matchingCode).put("enrolled_key", phoneControlHex(publicKey))
        val reply = transport(config, "enrollment/submit", body, maySend)
        if (reply !is PhoneControlReply.Pending) return@guarded refusal(reply)
        if (reply.enrollmentId != pending.enrollmentId) return@guarded malformed()
        PhoneControlResult.Value(pending)
    }

    /** Null means the computer has not approved the pending enrollment yet. */
    fun checkEnrollment(maySend: () -> Boolean): PhoneControlResult<PhoneControlGrant?> = guarded {
        if (!maySend()) return@guarded unavailable()
        val config = pairingStore.load() ?: return@guarded unavailable()
        val pending = store.pending() ?: return@guarded unavailable()
        if (!matches(config, pending.receiverIdentity, pending.pairingBinding)) {
            if (config.receiverIdentitySha256.lowercase() != pending.receiverIdentity) revokePhoneOffline(app, pending.receiverIdentity)
            store.clear()
            return@guarded PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        }
        val body = message("enrollment_status").put("enrollment_id", pending.enrollmentId)
        when (val reply = transport(config, "enrollment/status", body, maySend)) {
            is PhoneControlReply.Pending -> {
                if (reply.enrollmentId != pending.enrollmentId) malformed() else PhoneControlResult.Value(null)
            }
            is PhoneControlReply.Granted -> {
                if (reply.enrollmentId != pending.enrollmentId || reply.receiverIdentity != pending.receiverIdentity ||
                    reply.keyHash != pending.keyHash || hash(keys.publicKey()) != pending.keyHash) return@guarded malformed()
                if (!maySend()) return@guarded unavailable()
                val grant = PhoneControlGrant(reply.grantId, reply.revision, reply.receiverIdentity, pending.pairingBinding, reply.keyHash)
                offlineStore.allowReceiver(grant.receiverIdentity)
                store.saveGrant(grant)
                PhoneControlResult.Value(grant)
            }
            else -> refusal(reply)
        }
    }

    fun prepareUnlock(maySend: () -> Boolean): PhoneControlResult<PreparedPhoneControlUnlock> = guarded {
        if (!maySend()) return@guarded unavailable()
        val config = pairingStore.load() ?: return@guarded unavailable()
        val grant = currentGrant(config) ?: return@guarded unavailable()
        val body = message("challenge").put("grant_id", grant.grantId).put("grant_revision", grant.revision)
        val reply = transport(config, "challenge", body, maySend)
        if (reply !is PhoneControlReply.Challenge) return@guarded refusal(reply)
        if (reply.grantId != grant.grantId || reply.revision != grant.revision || reply.receiverIdentity != grant.receiverIdentity) {
            return@guarded malformed()
        }
        if (!maySend()) return@guarded unavailable()
        val workingKey = phoneControlWorkingKey()
        val proof = PhoneControlProof(grant.grantId, grant.revision, grant.keyHash, grant.receiverIdentity,
            reply.challengeId, reply.challenge, phoneControlHex(phoneControlPublicKey(workingKey.public)))
        PhoneControlResult.Value(PreparedPhoneControlUnlock(grant, proof, workingKey, keys.signingOperation()))
    }

    fun finishUnlock(
        prepared: PreparedPhoneControlUnlock,
        signature: ByteArray,
        startedAtMs: Long,
        maySend: () -> Boolean,
    ): PhoneControlResult<PhoneControlPeriod> = guarded {
        if (!maySend() || !prepared.available()) return@guarded unavailable()
        val config = pairingStore.load() ?: return@guarded unavailable()
        if (currentGrant(config) != prepared.grant) return@guarded unavailable()
        val reply = transport(config, "unlock", message("unlock").put("proof", prepared.proof.json(signature)), maySend)
        if (reply !is PhoneControlReply.Unlocked) return@guarded refusal(reply)
        if (reply.grantId != prepared.grant.grantId || reply.revision != prepared.grant.revision) return@guarded malformed()
        if (!maySend()) return@guarded unavailable()
        val workingKey = prepared.takeWorkingKey() ?: return@guarded unavailable()
        PhoneControlResult.Value(PhoneControlPeriod(prepared.grant, reply.periodId, workingKey, startedAtMs, reply.sendSupported))
    }

    /** Read only the exact current incarnation; callbacks cannot extend a locked or replaced period. */
    fun readSession(
        period: PhoneControlPeriod,
        target: PhoneControlTarget,
        nowMs: Long,
        before: Long = 0,
        limit: Int = 50,
        expectedRevision: Long = 0,
        interaction: Boolean = true,
        maySend: () -> Boolean,
    ): PhoneControlResult<PhoneControlReply.SessionPage> = guarded {
        readSessionRequest(period, target, nowMs, before, limit, expectedRevision, interaction, null, maySend)
    }

    /** Read only the exact live Session and the Run owner that exposed it. */
    fun readRunSession(
        period: PhoneControlPeriod,
        target: PhoneRunSessionTarget,
        nowMs: Long,
        before: Long = 0,
        limit: Int = 50,
        expectedRevision: Long = 0,
        interaction: Boolean = true,
        maySend: () -> Boolean,
    ): PhoneControlResult<PhoneControlReply.SessionPage> = guarded {
        readSessionRequest(period, target.session, nowMs, before, limit, expectedRevision, interaction, target, maySend)
    }

    private fun readSessionRequest(
        period: PhoneControlPeriod,
        target: PhoneControlTarget,
        nowMs: Long,
        before: Long,
        limit: Int,
        expectedRevision: Long,
        interaction: Boolean,
        linked: PhoneRunSessionTarget?,
        maySend: () -> Boolean,
    ): PhoneControlResult<PhoneControlReply.SessionPage> {
        if (!period.isActive(nowMs)) return PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        if (!maySend()) return unavailable()
        val config = pairingStore.load() ?: return unavailable()
        if (currentGrant(config) != period.grant) return PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        val operationId = phoneControlHex(ByteArray(16).also { SecureRandom().nextBytes(it) })
        val payload = if (linked == null) phoneControlReadPayload(before, limit)
            else phoneControlRunSessionPayload(linked.runId, linked.ownerId, linked.runRevision, before, limit)
        val request = PhoneControlRequest(period.grant.grantId, period.grant.revision, period.periodId,
            period.grant.receiverIdentity, target.controllerId, target.subjectId, payload,
            operationId, expectedRevision, interaction, action = if (linked == null) 1 else 57)
        val signature = period.sign(request, nowMs) ?: return PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        val reply = transport(config, "request", message("request").put("request", request.json(signature)), maySend)
        if (!maySend() || !period.isActive(SystemClock.elapsedRealtime())) return unavailable()
        val currentConfig = pairingStore.load() ?: return PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        if (currentGrant(currentConfig) != period.grant) return PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        val source = PhoneOfflineSource(period.grant.receiverIdentity, target, "", "", "")
        if (reply !is PhoneControlReply.SessionPage) {
            if (linked == null && reply is PhoneControlReply.Failure && reply.outcome == PhoneControlOutcome.OUT_OF_SCOPE) {
                denyPhoneOffline(app, source)
            }
            return refusal(reply)
        }
        if (reply.controllerId != target.controllerId || reply.subjectId != target.subjectId || reply.operationId != operationId) {
            return malformed()
        }
        val first = reply.entries.firstOrNull()?.seq
        if ((expectedRevision != 0L && reply.revision != expectedRevision) || reply.entries.size > limit ||
            (before != 0L && reply.entries.any { it.seq >= before }) ||
            (reply.nextBefore != 0L && ((before != 0L && reply.nextBefore >= before) ||
                (first != null && reply.nextBefore > first) || (!reply.source.missing && reply.nextBefore != first)))) {
            return malformed()
        }
        return PhoneControlResult.Value(reply)
    }

    /** List attached controller identities; no Session details are returned by this request. */
    fun listControllers(period: PhoneControlPeriod, nowMs: Long, after: String? = null,
        limit: Int = 50, interaction: Boolean = true, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.ControllerList> = guarded {
        val result = discover(period, nowMs, "00".repeat(16), after, limit, 48, interaction, maySend)
        if (result !is PhoneControlResult.Value) return@guarded result as PhoneControlResult.Failure
        val (reply, operationId) = result.value
        if (reply !is PhoneControlReply.ControllerList) return@guarded refusal(reply)
        if (reply.operationId != operationId || reply.controllers.size > limit ||
            reply.controllers.any { after != null && it <= after } ||
            (reply.nextAfter.isNotEmpty() && reply.controllers.size != limit)) return@guarded malformed()
        PhoneControlResult.Value(reply)
    }

    /** List only authorized live Sessions of one exact attached controller. */
    fun listSessions(period: PhoneControlPeriod, nowMs: Long, controllerId: String, after: String? = null,
        limit: Int = 50, interaction: Boolean = false, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.SessionList> = guarded {
        val result = discover(period, nowMs, controllerId, after, limit, 49, interaction, maySend)
        if (result !is PhoneControlResult.Value) return@guarded result as PhoneControlResult.Failure
        val (reply, operationId) = result.value
        if (reply !is PhoneControlReply.SessionList) return@guarded refusal(reply)
        if (reply.controllerId != controllerId || reply.operationId != operationId || reply.sessions.size > limit ||
            reply.sessions.any { after != null && it.subjectId <= after } ||
            (reply.nextAfter.isNotEmpty() && reply.sessions.size != limit)) return@guarded malformed()
        PhoneControlResult.Value(reply)
    }

    /** Read one bounded page of explicitly published drafts under current phone authority. */
    fun listPrepared(period: PhoneControlPeriod, nowMs: Long, after: String? = null,
        interaction: Boolean = true, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.PreparedPage> = guarded {
        val payload = after?.let { phoneControlHex(it, 16) } ?: ByteArray(16)
        val result = ownerRead(period, nowMs, "00".repeat(16), payload, 0, 52, interaction, maySend)
        if (result !is PhoneControlResult.Value) return@guarded result as PhoneControlResult.Failure
        val (reply, operationId) = result.value
        if (reply !is PhoneControlReply.PreparedPage) return@guarded refusal(reply)
        val item = reply.items.singleOrNull()
        if (reply.requestId != operationId || reply.items.size > 1 ||
            (item == null && reply.nextAfter.isNotEmpty()) ||
            (item != null && (reply.nextAfter != item.registrationId || after != null && item.registrationId <= after))) {
            return@guarded malformed()
        }
        PhoneControlResult.Value(reply)
    }

    /** Read the exact registered revision and Run identity selected from a protected page. */
    fun readPrepared(period: PhoneControlPeriod, target: PhonePreparedSummary, nowMs: Long,
        interaction: Boolean = true, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.PreparedDetail> = guarded {
        val result = ownerRead(period, nowMs, target.registrationId, ByteArray(0), target.revision, 53, interaction, maySend)
        if (result !is PhoneControlResult.Value) return@guarded result as PhoneControlResult.Failure
        val (reply, operationId) = result.value
        if (reply !is PhoneControlReply.PreparedDetail) return@guarded refusal(reply)
        if (reply.requestId != operationId || reply.registrationId != target.registrationId ||
            reply.runId != target.runId || reply.revision != target.revision) return@guarded malformed()
        PhoneControlResult.Value(reply)
    }

    /** Discover one bounded page of currently authorized durable Runs. */
    fun listRuns(period: PhoneControlPeriod, nowMs: Long, after: String? = null,
        interaction: Boolean = true, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.RunPage> = guarded {
        val result = ownerRead(period, nowMs, "00".repeat(16), phoneControlRunCursor(after), 0, 54, interaction, maySend)
        if (result !is PhoneControlResult.Value) return@guarded result as PhoneControlResult.Failure
        val (reply, operationId) = result.value
        if (reply !is PhoneControlReply.RunPage) return@guarded refusal(reply)
        val item = reply.items.singleOrNull()
        if (reply.requestId != operationId || reply.items.size > 1 ||
            (item == null && reply.nextAfter.isNotEmpty()) ||
            (item != null && (reply.nextAfter != item.runId || after != null && item.runId <= after))) {
            return@guarded malformed()
        }
        PhoneControlResult.Value(reply)
    }

    /** Read one exact durable Run; revision zero requests its current revision. */
    fun readRun(period: PhoneControlPeriod, runId: String, expectedRevision: Long, nowMs: Long,
        interaction: Boolean = true, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.RunDetail> = guarded {
        val subject = phoneControlHex(phoneControlRunIdBytes(runId))
        val result = ownerRead(period, nowMs, subject, ByteArray(0), expectedRevision, 55, interaction, maySend)
        if (result !is PhoneControlResult.Value) return@guarded result as PhoneControlResult.Failure
        val (reply, operationId) = result.value
        if (reply !is PhoneControlReply.RunDetail) return@guarded refusal(reply)
        if (reply.requestId != operationId || reply.run.summary.runId != runId ||
            expectedRevision != 0L && reply.run.summary.revision != expectedRevision) return@guarded malformed()
        PhoneControlResult.Value(reply)
    }

    /** Observe a live Run owner while fencing it to one durable revision. */
    fun readRunLive(period: PhoneControlPeriod, runId: String, expectedRevision: Long, nowMs: Long,
        interaction: Boolean = false, maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.RunLive> = guarded {
        val subject = phoneControlHex(phoneControlRunIdBytes(runId))
        val result = ownerRead(period, nowMs, subject, ByteArray(0), expectedRevision, 56, interaction, maySend)
        if (result !is PhoneControlResult.Value) return@guarded result as PhoneControlResult.Failure
        val (reply, operationId) = result.value
        if (reply !is PhoneControlReply.RunLive) return@guarded refusal(reply)
        if (reply.requestId != operationId || reply.runId != runId ||
            expectedRevision != 0L && reply.durableRevision != expectedRevision) return@guarded malformed()
        PhoneControlResult.Value(reply)
    }

    /** Read one exact saved-result page without granting a decision or Run authority. */
    fun readSavedReview(period: PhoneControlPeriod, runId: String, resultId: String, revision: String?,
        cursor: PhoneSavedCursor?, nowMs: Long, interaction: Boolean = true,
        maySend: () -> Boolean): PhoneControlResult<PhoneControlReply.SavedReview> = guarded {
        val subject = phoneControlHex(phoneControlRunIdBytes(runId))
        val payload = phoneSavedReviewPayload(resultId, revision, cursor)
        val result = ownerRead(period, nowMs, subject, payload, 0, 80, interaction, maySend)
        if (result !is PhoneControlResult.Value) return@guarded result as PhoneControlResult.Failure
        val (reply, operationId) = result.value
        if (reply !is PhoneControlReply.SavedReview) return@guarded refusal(reply)
        val page = reply.review
        val previous = cursor ?: PhoneSavedCursor(0, PhoneSavedSide.BEFORE, 0)
        val next = page.next
        if (reply.requestId != operationId || page.runId != runId || page.resultId != resultId ||
            revision != null && page.revision != revision || next != null && !savedCursorAdvances(previous, next)) {
            return@guarded malformed()
        }
        PhoneControlResult.Value(reply)
    }

    private fun ownerRead(period: PhoneControlPeriod, nowMs: Long, subject: String, payload: ByteArray,
        revision: Long, action: Int, interaction: Boolean, maySend: () -> Boolean): PhoneControlResult<Pair<PhoneControlReply, String>> {
        if (!period.isActive(nowMs)) return PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        if (!maySend()) return unavailable()
        val config = pairingStore.load() ?: return unavailable()
        if (currentGrant(config) != period.grant) return PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        val operationId = phoneControlHex(ByteArray(16).also { SecureRandom().nextBytes(it) })
        val request = PhoneControlRequest(period.grant.grantId, period.grant.revision, period.periodId,
            period.grant.receiverIdentity, "00".repeat(16), subject, payload, operationId, revision, interaction, action = action)
        val signature = period.sign(request, nowMs) ?: return PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        val reply = transport(config, "request", message("request").put("request", request.json(signature)), maySend)
        if (!maySend() || !period.isActive(SystemClock.elapsedRealtime())) return unavailable()
        val currentConfig = pairingStore.load() ?: return PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        if (currentGrant(currentConfig) != period.grant) return PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        return PhoneControlResult.Value(reply to operationId)
    }

    private fun discover(period: PhoneControlPeriod, nowMs: Long, controllerId: String, after: String?, limit: Int,
        action: Int, interaction: Boolean, maySend: () -> Boolean): PhoneControlResult<Pair<PhoneControlReply, String>> {
        if (!period.isActive(nowMs)) return PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        if (!maySend()) return unavailable()
        val config = pairingStore.load() ?: return unavailable()
        if (currentGrant(config) != period.grant) return PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        val operationId = phoneControlHex(ByteArray(16).also { SecureRandom().nextBytes(it) })
        val request = PhoneControlRequest(period.grant.grantId, period.grant.revision, period.periodId,
            period.grant.receiverIdentity, controllerId, "00".repeat(16), phoneControlDiscoveryPayload(after, limit),
            operationId, 0, interaction, action = action)
        val signature = period.sign(request, nowMs) ?: return PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        val reply = transport(config, "request", message("request").put("request", request.json(signature)), maySend)
        if (!maySend() || !period.isActive(SystemClock.elapsedRealtime())) return unavailable()
        val currentConfig = pairingStore.load() ?: return PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        if (currentGrant(currentConfig) != period.grant) return PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        return PhoneControlResult.Value(reply to operationId)
    }

    /** Reserve the exact explicit Send before transport; a reply alone never proves owner admission. */
    fun sendPrompt(
        period: PhoneControlPeriod,
        target: PhoneControlTarget,
        expectedRevision: Long,
        text: String,
        pendingStore: PendingPhoneActionStore,
        nowMs: Long,
        maySend: () -> Boolean,
    ): PhoneControlResult<PhoneSendAttempt> = guarded {
        if (!period.isActive(nowMs)) return@guarded PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        if (!maySend()) return@guarded unavailable()
        val config = pairingStore.load() ?: return@guarded unavailable()
        if (currentGrant(config) != period.grant) return@guarded PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        when (period.sendSupported) {
            false -> return@guarded PhoneControlResult.Failure(PhoneControlOutcome.UNSUPPORTED)
            null -> return@guarded unavailable()
            true -> Unit
        }
        val operationId = phoneControlHex(ByteArray(16).also { SecureRandom().nextBytes(it) })
        val request = PhoneControlRequest(period.grant.grantId, period.grant.revision, period.periodId,
            period.grant.receiverIdentity, target.controllerId, target.subjectId, phoneControlSendPayload(text),
            operationId, expectedRevision, true, action = 3)
        val signature = period.sign(request, nowMs) ?: return@guarded PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        if (!maySend() || !period.isActive(SystemClock.elapsedRealtime())) return@guarded unavailable()
        if (pendingStore.reserve(request) != PendingReservation.NEW) return@guarded unavailable()
        val pending = PendingPhoneAction(request.operationId, request.receiverIdentity, request.controllerId,
            request.subjectId, request.action, request.payloadHash, request.grantId, request.grantRevision,
            request.periodId, request.expectedRevision)
        val attempt = PhoneSendAttempt(pending, PhoneControlOutcome.UNAVAILABLE)
        if (!maySend() || !period.isActive(SystemClock.elapsedRealtime())) return@guarded PhoneControlResult.Value(attempt)
        val currentConfig = pairingStore.load() ?: return@guarded PhoneControlResult.Value(attempt)
        if (currentGrant(currentConfig) != period.grant) return@guarded PhoneControlResult.Value(attempt.copy(outcome = PhoneControlOutcome.REVOKED))
        val reply = transport(currentConfig, "request", message("request").put("request", request.json(signature))) {
            maySend() && period.isActive(SystemClock.elapsedRealtime())
        }
        if (!maySend() || !period.isActive(SystemClock.elapsedRealtime())) return@guarded PhoneControlResult.Value(attempt)
        val finalConfig = pairingStore.load() ?: return@guarded PhoneControlResult.Value(attempt)
        if (currentGrant(finalConfig) != period.grant) return@guarded PhoneControlResult.Value(attempt.copy(outcome = PhoneControlOutcome.REVOKED))
        val outcome = (reply as? PhoneControlReply.Failure)?.outcome
        if (reply is PhoneControlReply.Failure) refusal(reply)
        PhoneControlResult.Value(PhoneSendAttempt(pending, outcome))
    }

    /** Inspect the original operation under current read authority without renewing the period. */
    fun readActionHistory(
        period: PhoneControlPeriod,
        pending: PendingPhoneAction,
        nowMs: Long,
        maySend: () -> Boolean,
    ): PhoneControlResult<PhoneActionRecord> = guarded {
        if (!period.isActive(nowMs)) return@guarded PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        if (!maySend()) return@guarded unavailable()
        val config = pairingStore.load() ?: return@guarded unavailable()
        if (currentGrant(config) != period.grant || pending.receiverIdentity != period.grant.receiverIdentity) {
            return@guarded PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        }
        val operationId = phoneControlHex(ByteArray(16).also { SecureRandom().nextBytes(it) })
        val request = PhoneControlRequest(period.grant.grantId, period.grant.revision, period.periodId,
            period.grant.receiverIdentity, pending.controllerId, pending.subjectId,
            phoneControlHistoryRecordPayload(pending.operationId), operationId, 0, false, action = 2)
        val signature = period.sign(request, nowMs) ?: return@guarded PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        val reply = transport(config, "request", message("request").put("request", request.json(signature)), maySend)
        if (!maySend() || !period.isActive(SystemClock.elapsedRealtime())) return@guarded unavailable()
        val currentConfig = pairingStore.load() ?: return@guarded PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        if (currentGrant(currentConfig) != period.grant) return@guarded PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        if (reply !is PhoneControlReply.ActionHistory) return@guarded refusal(reply)
        val record = reply.record
        if (reply.requestId != operationId || record.operationId != pending.operationId ||
            record.receiverIdentity != pending.receiverIdentity || record.controllerId != pending.controllerId ||
            record.subjectId != pending.subjectId || record.action != pending.action || record.payloadHash != pending.payloadHash ||
            record.grantId != pending.grantId || record.grantRevision != pending.grantRevision ||
            record.expectedRevision != pending.expectedRevision) return@guarded malformed()
        PhoneControlResult.Value(record)
    }

    /** Inspect scoped retained evidence without a live Session or idle-period renewal. */
    fun readActionHistoryPage(
        period: PhoneControlPeriod,
        nowMs: Long,
        after: String? = null,
        limit: Int = 50,
        maySend: () -> Boolean,
    ): PhoneControlResult<PhoneControlReply.ActionHistoryPage> = guarded {
        if (!period.isActive(nowMs)) return@guarded PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        if (!maySend()) return@guarded unavailable()
        val config = pairingStore.load() ?: return@guarded unavailable()
        if (currentGrant(config) != period.grant) return@guarded PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        val operationId = phoneControlHex(ByteArray(16).also { SecureRandom().nextBytes(it) })
        val zeros = "00".repeat(16)
        val request = PhoneControlRequest(period.grant.grantId, period.grant.revision, period.periodId,
            period.grant.receiverIdentity, zeros, zeros, phoneControlHistoryPagePayload(after, limit), operationId, 0, false, action = 2)
        val signature = period.sign(request, nowMs) ?: return@guarded PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        val reply = transport(config, "request", message("request").put("request", request.json(signature)), maySend)
        if (!maySend() || !period.isActive(SystemClock.elapsedRealtime())) return@guarded unavailable()
        val currentConfig = pairingStore.load() ?: return@guarded PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        if (currentGrant(currentConfig) != period.grant) return@guarded PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        if (reply !is PhoneControlReply.ActionHistoryPage) return@guarded refusal(reply)
        if (reply.requestId != operationId || reply.records.size > limit ||
            reply.records.any { it.receiverIdentity != period.grant.receiverIdentity || after != null && it.operationId <= after }) {
            return@guarded malformed()
        }
        PhoneControlResult.Value(reply)
    }

    private fun currentGrant(config: PairingConfig): PhoneControlGrant? {
        val grant = store.grant() ?: return null
        if (!matches(config, grant.receiverIdentity, grant.pairingBinding)) {
            if (config.receiverIdentitySha256.lowercase() != grant.receiverIdentity) revokePhoneOffline(app, grant.receiverIdentity)
            store.clear()
            throw LocalControlRevoked()
        }
        if (hash(keys.publicKey()) != grant.keyHash) return null
        return grant
    }

    private fun matches(config: PairingConfig, receiver: String, binding: String): Boolean =
        config.receiverIdentitySha256.lowercase() == receiver && phoneControlPairingBinding(config) == binding

    private fun hash(bytes: ByteArray): String = phoneControlHex(MessageDigest.getInstance("SHA-256").digest(bytes))

    private fun request(config: PairingConfig, path: String, type: String, maySend: () -> Boolean): PhoneControlReply =
        transport(config, path, message(type), maySend)

    private fun message(type: String) = JSONObject().put("v", 1).put("type", type)

    private fun refusal(reply: PhoneControlReply): PhoneControlResult.Failure {
        val outcome = (reply as? PhoneControlReply.Failure)?.outcome ?: PhoneControlOutcome.MALFORMED
        if (outcome == PhoneControlOutcome.REVOKED) {
            val receiver = store.grant()?.receiverIdentity ?: store.pending()?.receiverIdentity
            if (receiver != null) revokePhoneOffline(app, receiver)
        }
        if (outcome == PhoneControlOutcome.REVOKED || outcome == PhoneControlOutcome.UNAUTHENTICATED) store.clear()
        return PhoneControlResult.Failure(outcome)
    }

    private fun unavailable() = PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)
    private fun malformed() = PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED)

    private inline fun <T> guarded(block: () -> PhoneControlResult<T>): PhoneControlResult<T> = try {
        block()
    } catch (_: LocalControlRevoked) {
        PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
    } catch (_: GeneralSecurityException) {
        unavailable()
    } catch (_: IOException) {
        unavailable()
    } catch (_: SecurityException) {
        unavailable()
    } catch (_: ProviderException) {
        unavailable()
    } catch (_: JSONException) {
        malformed()
    } catch (_: IllegalArgumentException) {
        malformed()
    }
}
