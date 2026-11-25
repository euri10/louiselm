package dev.louiselm.capture

import android.app.Activity
import android.hardware.biometrics.BiometricManager
import android.hardware.biometrics.BiometricPrompt
import android.os.CancellationSignal
import java.security.Signature

internal class PhoneControlAuthenticator(private val activity: Activity) {
    fun start(signing: Signature, success: (Signature?) -> Unit, error: (Boolean) -> Unit): CancellationSignal {
        val cancellation = CancellationSignal()
        val prompt = BiometricPrompt.Builder(activity)
            .setTitle(activity.getString(R.string.phone_auth_title))
            .setAllowedAuthenticators(BiometricManager.Authenticators.BIOMETRIC_STRONG or BiometricManager.Authenticators.DEVICE_CREDENTIAL)
            .build()
        prompt.authenticate(BiometricPrompt.CryptoObject(signing), cancellation, activity.mainExecutor,
            object : BiometricPrompt.AuthenticationCallback() {
                override fun onAuthenticationSucceeded(result: BiometricPrompt.AuthenticationResult) {
                    success(result.cryptoObject?.signature)
                }

                override fun onAuthenticationError(errorCode: Int, errString: CharSequence) {
                    error(errorCode == BiometricPrompt.BIOMETRIC_ERROR_CANCELED ||
                        errorCode == BiometricPrompt.BIOMETRIC_ERROR_USER_CANCELED)
                }
            })
        return cancellation
    }
}
