package dev.louiselm.capture

import android.content.pm.PackageManager
import com.google.android.gms.tasks.Task
import com.google.android.gms.tasks.Tasks
import com.google.firebase.installations.FirebaseInstallations
import com.google.firebase.messaging.FirebaseMessaging
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertThrows
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements
import org.robolectric.annotation.Resetter
import org.robolectric.shadow.api.Shadow
import java.io.IOException
import java.util.concurrent.ExecutionException
import java.util.concurrent.Executors

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [28, 37], shadows = [MessagingRegistrationShadow::class, InstallationShadow::class])
class AttentionInstallationTest {
    @Before
    fun resetSdkFixtures() {
        MessagingRegistrationShadow.reset()
        InstallationShadow.reset()
    }

    private fun prepare(reset: Boolean, callback: String? = null): String {
        val executor = Executors.newSingleThreadExecutor()
        try {
            return executor.submit<String> { currentReceiverInstallation(reset, callback) }.get()
        } finally {
            executor.shutdownNow()
        }
    }

    @Test
    fun replacementUnregistersAndDeletesBeforeRegisteringFreshIdentity() {
        assertEquals(InstallationShadow.NEW_FID, prepare(true, InstallationShadow.OLD_FID))
        assertEquals(listOf("auto:false", "unregister", "delete", "id", "register", "id"), InstallationShadow.calls)
    }

    @Test
    fun callbackForCurrentIdentityDoesNotRegisterAgainButStaleCallbackDoes() {
        assertEquals(InstallationShadow.OLD_FID, prepare(false, InstallationShadow.OLD_FID))
        assertEquals(listOf("id", "id"), InstallationShadow.calls)
        InstallationShadow.calls.clear()
        assertEquals(InstallationShadow.OLD_FID, prepare(false, InstallationShadow.NEW_FID))
        assertEquals(listOf("id", "register", "id"), InstallationShadow.calls)
        InstallationShadow.calls.clear()
        prepare(false)
        assertEquals(listOf("id", "register", "id"), InstallationShadow.calls)
    }

    @Test
    fun failedUnregisterCannotDeleteOrRegisterAndConcurrentRotationRetries() {
        MessagingRegistrationShadow.failUnregister = true
        assertThrows(ExecutionException::class.java) { prepare(true) }
        assertEquals(listOf("auto:false", "unregister"), InstallationShadow.calls)
        MessagingRegistrationShadow.failUnregister = false
        MessagingRegistrationShadow.rotateOnRegister = true
        val failure = assertThrows(ExecutionException::class.java) { prepare(false) }
        assertTrue(failure.cause is IOException)
    }

    @Test
    fun wireBodyIsExactlyFidAndManifestEnablesInstallationModeWithoutStartupRegistration() {
        val json = JSONObject(String(attentionInstallationBody(InstallationShadow.OLD_FID), Charsets.UTF_8))
        assertEquals(setOf("fid"), json.keys().asSequence().toSet())
        assertEquals(InstallationShadow.OLD_FID, json.getString("fid"))
        for (bad in listOf("", "legacy-token", "x".repeat(21), "x".repeat(23), "!".repeat(22), "é".repeat(22))) {
            assertThrows(IllegalArgumentException::class.java) { attentionInstallationBody(bad) }
        }
        val context = RuntimeEnvironment.getApplication()
        val metadata = context.packageManager.getApplicationInfo(context.packageName, PackageManager.GET_META_DATA).metaData
        assertTrue(metadata.getBoolean("firebase_messaging_installation_id_enabled"))
        assertFalse(metadata.getBoolean("firebase_messaging_auto_init_enabled"))
    }
}

/** Test-only SDK boundary: no Firebase app, credentials or network are initialized. */
@Implements(FirebaseMessaging::class)
class MessagingRegistrationShadow {
    @Implementation
    fun setAutoInitEnabled(enabled: Boolean) { InstallationShadow.calls += "auto:$enabled" }

    @Implementation
    fun unregister(): Task<Void> {
        InstallationShadow.calls += "unregister"
        return if (failUnregister) Tasks.forException(IOException("offline")) else Tasks.forResult(null)
    }

    @Implementation
    fun register(): Task<Void> {
        InstallationShadow.calls += "register"
        if (rotateOnRegister) InstallationShadow.current = InstallationShadow.NEW_FID
        return Tasks.forResult(null)
    }

    companion object {
        var failUnregister = false
        var rotateOnRegister = false
        @JvmStatic @Implementation
        fun getInstance(): FirebaseMessaging = Shadow.newInstanceOf(FirebaseMessaging::class.java)
        @JvmStatic @Resetter
        fun reset() { failUnregister = false; rotateOnRegister = false }
    }
}

@Implements(FirebaseInstallations::class)
class InstallationShadow {
    @Implementation
    fun getId(): Task<String> { calls += "id"; return Tasks.forResult(current) }

    @Implementation
    fun delete(): Task<Void> { calls += "delete"; current = NEW_FID; return Tasks.forResult(null) }

    companion object {
        const val OLD_FID = "c123456789012345678901"
        const val NEW_FID = "d123456789012345678901"
        var current = OLD_FID
        val calls = mutableListOf<String>()
        @JvmStatic @Implementation
        fun getInstance(): FirebaseInstallations = Shadow.newInstanceOf(FirebaseInstallations::class.java)
        @JvmStatic @Resetter
        fun reset() { current = OLD_FID; calls.clear() }
    }
}
