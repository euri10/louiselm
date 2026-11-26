package dev.louiselm.capture

import android.annotation.SuppressLint
import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyPermanentlyInvalidatedException
import android.security.keystore.KeyProperties
import android.util.Base64
import java.io.File
import java.io.FileOutputStream
import java.io.IOException
import java.nio.channels.OverlappingFileLockException
import java.nio.file.Files
import java.security.GeneralSecurityException
import java.security.KeyStore
import java.security.KeyStoreException
import java.security.SecureRandom
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec
import javax.crypto.spec.SecretKeySpec

/** Process-only local protected-data access; this period never grants online authority. */
internal class PhoneOfflinePeriod(material: ByteArray, private val startedAtMs: Long) {
    private var material: ByteArray? = material.copyOf()
    private val absoluteDeadline = startedAtMs + 3_600_000L
    private var idleDeadline = startedAtMs + 300_000L

    init {
        require(material.size == 32 && startedAtMs in 0..Long.MAX_VALUE - 3_600_000L)
    }

    @Synchronized
    fun isActive(nowMs: Long): Boolean {
        if (nowMs < startedAtMs || nowMs >= idleDeadline || nowMs >= absoluteDeadline) lock()
        return material != null
    }

    @Synchronized
    fun remainingMs(nowMs: Long): Long = if (isActive(nowMs)) minOf(idleDeadline, absoluteDeadline) - nowMs else 0

    /** Only direct user interaction may reset the local inactivity timer. */
    @Synchronized
    fun confirmInteraction(nowMs: Long): Boolean {
        if (!isActive(nowMs)) return false
        idleDeadline = if (absoluteDeadline - nowMs <= 300_000L) absoluteDeadline else nowMs + 300_000L
        return true
    }

    @Synchronized
    fun key(nowMs: Long): SecretKey? = if (isActive(nowMs)) material?.let { SecretKeySpec(it, "AES") } else null

    @Synchronized
    fun lock() {
        material?.fill(0)
        material = null
    }
}

internal class PreparedPhoneOfflineUnlock internal constructor(
    val operation: Cipher, internal val creating: Boolean, private var payload: ByteArray?,
) {
    @Synchronized
    internal fun take(cipher: Cipher): ByteArray {
        require(cipher === operation)
        val value = payload ?: throw IllegalStateException("offline unlock was discarded")
        payload = null
        return value
    }

    @Synchronized
    fun discard() {
        payload?.fill(0)
        payload = null
    }
}

/** Wraps one local-data key with its own authentication-bound Android Keystore key. */
internal class PhoneOfflineUnlockStore(
    context: Context,
    private val downloads: File,
    preferencesName: String = "phone-offline-key-v1",
    private val draft: Boolean = false,
    private val removeWrappingKey: () -> Unit = ::removePhoneDraftWrappingKey,
    private val wrappingKey: (Boolean) -> SecretKey = if (draft) ::phoneDraftWrappingKey else ::phoneOfflineWrappingKey,
) {
    private val preferences = context.applicationContext.getSharedPreferences(preferencesName, Context.MODE_PRIVATE)
    @Volatile private var lostWrapped: String? = null

    fun hasWrappedKey(): Boolean = preferences.contains(WRAPPED)

    /** Prepare a cipher for BiometricPrompt; no key material is returned before authentication. */
    fun prepare(): PreparedPhoneOfflineUnlock = try {
        withKeyLock {
            val encoded = preferences.getString(WRAPPED, null)
            val cipher = Cipher.getInstance("AES/GCM/NoPadding")
            if (encoded == null) {
                if (hasDownloads()) throw KeyStoreException("download key is missing")
                cipher.init(Cipher.ENCRYPT_MODE, wrappingKey(true))
                cipher.updateAAD(if (draft) DRAFT_AAD else DOWNLOAD_AAD)
                return@withKeyLock PreparedPhoneOfflineUnlock(cipher, true, ByteArray(32).also { SecureRandom().nextBytes(it) })
            }
            val wrapped = try { Base64.decode(encoded, Base64.NO_WRAP) } catch (error: IllegalArgumentException) {
                throw GeneralSecurityException("download key is unreadable", error)
            }
            if (wrapped.size != WRAPPED_BYTES) throw GeneralSecurityException("download key is unreadable")
            cipher.init(Cipher.DECRYPT_MODE, wrappingKey(false), GCMParameterSpec(128, wrapped, 0, NONCE_BYTES))
            cipher.updateAAD(if (draft) DRAFT_AAD else DOWNLOAD_AAD)
            PreparedPhoneOfflineUnlock(cipher, false, wrapped.copyOfRange(NONCE_BYTES, wrapped.size))
        }
    } catch (error: KeyPermanentlyInvalidatedException) {
        rememberLoss()
        throw error
    } catch (error: PhoneDraftKeyLost) {
        rememberLoss()
        throw error
    }

    /** Finish only the exact cipher returned by the system authentication callback. */
    fun finish(prepared: PreparedPhoneOfflineUnlock, cipher: Cipher, startedAtMs: Long): PhoneOfflinePeriod {
        val payload = prepared.take(cipher)
        var material: ByteArray? = null
        try {
            if (prepared.creating) {
                val wrapped = cipher.iv + cipher.doFinal(payload)
                if (wrapped.size != WRAPPED_BYTES) throw GeneralSecurityException("invalid wrapped download key")
                publishWrappedKey(wrapped, payload)
                material = payload
            } else {
                material = cipher.doFinal(payload)
            }
            if (material.size != 32) throw GeneralSecurityException("invalid download key")
            return PhoneOfflinePeriod(material, startedAtMs)
        } catch (error: KeyPermanentlyInvalidatedException) {
            rememberLoss()
            throw error
        } finally {
            material?.fill(0)
            payload.fill(0)
        }
    }

    @SuppressLint("UseKtx") // The Boolean is the durability result, not a best-effort preference edit.
    private fun publishWrappedKey(wrapped: ByteArray, material: ByteArray) {
        withKeyLock {
            if (hasWrappedKey() || hasDownloads()) throw IOException("local key changed; retry unlock")
            if (draft) PhoneDraftStore(downloads).installKey(SecretKeySpec(material, "AES"))
            if (!preferences.edit().putString(WRAPPED, Base64.encodeToString(wrapped, Base64.NO_WRAP)).commit()) {
                throw IOException("download key could not be stored")
            }
        }
    }

    fun hasProvenDraftKeyLoss(): Boolean = draft && lostWrapped != null

    private fun rememberLoss() {
        if (draft) lostWrapped = preferences.getString(WRAPPED, null)
    }

    /** Called only after a permanent loss was shown and the operator confirmed deletion. */
    @SuppressLint("UseKtx") // Reset must not claim success unless the preference removal is durable.
    fun resetLostDraftKey() = withKeyLock {
        val expected = lostWrapped ?: throw IllegalStateException("draft key loss has not been proved")
        if (!draft || preferences.getString(WRAPPED, null) != expected) throw IOException("draft key changed")
        PhoneDraftStore(downloads).resetLost()
        removeWrappingKey()
        if (!preferences.edit().remove(WRAPPED).commit()) throw IOException("draft key reset failed")
        lostWrapped = null
    }

    private fun <T> withKeyLock(block: () -> T): T {
        if (Files.isSymbolicLink(downloads.toPath())) throw IOException("invalid download path")
        if (!downloads.isDirectory && !downloads.mkdirs() && !downloads.isDirectory) throw IOException("downloads unavailable")
        return FileOutputStream(File(downloads, ".key-lock"), true).channel.use { channel ->
            val lock = try { channel.tryLock() } catch (_: OverlappingFileLockException) { null }
                ?: throw IOException("download key is busy")
            lock.use { block() }
        }
    }

    private fun hasDownloads(): Boolean {
        if (!downloads.exists()) return false
        val directories = downloads.listFiles() ?: throw IOException("downloads unavailable")
        if (draft) return directories.any { it.name.endsWith(".draft") }
        return directories.filter { it.isDirectory }.any { directory ->
            val files = directory.listFiles() ?: throw IOException("downloads unavailable")
            files.any { it.name.endsWith(".meta") || it.name.endsWith(".data") }
        }
    }

    private companion object {
        const val WRAPPED = "wrapped"
        const val NONCE_BYTES = 12
        const val WRAPPED_BYTES = NONCE_BYTES + 32 + 16
        val DOWNLOAD_AAD = "louiselm-offline-download-key-v1".toByteArray(Charsets.UTF_8)
        val DRAFT_AAD = "louiselm-offline-draft-key-v1".toByteArray(Charsets.UTF_8)
    }
}

private fun phoneOfflineWrappingKey(create: Boolean): SecretKey = phoneLocalWrappingKey(OFFLINE_KEY_ALIAS, create)
private fun phoneDraftWrappingKey(create: Boolean): SecretKey = phoneLocalWrappingKey(DRAFT_KEY_ALIAS, create)

private fun phoneLocalWrappingKey(alias: String, create: Boolean): SecretKey {
    val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
    (store.getKey(alias, null) as? SecretKey)?.let { return it }
    if (!create) {
        if (alias == DRAFT_KEY_ALIAS) throw PhoneDraftKeyLost()
        throw KeyStoreException("download wrapping key is missing")
    }
    return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
        init(KeyGenParameterSpec.Builder(alias, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
            .setBlockModes(KeyProperties.BLOCK_MODE_GCM).setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
            .setKeySize(256).setUserAuthenticationRequired(true)
            .setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL)
            .build())
        generateKey()
    }
}

private const val OFFLINE_KEY_ALIAS = "louiselm-offline-download-wrap-v1"

private const val DRAFT_KEY_ALIAS = "louiselm-offline-draft-wrap-v1"

private class PhoneDraftKeyLost : KeyStoreException("draft wrapping key is permanently missing")

private fun removePhoneDraftWrappingKey() {
    KeyStore.getInstance("AndroidKeyStore").apply { load(null); deleteEntry(DRAFT_KEY_ALIAS) }
}
