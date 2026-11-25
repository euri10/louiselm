package dev.louiselm.capture

import android.content.Context
import android.os.CancellationSignal
import android.os.Looper
import android.os.PowerManager
import android.app.KeyguardManager
import android.view.View
import android.view.ViewGroup
import android.widget.Button
import android.widget.TextView
import androidx.work.Configuration
import androidx.work.WorkManager
import androidx.work.impl.WorkManagerImpl
import java.io.File
import java.security.KeyFactory
import java.security.Signature
import java.security.spec.PKCS8EncodedKeySpec
import java.util.concurrent.ExecutorService
import javax.crypto.spec.SecretKeySpec
import org.json.JSONObject
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Assume.assumeTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.Robolectric
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.Shadows.shadowOf
import org.robolectric.android.util.concurrent.PausedExecutorService
import org.robolectric.annotation.Config
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements
import org.robolectric.annotation.LooperMode
import org.robolectric.util.ReflectionHelpers

private fun integrationConfig() = PairingConfig(
    requireNotNull(System.getenv("LOUISELM_E2E_URL")),
    requireNotNull(System.getenv("LOUISELM_E2E_RECEIVER")),
    requireNotNull(System.getenv("LOUISELM_E2E_DEVICE")),
    requireNotNull(System.getenv("LOUISELM_E2E_CREDENTIAL")),
)

@Implements(PairingStore::class, isInAndroidSdk = false)
class EndToEndPairingFixture {
    @Implementation(methodName = "load")
    internal fun load(): PairingConfig = integrationConfig()
    @Implementation
    fun binding(): String = "isolated-test-pairing"
}

@Implements(PhoneControlKeys::class, isInAndroidSdk = false)
class EndToEndKeysFixture {
    private fun fixture(): JSONObject = JSONObject(requireNotNull(javaClass.getResourceAsStream("/phone-control-v1.json"))
        .bufferedReader().use { it.readText() }).getJSONObject("keys").getJSONObject("enrolled")
    @Implementation
    fun publicKey(): ByteArray = phoneControlHex(fixture().getString("sec1"))
    @Implementation
    fun signingOperation(): Signature {
        val privateKey = KeyFactory.getInstance("EC").generatePrivate(PKCS8EncodedKeySpec(phoneControlHex(fixture().getString("pkcs8"))))
        return Signature.getInstance("SHA256withECDSA").apply { initSign(privateKey) }
    }
}

@Implements(PhoneControlAuthenticator::class, isInAndroidSdk = false)
class EndToEndAuthenticatorFixture {
    @Implementation
    fun start(signing: Signature, success: (Signature?) -> Unit, @Suppress("UNUSED_PARAMETER") error: (Boolean) -> Unit): CancellationSignal {
        success(signing)
        return CancellationSignal()
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30], shadows = [EndToEndPairingFixture::class, EndToEndKeysFixture::class, EndToEndAuthenticatorFixture::class,
    PhoneScreenOfflineUnlockFixture::class, PhoneScreenOfflineAuthenticatorFixture::class],
    instrumentedPackages = ["dev.louiselm.capture"])
@LooperMode(LooperMode.Mode.PAUSED)
class PhoneControlEndToEndTest {
    private fun views(view: View): Sequence<View> = sequence {
        yield(view)
        if (view is ViewGroup) for (index in 0 until view.childCount) yieldAll(views(view.getChildAt(index)))
    }

    @Test
    fun tapUnlockPinnedHttpsAndDisplayARealHeadlessSession() {
        assumeTrue(System.getenv("LOUISELM_E2E_URL") != null)
        val app = RuntimeEnvironment.getApplication()
        shadowOf(app).grantPermissions("${app.packageName}.DYNAMIC_RECEIVER_NOT_EXPORTED_PERMISSION")
        shadowOf(app.getSystemService(PowerManager::class.java)).turnScreenOn(true)
        shadowOf(app.getSystemService(KeyguardManager::class.java)).setIsDeviceLocked(false)
        val work = PausedExecutorService()
        WorkManager.initialize(app, Configuration.Builder().setExecutor(work).setTaskExecutor(work).build())
        val config = integrationConfig()
        val key = JSONObject(requireNotNull(javaClass.getResourceAsStream("/phone-control-v1.json"))
            .bufferedReader().use { it.readText() }).getJSONObject("proof").getString("enrolled_key_hash")
        PhoneControlStore(app).saveGrant(PhoneControlGrant(requireNotNull(System.getenv("LOUISELM_E2E_GRANT")), 1,
            config.receiverIdentitySha256, phoneControlPairingBinding(config), key))
        val main = Robolectric.buildActivity(MainActivity::class.java)
        val mainActivity = main.get()
        ReflectionHelpers.getField<ExecutorService>(mainActivity, "networkExecutor").shutdown()
        val mainExecutor = PausedExecutorService()
        ReflectionHelpers.setField(mainActivity, "networkExecutor", mainExecutor)
        main.setup()
        fun drain(executor: PausedExecutorService) { executor.runAll(); shadowOf(Looper.getMainLooper()).idle() }
        drain(mainExecutor)
        val open = views(mainActivity.window.decorView).filterIsInstance<Button>()
            .single { it.text.toString() == mainActivity.getString(R.string.phone_open_session) }
        open.performClick()
        val intent = shadowOf(mainActivity).nextStartedActivity
        val phone = Robolectric.buildActivity(PhoneControlActivity::class.java, intent)
        val phoneActivity = phone.get()
        ReflectionHelpers.getField<ExecutorService>(phoneActivity, "networkExecutor").shutdown()
        val phoneExecutor = PausedExecutorService()
        ReflectionHelpers.setField(phoneActivity, "networkExecutor", phoneExecutor)
        try {
            phone.setup()
            drain(phoneExecutor)
            val unlock = views(phoneActivity.window.decorView).filterIsInstance<Button>()
                .single { it.text.toString() == phoneActivity.getString(R.string.phone_unlock) }
            unlock.performClick()
            repeat(8) { drain(phoneExecutor) }
            assertTrue(views(phoneActivity.window.decorView).filterIsInstance<TextView>()
                .any { it.text.toString().contains("fixture request") })
            val store = PhoneOfflineStore(File(app.filesDir, "phone-offline-v1"))
            val offlineKey = SecretKeySpec(ByteArray(32) { 7 }, "AES")
            val copy = store.listAll(offlineKey).copies.single()
            assertTrue(store.readPage(copy.source, 0, offlineKey).entries
                .any { it is PhoneControlEntry.Message && it.text == "fixture request" })
            assertEquals("unavailable", store.readPage(copy.source, 0, offlineKey).source.archive)
            views(phoneActivity.window.decorView).filterIsInstance<Button>()
                .single { it.text.toString() == phoneActivity.getString(R.string.phone_offline_copies) }.performClick()
            repeat(2) { drain(phoneExecutor) }
            views(phoneActivity.window.decorView).filterIsInstance<Button>()
                .single { it.text.toString().contains("Saved:") }.performClick()
            repeat(2) { drain(phoneExecutor) }
            assertTrue(views(phoneActivity.window.decorView).filterIsInstance<TextView>()
                .any { it.text.toString().contains("fixture request") })
            assertTrue(views(phoneActivity.window.decorView).filterIsInstance<TextView>()
                .any { it.text.toString() == phoneActivity.getString(R.string.phone_archive_unavailable) })
            val rejected = phoneExecutor.submit<PhoneControlReply> {
                PinnedHttps.phoneControl(config.copy(receiverIdentitySha256 = "ff".repeat(32)), "challenge",
                    JSONObject().put("v", 1).put("type", "challenge"), { true })
            }
            phoneExecutor.runAll()
            assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.UNAVAILABLE), rejected.get())
            phone.close()
            assertFalse(views(phoneActivity.window.decorView).filterIsInstance<TextView>()
                .any { it.text.toString().contains("fixture request") })
        } finally {
            if (!phoneActivity.isDestroyed) phone.close()
            main.close()
            phoneExecutor.runAll(); mainExecutor.runAll(); shadowOf(Looper.getMainLooper()).idle()
            phoneExecutor.shutdownNow(); mainExecutor.shutdownNow()
            @Suppress("RestrictedApi") // This fixture owns the isolated WorkManager instance.
            if (WorkManagerImpl.isInitialized()) WorkManagerImpl.getInstance(app).closeDatabase()
            @Suppress("RestrictedApi")
            WorkManagerImpl.setDelegate(null)
            ReflectionHelpers.setStaticField(WorkManagerImpl::class.java, "sDefaultInstance", null)
            work.shutdownNow()
        }
    }
}
