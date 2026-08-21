package dev.louiselm.capture

import java.net.URI

internal enum class UploadDisposition {
    SUCCESS,
    RETRY,
    OPERATOR_ACTION,
}

internal enum class PairingTransition {
    FIRST_PAIR,
    ENDPOINT_UPDATE,
    RECEIVER_MIGRATION,
}

internal fun pairingTransition(currentIdentity: String?, offeredIdentity: String): PairingTransition = when {
    currentIdentity == null -> PairingTransition.FIRST_PAIR
    currentIdentity == offeredIdentity -> PairingTransition.ENDPOINT_UPDATE
    else -> PairingTransition.RECEIVER_MIGRATION
}

internal fun queueBelongsToReceiver(ownerIdentity: String?, receiverIdentity: String): Boolean =
    ownerIdentity != null && ownerIdentity == receiverIdentity

internal fun uploadDisposition(status: Int): UploadDisposition = when {
    status == 200 || status == 201 -> UploadDisposition.SUCCESS
    status == 408 || status == 429 || status >= 500 -> UploadDisposition.RETRY
    else -> UploadDisposition.OPERATOR_ACTION
}

internal fun decodeSha256(value: String): ByteArray? {
    if (value.length != 64 || value.any { it.digitToIntOrNull(16) == null }) return null
    return ByteArray(32) { index -> value.substring(index * 2, index * 2 + 2).toInt(16).toByte() }
}

internal fun validReceiverUrl(value: String): Boolean = runCatching {
    val uri = URI(value)
    uri.scheme == "https" &&
        !uri.host.isNullOrBlank() &&
        uri.userInfo == null &&
        uri.fragment == null &&
        (uri.path.isNullOrEmpty() || uri.path == "/")
}.getOrDefault(false)
