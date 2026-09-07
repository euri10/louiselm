package dev.louiselm.capture

import android.annotation.SuppressLint
import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import android.util.Base64
import org.json.JSONObject
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

internal data class PairingConfig(
    val receiverUrl: String,
    val receiverIdentitySha256: String,
    val deviceId: String,
    val credential: String,
)

internal class PairingStore(context: Context) {
    private val preferences = context.applicationContext.getSharedPreferences(PREFERENCES, Context.MODE_PRIVATE)

    @SuppressLint("UseKtx") // KTX edit returns Unit; credential durability requires the commit Boolean.
    fun save(config: PairingConfig) {
        require(validReceiverUrl(config.receiverUrl)) { "receiver URL is invalid" }
        require(decodeSha256(config.receiverIdentitySha256) != null) { "receiver identity is invalid" }
        require(
            config.deviceId.isNotBlank() &&
                config.deviceId.length <= 80 &&
                config.credential.isNotBlank() &&
                config.credential.length <= 256,
        ) { "device credential is invalid" }
        val plaintext = JSONObject()
            .put("receiver_url", config.receiverUrl)
            .put("receiver_identity_sha256", config.receiverIdentitySha256)
            .put("device_id", config.deviceId)
            .put("credential", config.credential)
            .toString()
            .toByteArray(Charsets.UTF_8)
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, key())
        cipher.updateAAD(ASSOCIATED_DATA)
        val ciphertext = cipher.doFinal(plaintext)
        check(preferences.edit()
            .putString(NONCE, Base64.encodeToString(cipher.iv, Base64.NO_WRAP))
            .putString(CIPHERTEXT, Base64.encodeToString(ciphertext, Base64.NO_WRAP))
            .commit()) { "pairing credential could not be persisted" }
    }

    fun load(): PairingConfig? {
        val nonce = preferences.getString(NONCE, null) ?: return null
        val ciphertext = preferences.getString(CIPHERTEXT, null) ?: return null
        val plaintext = runCatching {
            val cipher = Cipher.getInstance(TRANSFORMATION)
            cipher.init(
                Cipher.DECRYPT_MODE,
                key(),
                GCMParameterSpec(128, Base64.decode(nonce, Base64.NO_WRAP)),
            )
            cipher.updateAAD(ASSOCIATED_DATA)
            cipher.doFinal(Base64.decode(ciphertext, Base64.NO_WRAP))
        }.getOrElse { throw SecurityException("stored pairing credential cannot be decrypted") }
        val value = JSONObject(String(plaintext, Charsets.UTF_8))
        return PairingConfig(
            receiverUrl = value.getString("receiver_url"),
            receiverIdentitySha256 = value.getString("receiver_identity_sha256"),
            deviceId = value.getString("device_id"),
            credential = value.getString("credential"),
        ).also {
            require(
                validReceiverUrl(it.receiverUrl) &&
                    decodeSha256(it.receiverIdentitySha256) != null &&
                    it.deviceId.isNotBlank() &&
                    it.deviceId.length <= 80 &&
                    it.credential.isNotBlank() &&
                    it.credential.length <= 256,
            ) {
                "stored pairing configuration is invalid"
            }
        }
    }

    private fun key(): SecretKey {
        val store = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }
        (store.getKey(KEY_ALIAS, null) as? SecretKey)?.let { return it }
        return KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, "AndroidKeyStore").run {
            init(
                KeyGenParameterSpec.Builder(
                    KEY_ALIAS,
                    KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT,
                )
                    .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                    .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                    .setKeySize(256)
                    .build(),
            )
            generateKey()
        }
    }

    companion object {
        private const val PREFERENCES = "pairing-v2"
        private const val KEY_ALIAS = "louiselm-capture-pairing-v2"
        private const val TRANSFORMATION = "AES/GCM/NoPadding"
        private const val NONCE = "nonce"
        private const val CIPHERTEXT = "ciphertext"
        private val ASSOCIATED_DATA = KEY_ALIAS.toByteArray(Charsets.UTF_8)
    }
}
