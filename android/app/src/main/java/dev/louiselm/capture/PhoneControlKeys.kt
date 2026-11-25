package dev.louiselm.capture

import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.security.KeyPair
import java.security.KeyPairGenerator
import java.security.KeyStore
import java.security.PublicKey
import java.security.Signature
import java.security.interfaces.ECPublicKey
import java.security.spec.ECGenParameterSpec

internal interface PhoneControlKeyAccess {
    fun enrollmentPublicKey(): ByteArray
    fun publicKey(): ByteArray
    fun signingOperation(): Signature
}

internal class PhoneControlKeys : PhoneControlKeyAccess {
    /** Create the permanent key only during explicit enrollment; its private key stays in Keystore. */
    override fun enrollmentPublicKey(): ByteArray {
        val store = keyStore()
        if (!store.containsAlias(ALIAS)) {
            KeyPairGenerator.getInstance(KeyProperties.KEY_ALGORITHM_EC, "AndroidKeyStore").apply {
                initialize(KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_SIGN)
                    .setAlgorithmParameterSpec(ECGenParameterSpec("secp256r1"))
                    .setDigests(KeyProperties.DIGEST_SHA256)
                    .setUserAuthenticationRequired(true)
                    .setUserAuthenticationParameters(0, KeyProperties.AUTH_BIOMETRIC_STRONG or KeyProperties.AUTH_DEVICE_CREDENTIAL)
                    .build())
                generateKeyPair()
            }
        }
        return publicKey()
    }

    override fun publicKey(): ByteArray {
        val certificate = keyStore().getCertificate(ALIAS) ?: throw java.security.KeyStoreException("phone-control key is missing")
        return phoneControlPublicKey(certificate.publicKey)
    }

    /** Prepare an operation for BiometricPrompt without using it before authentication. */
    override fun signingOperation(): Signature {
        val key = keyStore().getKey(ALIAS, null) as? java.security.PrivateKey
            ?: throw java.security.KeyStoreException("phone-control key is missing")
        return Signature.getInstance("SHA256withECDSA").apply {
            initSign(key)
        }
    }

    private fun keyStore(): KeyStore = KeyStore.getInstance("AndroidKeyStore").apply { load(null) }

    companion object {
        private const val ALIAS = "louiselm-phone-control-v1"
    }
}

internal fun phoneControlWorkingKey(): KeyPair = KeyPairGenerator.getInstance("EC").run {
    initialize(ECGenParameterSpec("secp256r1"))
    generateKeyPair()
}

internal fun phoneControlPublicKey(key: PublicKey): ByteArray {
    val point = (key as? ECPublicKey)?.w ?: throw IllegalArgumentException("invalid EC public key")
    fun coordinate(value: java.math.BigInteger): ByteArray {
        require(value.signum() >= 0 && value.bitLength() <= 256) { "invalid EC coordinate" }
        val encoded = value.toByteArray()
        val unsigned = if (encoded.size == 33 && encoded[0] == 0.toByte()) encoded.copyOfRange(1, 33) else encoded
        require(unsigned.size <= 32) { "invalid EC coordinate" }
        return ByteArray(32 - unsigned.size) + unsigned
    }
    return byteArrayOf(4) + coordinate(point.affineX) + coordinate(point.affineY)
}
