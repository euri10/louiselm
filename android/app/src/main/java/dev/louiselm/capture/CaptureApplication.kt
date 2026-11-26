package dev.louiselm.capture

import android.app.Application

/** Owns recorder identities, which disappear with the app process. */
class CaptureApplication : Application() {
    internal val voiceRecordingLocks = mutableSetOf<String>()
    internal val voiceMetadataLock = Any()
}
