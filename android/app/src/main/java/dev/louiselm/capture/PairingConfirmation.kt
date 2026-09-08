package dev.louiselm.capture

import android.app.AlertDialog
import android.content.Context

/** Ask before changing pairing; renewal preserves the receiver's capture ownership. */
internal fun showPairingConfirmation(
    context: Context,
    transition: PairingTransition,
    affectedCaptureCount: Int,
    receiverUrl: String,
    onConfirm: (PairingTransition) -> Unit,
    onCancel: () -> Unit,
) {
    val message = when (transition) {
        PairingTransition.FIRST_PAIR -> context.resources.getQuantityString(
            R.plurals.confirm_first_pair, affectedCaptureCount, affectedCaptureCount, receiverUrl,
        )
        PairingTransition.RECEIVER_MIGRATION -> context.resources.getQuantityString(
            R.plurals.confirm_receiver_migration, affectedCaptureCount, affectedCaptureCount, receiverUrl,
        )
        PairingTransition.ENDPOINT_UPDATE, PairingTransition.CREDENTIAL_RENEWAL ->
            context.getString(R.string.confirm_same_receiver, receiverUrl)
    }
    val builder = AlertDialog.Builder(context)
        .setTitle(R.string.confirm_pairing_title)
        .setMessage(message)
        .setPositiveButton(
            if (transition == PairingTransition.ENDPOINT_UPDATE) R.string.keep_pairing else R.string.confirm_pairing,
        ) { _, _ -> onConfirm(transition) }
        .setNegativeButton(android.R.string.cancel) { _, _ -> onCancel() }
        .setOnCancelListener { onCancel() }
    if (transition == PairingTransition.ENDPOINT_UPDATE) {
        builder.setNeutralButton(R.string.renew_pairing) { _, _ -> onConfirm(PairingTransition.CREDENTIAL_RENEWAL) }
    }
    builder.show()
}
