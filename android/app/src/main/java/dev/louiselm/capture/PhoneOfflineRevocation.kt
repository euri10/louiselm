package dev.louiselm.capture

import android.content.Context
import android.content.Intent
import java.io.File

internal const val PHONE_OFFLINE_REVOKED = "dev.louiselm.capture.PHONE_OFFLINE_REVOKED"

/** Fence first, notify local screens, then erase only this computer's downloads. */
internal fun revokePhoneOffline(context: Context, receiver: String) {
    val app = context.applicationContext
    val store = PhoneOfflineStore(File(app.filesDir, "phone-offline-v1"))
    store.markRevoked(receiver)
    app.sendBroadcast(Intent(PHONE_OFFLINE_REVOKED).setPackage(app.packageName))
    store.purgeRevoked(receiver)
}

/** A scoped refusal invalidates only this Session, but also hides any displayed copy. */
internal fun denyPhoneOffline(context: Context, source: PhoneOfflineSource) {
    val app = context.applicationContext
    PhoneOfflineStore(File(app.filesDir, "phone-offline-v1")).denySource(source)
    app.sendBroadcast(Intent(PHONE_OFFLINE_REVOKED).setPackage(app.packageName))
}
