package dev.louiselm.capture

import android.content.Context
import android.os.SystemClock
import java.io.IOException
import java.security.GeneralSecurityException
import java.security.ProviderException
import java.security.SecureRandom
import org.json.JSONException
import org.json.JSONObject

internal typealias PhoneVoiceTransport = (PairingConfig, JSONObject, CaptureRecord?, () -> Boolean) -> PhoneVoiceJobResponse

/** All job work uses the retained destination and a still-current working period. */
internal class PhoneVoiceJobClient(
    private val binding: () -> Pair<PairingConfig, PhoneControlGrant>?,
    private val transport: PhoneVoiceTransport,
    private val clock: () -> Long,
    private val onRefusal: (PhoneControlOutcome) -> Unit = {},
) {
    constructor(context: Context, control: PhoneControlClient) : this(
        binding = {
            val config = PairingStore(context).load()
            val status = control.status()
            val grant = (status as? PhoneControlResult.Value)?.value as? PhoneControlStatus.Granted
            if (config == null || grant == null || config.receiverIdentitySha256.lowercase() != grant.value.receiverIdentity ||
                phoneControlPairingBinding(config) != grant.value.pairingBinding) null else config to grant.value
        },
        transport = PinnedHttps::phoneVoiceJob,
        clock = SystemClock::elapsedRealtime,
        onRefusal = { outcome ->
            if (outcome == PhoneControlOutcome.REVOKED || outcome == PhoneControlOutcome.UNAUTHENTICATED) {
                val store = PhoneControlStore(context)
                if (outcome == PhoneControlOutcome.REVOKED) store.grant()?.let { revokePhoneOffline(context, it.receiverIdentity) }
                store.clear()
            }
        },
    )

    fun submit(period: PhoneControlPeriod, record: PhoneVoiceRecord, maySend: () -> Boolean): PhoneControlResult<String> = guarded {
        if (record.intent != PhoneVoiceIntent.REQUESTED) return@guarded unavailable()
        val audio = record.audio ?: return@guarded unavailable()
        require(audio.id == record.id)
        val payload = phoneVoiceAdmissionPayload(audio, record.source, record.draftId, record.draftRevision)
        perform(period, record, 83, payload, audio, maySend)
    }

    fun cancel(period: PhoneControlPeriod, record: PhoneVoiceRecord, maySend: () -> Boolean): PhoneControlResult<String> = guarded {
        if (record.intent != PhoneVoiceIntent.CANCEL_PENDING) return@guarded unavailable()
        perform(period, record, 84, phoneVoiceMutationPayload(record.id), null, maySend)
    }

    fun retry(period: PhoneControlPeriod, record: PhoneVoiceRecord, maySend: () -> Boolean): PhoneControlResult<String> = guarded {
        if (record.intent != PhoneVoiceIntent.RETRY_PENDING) return@guarded unavailable()
        val marker = record.retryOperation ?: return@guarded unavailable()
        perform(period, record, 85, phoneVoiceRetryPayload(record.id, marker), null, maySend)
    }

    private fun perform(period: PhoneControlPeriod, record: PhoneVoiceRecord, action: Int, payload: ByteArray,
        audio: CaptureRecord?, maySend: () -> Boolean): PhoneControlResult<String> {
        val started = clock()
        if (!period.isActive(started)) return PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        if (!maySend()) return unavailable()
        val current = binding() ?: return PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        if (current.second != period.grant || record.source.receiver != period.grant.receiverIdentity) {
            return PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        }
        var operation: String
        do { operation = phoneControlHex(ByteArray(16).also { SecureRandom().nextBytes(it) }) } while (operation == "00".repeat(16))
        val target = record.source.target
        val request = PhoneControlRequest(period.grant.grantId, period.grant.revision, period.periodId,
            period.grant.receiverIdentity, target.controllerId, target.subjectId, payload, operation, 0, false, action)
        val signature = period.sign(request, started) ?: return PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED)
        val allowed = { maySend() && period.isActive(clock()) }
        if (!allowed()) return unavailable()
        val reply = transport(current.first, JSONObject().put("v", 1).put("type", "request").put("request", request.json(signature)), audio, allowed)
        if (!allowed()) return unavailable()
        if (binding()?.second != period.grant) return PhoneControlResult.Failure(PhoneControlOutcome.REVOKED)
        if (reply is PhoneVoiceJobResponse.Failure) onRefusal(reply.outcome)
        return when (reply) {
            is PhoneVoiceJobResponse.Failure -> PhoneControlResult.Failure(reply.outcome)
            is PhoneVoiceJobResponse.Accepted -> if (reply.operation != operation || reply.id != record.id ||
                (action == 84 && reply.state != "cancelled")) PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED)
                else PhoneControlResult.Value(reply.state)
        }
    }

    private fun unavailable() = PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE)

    private inline fun <T> guarded(block: () -> PhoneControlResult<T>): PhoneControlResult<T> = try {
        block()
    } catch (_: GeneralSecurityException) {
        unavailable()
    } catch (_: IOException) {
        unavailable()
    } catch (_: SecurityException) {
        unavailable()
    } catch (_: ProviderException) {
        unavailable()
    } catch (_: JSONException) {
        PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED)
    } catch (_: IllegalArgumentException) {
        PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED)
    }
}
