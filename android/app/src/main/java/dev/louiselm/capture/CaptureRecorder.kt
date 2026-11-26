package dev.louiselm.capture

import android.media.MediaRecorder
import java.io.File
import java.io.IOException

/** Start the app's AAC recorder; its caller owns stopping and releasing it. */
@Suppress("DEPRECATION")
internal fun startCaptureRecorder(file: File): MediaRecorder {
    val recorder = MediaRecorder()
    try {
        recorder.setAudioSource(MediaRecorder.AudioSource.MIC)
        recorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
        recorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
        recorder.setAudioSamplingRate(44_100)
        recorder.setAudioEncodingBitRate(128_000)
        recorder.setOutputFile(file.absolutePath)
        recorder.prepare()
        recorder.start()
        return recorder
    } catch (error: RuntimeException) {
        recorder.release()
        throw error
    } catch (error: IOException) {
        recorder.release()
        throw error
    }
}
