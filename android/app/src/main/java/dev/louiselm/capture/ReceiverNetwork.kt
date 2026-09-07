package dev.louiselm.capture

import android.Manifest
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build

internal fun hasReceiverNetworkAccess(context: Context): Boolean =
    Build.VERSION.SDK_INT < 37 ||
        context.checkSelfPermission(Manifest.permission.ACCESS_LOCAL_NETWORK) == PackageManager.PERMISSION_GRANTED

internal fun requireReceiverNetworkAccess(context: Context) {
    if (!hasReceiverNetworkAccess(context)) {
        throw SecurityException(context.getString(R.string.local_network_denied))
    }
}
