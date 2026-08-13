package dev.louiselm.capture

import org.json.JSONObject
import java.io.ByteArrayOutputStream
import java.io.FileInputStream
import java.io.IOException
import java.net.URL
import java.security.MessageDigest
import java.security.SecureRandom
import java.security.cert.CertificateException
import java.security.cert.X509Certificate
import javax.net.ssl.HostnameVerifier
import javax.net.ssl.HttpsURLConnection
import javax.net.ssl.SSLContext
import javax.net.ssl.SSLHandshakeException
import javax.net.ssl.TrustManager
import javax.net.ssl.X509TrustManager

internal data class PairingOffer(
    val receiverUrl: String,
    val certificateSha256: String,
    val token: String,
    val expiresAtMs: Long,
) {
    companion object {
        fun parse(payload: String): PairingOffer {
            require(payload.length <= MAX_PAIRING_PAYLOAD) { "pairing QR is too large" }
            val value = JSONObject(payload)
            require(value.getInt("version") == 1) { "pairing QR version is unsupported" }
            val offer = PairingOffer(
                receiverUrl = value.getString("receiver_url").trimEnd('/'),
                certificateSha256 = value.getString("certificate_sha256").lowercase(),
                token = value.getString("token"),
                expiresAtMs = value.getLong("expires_at_ms"),
            )
            require(validReceiverUrl(offer.receiverUrl)) { "pairing receiver URL is invalid" }
            require(decodeSha256(offer.certificateSha256) != null) { "pairing certificate pin is invalid" }
            require(offer.token.isNotBlank() && offer.token.length <= 256) { "pairing token is invalid" }
            require(offer.expiresAtMs >= System.currentTimeMillis()) { "pairing QR has expired" }
            return offer
        }

        private const val MAX_PAIRING_PAYLOAD = 4_096
    }
}

internal sealed class UploadAttempt {
    data object Success : UploadAttempt()
    data class Retry(val message: String) : UploadAttempt()
    data class OperatorAction(val message: String) : UploadAttempt()
}

internal object PinnedHttps {
    fun pair(offer: PairingOffer, deviceName: String): PairingConfig {
        val body = JSONObject()
            .put("token", offer.token)
            .put("device_name", deviceName.take(80))
            .toString()
            .toByteArray(Charsets.UTF_8)
        val connection = connection("${offer.receiverUrl}/v1/pair", offer.certificateSha256).apply {
            requestMethod = "POST"
            doOutput = true
            setRequestProperty("Content-Type", "application/json")
            setFixedLengthStreamingMode(body.size)
        }
        connection.outputStream.use { it.write(body) }
        val status = connection.responseCode
        if (status !in 200..299) {
            connection.disconnect()
            throw IOException("pairing was rejected by the receiver ($status)")
        }
        val response = JSONObject(readBounded(connection))
        connection.disconnect()
        return PairingConfig(
            receiverUrl = offer.receiverUrl,
            certificateSha256 = offer.certificateSha256,
            deviceId = response.getString("device_id"),
            credential = response.getString("credential"),
        )
    }

    fun upload(config: PairingConfig, record: CaptureRecord): UploadAttempt = try {
        val connection = connection(
            "${config.receiverUrl}/v1/captures/${record.id}",
            config.certificateSha256,
        ).apply {
            requestMethod = "PUT"
            doOutput = true
            setRequestProperty("Authorization", "Bearer ${config.credential}")
            setRequestProperty("Content-Type", record.mimeType)
            setRequestProperty("X-Louiselm-Source", "android")
            setRequestProperty("X-Louiselm-Recorded-At-Ms", record.recordedAtMs.toString())
            setRequestProperty("X-Louiselm-Duration-Ms", record.durationMs.toString())
            setRequestProperty("X-Louiselm-Sha256", record.sha256)
            setFixedLengthStreamingMode(record.bytes)
        }
        FileInputStream(record.audio).use { input ->
            connection.outputStream.use { output -> input.copyTo(output) }
        }
        val status = connection.responseCode
        connection.disconnect()
        when (uploadDisposition(status)) {
            UploadDisposition.SUCCESS -> UploadAttempt.Success
            UploadDisposition.RETRY -> UploadAttempt.Retry("receiver is temporarily unavailable ($status)")
            UploadDisposition.OPERATOR_ACTION -> UploadAttempt.OperatorAction("receiver rejected capture ($status)")
        }
    } catch (error: SSLHandshakeException) {
        if (hasCertificateCause(error)) {
            UploadAttempt.OperatorAction("receiver certificate does not match pairing")
        } else {
            UploadAttempt.Retry("secure connection failed")
        }
    } catch (_: IOException) {
        UploadAttempt.Retry("receiver is unreachable")
    } catch (_: SecurityException) {
        UploadAttempt.OperatorAction("receiver security configuration is invalid")
    }

    private fun connection(value: String, certificateSha256: String): HttpsURLConnection {
        val pin = decodeSha256(certificateSha256) ?: throw SecurityException("certificate pin is invalid")
        val trustManager = PinnedTrustManager(pin)
        val context = SSLContext.getInstance("TLS").apply {
            init(null, arrayOf<TrustManager>(trustManager), SecureRandom())
        }
        val url = URL(value)
        require(url.protocol == "https") { "receiver URL must use HTTPS" }
        return (url.openConnection() as HttpsURLConnection).apply {
            connectTimeout = CONNECT_TIMEOUT_MS
            readTimeout = READ_TIMEOUT_MS
            instanceFollowRedirects = false
            sslSocketFactory = context.socketFactory
            hostnameVerifier = HostnameVerifier { _, session ->
                val certificate = runCatching { session.peerCertificates.firstOrNull() as? X509Certificate }.getOrNull()
                certificate != null && MessageDigest.isEqual(pin, sha256(certificate.encoded))
            }
        }
    }

    private fun readBounded(connection: HttpsURLConnection): String {
        val input = connection.inputStream
        val output = ByteArrayOutputStream()
        input.use {
            val buffer = ByteArray(DEFAULT_BUFFER_SIZE)
            while (true) {
                val read = it.read(buffer)
                if (read < 0) break
                require(output.size() + read <= MAX_RESPONSE_BYTES) { "receiver response is too large" }
                output.write(buffer, 0, read)
            }
        }
        return output.toString(Charsets.UTF_8.name())
    }

    private fun hasCertificateCause(error: Throwable): Boolean {
        var current: Throwable? = error
        while (current != null) {
            if (current is CertificateException) return true
            current = current.cause
        }
        return false
    }

    private fun sha256(value: ByteArray): ByteArray = MessageDigest.getInstance("SHA-256").digest(value)

    private class PinnedTrustManager(private val pin: ByteArray) : X509TrustManager {
        override fun checkServerTrusted(chain: Array<out X509Certificate>?, authType: String?) {
            val certificate = chain?.firstOrNull() ?: throw CertificateException("receiver sent no certificate")
            certificate.checkValidity()
            if (!MessageDigest.isEqual(pin, sha256(certificate.encoded))) {
                throw CertificateException("receiver certificate pin mismatch")
            }
        }

        override fun checkClientTrusted(chain: Array<out X509Certificate>?, authType: String?) {
            throw CertificateException("client certificates are not accepted")
        }

        override fun getAcceptedIssuers(): Array<X509Certificate> = emptyArray()
    }

    private const val CONNECT_TIMEOUT_MS = 10_000
    private const val READ_TIMEOUT_MS = 180_000
    private const val MAX_RESPONSE_BYTES = 64 * 1024
}
