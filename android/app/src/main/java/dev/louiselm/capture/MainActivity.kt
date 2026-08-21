package dev.louiselm.capture

import android.Manifest
import android.app.Activity
import android.content.ClipData
import android.content.ActivityNotFoundException
import android.content.Intent
import android.content.pm.PackageManager
import android.media.MediaRecorder
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.os.SystemClock
import android.provider.MediaStore
import android.view.Gravity
import android.view.ViewGroup
import android.view.WindowManager
import android.widget.Button
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import androidx.core.content.FileProvider
import com.google.mlkit.vision.barcode.BarcodeScanning
import com.google.mlkit.vision.common.InputImage
import java.util.concurrent.Executors
import java.io.File

class MainActivity : Activity() {
    private lateinit var captureStore: CaptureStore
    private lateinit var pairingStore: PairingStore
    private lateinit var statusView: TextView
    private lateinit var captureButton: Button
    private lateinit var pairButton: Button
    private val networkExecutor = Executors.newSingleThreadExecutor()
    private var recorder: MediaRecorder? = null
    private var pendingRecording: PendingRecording? = null
    private var startedAtElapsedMs = 0L
    private var qrPhoto: File? = null

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        captureStore = CaptureStore(applicationContext)
        pairingStore = PairingStore(applicationContext)
        setContentView(contentView())
        refreshStatus()
    }

    override fun onResume() {
        super.onResume()
        if (recorder == null) refreshStatus()
    }

    override fun onStop() {
        if (recorder != null) stopRecording()
        super.onStop()
    }

    override fun onDestroy() {
        recorder?.release()
        recorder = null
        networkExecutor.shutdown()
        super.onDestroy()
    }

    private fun contentView(): ScrollView {
        val padding = (24 * resources.displayMetrics.density).toInt()
        val content = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            gravity = Gravity.CENTER_HORIZONTAL
            setPadding(padding, padding, padding, padding)
        }
        content.addView(TextView(this).apply {
            text = getString(R.string.app_name)
            textSize = 28f
        }, matchWidth())
        content.addView(TextView(this).apply {
            text = getString(R.string.intro)
            textSize = 18f
            setPadding(0, padding / 2, 0, padding)
        }, matchWidth())
        statusView = TextView(this).apply { textSize = 16f }
        content.addView(statusView, matchWidth())
        captureButton = Button(this).apply {
            text = getString(R.string.start_capture)
            setOnClickListener { toggleRecording() }
        }
        content.addView(captureButton, matchWidth())
        pairButton = Button(this).apply {
            text = getString(R.string.pair_receiver)
            setOnClickListener { launchQrCamera() }
        }
        content.addView(pairButton, matchWidth())
        content.addView(Button(this).apply {
            text = getString(R.string.sync_now)
            setOnClickListener {
                UploadWorker.enqueue(applicationContext, manual = true)
                statusView.text = getString(R.string.sync_queued)
            }
        }, matchWidth())
        return ScrollView(this).apply { addView(content) }
    }

    private fun matchWidth() = LinearLayout.LayoutParams(
        ViewGroup.LayoutParams.MATCH_PARENT,
        ViewGroup.LayoutParams.WRAP_CONTENT,
    )

    private fun toggleRecording() {
        if (recorder != null) {
            stopRecording()
            return
        }
        if (checkSelfPermission(Manifest.permission.RECORD_AUDIO) != PackageManager.PERMISSION_GRANTED) {
            requestPermissions(arrayOf(Manifest.permission.RECORD_AUDIO), MICROPHONE_PERMISSION_REQUEST)
            return
        }
        startRecording()
    }

    @Suppress("DEPRECATION")
    private fun startRecording() {
        val pending = captureStore.begin()
        val newRecorder = MediaRecorder()
        try {
            newRecorder.setAudioSource(MediaRecorder.AudioSource.MIC)
            newRecorder.setOutputFormat(MediaRecorder.OutputFormat.MPEG_4)
            newRecorder.setAudioEncoder(MediaRecorder.AudioEncoder.AAC)
            newRecorder.setAudioSamplingRate(44_100)
            newRecorder.setAudioEncodingBitRate(128_000)
            newRecorder.setOutputFile(pending.file.absolutePath)
            newRecorder.prepare()
            newRecorder.start()
        } catch (error: RuntimeException) {
            newRecorder.release()
            captureStore.abort(pending)
            statusView.text = getString(R.string.recording_failed, error.message ?: "recorder unavailable")
            return
        } catch (error: java.io.IOException) {
            newRecorder.release()
            captureStore.abort(pending)
            statusView.text = getString(R.string.recording_failed, error.message ?: "audio file unavailable")
            return
        }
        recorder = newRecorder
        pendingRecording = pending
        startedAtElapsedMs = SystemClock.elapsedRealtime()
        captureButton.text = getString(R.string.stop_capture)
        pairButton.isEnabled = false
        window.addFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
        statusView.text = getString(R.string.recording_status)
    }

    private fun stopRecording() {
        val active = recorder ?: return
        val pending = pendingRecording ?: return
        recorder = null
        pendingRecording = null
        val durationMs = (SystemClock.elapsedRealtime() - startedAtElapsedMs).coerceAtLeast(1)
        try {
            active.stop()
        } catch (_: RuntimeException) {
            active.release()
            resetRecordingUi()
            statusView.text = getString(R.string.recording_retained, pending.file.absolutePath)
            return
        }
        active.release()
        resetRecordingUi()
        captureButton.isEnabled = false
        statusView.text = getString(R.string.saving_status)
        networkExecutor.execute {
            val result = runCatching { captureStore.complete(pending, durationMs) }
            runOnUiThread {
                if (isDestroyed) return@runOnUiThread
                captureButton.isEnabled = true
                result.onSuccess { record ->
                    UploadWorker.enqueue(applicationContext)
                    statusView.text = getString(R.string.saved_status, record.id)
                }.onFailure { error ->
                    statusView.text = getString(R.string.recording_failed, error.message ?: "capture storage failed")
                }
            }
        }
    }

    private fun resetRecordingUi() {
        captureButton.text = getString(R.string.start_capture)
        pairButton.isEnabled = true
        window.clearFlags(WindowManager.LayoutParams.FLAG_KEEP_SCREEN_ON)
    }

    private fun launchQrCamera() {
        try {
            val directory = File(cacheDir, "pairing-qr")
            check(directory.mkdirs() || directory.isDirectory) { "pairing image directory could not be created" }
            val photo = File(directory, "pairing.jpg")
            val uri = FileProvider.getUriForFile(this, "$packageName.files", photo)
            qrPhoto = photo
            @Suppress("DEPRECATION")
            startActivityForResult(
                Intent(MediaStore.ACTION_IMAGE_CAPTURE).apply {
                    putExtra(MediaStore.EXTRA_OUTPUT, uri)
                    clipData = ClipData.newRawUri("pairing QR", uri)
                    addFlags(Intent.FLAG_GRANT_WRITE_URI_PERMISSION or Intent.FLAG_GRANT_READ_URI_PERMISSION)
                },
                QR_CAMERA_REQUEST,
            )
        } catch (_: ActivityNotFoundException) {
            statusView.text = getString(R.string.camera_missing)
        } catch (error: RuntimeException) {
            statusView.text = getString(R.string.pairing_failed, error.message ?: "camera could not start")
        }
    }

    @Deprecated("Framework callback retained to avoid another UI dependency")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != QR_CAMERA_REQUEST || resultCode != RESULT_OK) return
        val photo = qrPhoto
        qrPhoto = null
        if (photo == null || !photo.isFile) {
            statusView.text = getString(R.string.qr_not_found)
            return
        }
        val scanner = BarcodeScanning.getClient()
        val image = runCatching { InputImage.fromFilePath(this, Uri.fromFile(photo)) }.getOrElse {
            photo.delete()
            scanner.close()
            statusView.text = getString(R.string.qr_not_found)
            return
        }
        scanner.process(image)
            .addOnSuccessListener { barcodes ->
                photo.delete()
                scanner.close()
                if (isDestroyed) return@addOnSuccessListener
                val payload = barcodes.firstNotNullOfOrNull { it.rawValue }
                if (payload == null) {
                    statusView.text = getString(R.string.qr_not_found)
                } else {
                    pair(payload)
                }
            }
            .addOnFailureListener {
                photo.delete()
                scanner.close()
                if (isDestroyed) return@addOnFailureListener
                statusView.text = getString(R.string.qr_not_found)
            }
    }

    private fun pair(payload: String) {
        pairButton.isEnabled = false
        statusView.text = getString(R.string.pairing_in_progress)
        networkExecutor.execute {
            val result = runCatching {
                val offer = PairingOffer.parse(payload)
                val deviceName = "${Build.MANUFACTURER} ${Build.MODEL}".trim().take(80)
                PinnedHttps.pair(offer, deviceName).also(pairingStore::save)
            }
            runOnUiThread {
                if (isDestroyed) return@runOnUiThread
                pairButton.isEnabled = true
                result.onSuccess {
                    UploadWorker.enqueue(applicationContext)
                    statusView.text = getString(R.string.pairing_succeeded)
                }.onFailure { error ->
                    statusView.text = getString(R.string.pairing_failed, error.message ?: "receiver unavailable")
                }
            }
        }
    }

    private fun refreshStatus() {
        networkExecutor.execute {
            val status = runCatching {
                captureStore.recover()
                val pairing = pairingStore.load()
                val pairingText = if (pairing == null) {
                    getString(R.string.unpaired_status)
                } else {
                    getString(R.string.paired_status, pairing.receiverUrl)
                }
                val counts = captureStore.queueCounts()
                getString(R.string.status_format, pairingText, counts.waiting, counts.attention)
            }
            runOnUiThread {
                if (isDestroyed || recorder != null) return@runOnUiThread
                statusView.text = status.getOrElse { getString(R.string.queue_failed, it.message ?: "unknown error") }
            }
        }
    }

    override fun onRequestPermissionsResult(requestCode: Int, permissions: Array<out String>, results: IntArray) {
        super.onRequestPermissionsResult(requestCode, permissions, results)
        if (requestCode != MICROPHONE_PERMISSION_REQUEST) return
        if (results.firstOrNull() == PackageManager.PERMISSION_GRANTED) {
            startRecording()
        } else {
            statusView.text = getString(R.string.microphone_denied)
        }
    }

    companion object {
        private const val MICROPHONE_PERMISSION_REQUEST = 1
        private const val QR_CAMERA_REQUEST = 2
    }
}
