package dev.louiselm.capture

import android.content.Context
import java.security.MessageDigest
import java.security.PrivateKey
import java.security.PublicKey
import java.security.Signature
import java.security.SignatureException
import java.security.KeyFactory
import java.security.spec.X509EncodedKeySpec
import org.json.JSONObject
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config
import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements

/** Models an authentication-per-use Keystore operation: updates require a prompt first. */
internal class AuthGatedSignature : Signature("SHA256withECDSA") {
    private val delegate = Signature.getInstance("SHA256withECDSA")
    var authenticated = false

    override fun engineInitSign(key: PrivateKey) = delegate.initSign(key)
    override fun engineInitVerify(key: PublicKey) = delegate.initVerify(key)
    override fun engineUpdate(value: Byte) {
        if (!authenticated) throw SignatureException("authentication required before update")
        delegate.update(value)
    }
    override fun engineUpdate(bytes: ByteArray, offset: Int, length: Int) {
        if (!authenticated) throw SignatureException("authentication required before update")
        delegate.update(bytes, offset, length)
    }
    override fun engineSign(): ByteArray = delegate.sign()
    override fun engineVerify(signature: ByteArray): Boolean = delegate.verify(signature)
    @Deprecated("Required by Signature")
    @Suppress("DEPRECATION")
    override fun engineSetParameter(name: String, value: Any) = delegate.setParameter(name, value)
    @Deprecated("Required by Signature")
    @Suppress("DEPRECATION")
    override fun engineGetParameter(name: String): Any = delegate.getParameter(name)
}

@Implements(PairingStore::class, isInAndroidSdk = false)
class PhoneControlPairingFixture {
    @Implementation(methodName = "load")
    internal fun load(): PairingConfig? {
        val preferences = RuntimeEnvironment.getApplication().getSharedPreferences("control-pairing-fixture", Context.MODE_PRIVATE)
        if (!preferences.getBoolean("paired", true)) return null
        return PairingConfig(preferences.getString("url", "https://192.0.2.1") ?: "", "aa".repeat(32),
            "fixture", preferences.getString("credential", "dummy") ?: "")
    }
}

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], shadows = [PhoneControlPairingFixture::class], instrumentedPackages = ["dev.louiselm.capture"])
class PhoneControlClientTest {
    @Test
    fun enrollmentAndUnlockBindTheReceiverPairingAndBothKeys() {
        val fixture = JSONObject(requireNotNull(javaClass.getResourceAsStream("/phone-control-v1.json"))
            .bufferedReader().use { it.readText() }).getJSONObject("keys").getJSONObject("enrolled")
        val fixtureKey = KeyFactory.getInstance("EC").generatePublic(X509EncodedKeySpec(phoneControlHex(fixture.getString("spki"))))
        assertArrayEquals(phoneControlHex(fixture.getString("sec1"), 65), phoneControlPublicKey(fixtureKey))
        val context = RuntimeEnvironment.getApplication()
        context.getSharedPreferences("phone-control-v1", Context.MODE_PRIVATE).edit().clear().commit()
        context.getSharedPreferences("control-pairing-fixture", Context.MODE_PRIVATE).edit().clear().commit()
        val pair = phoneControlWorkingKey()
        val keys = object : PhoneControlKeyAccess {
            override fun enrollmentPublicKey() = phoneControlPublicKey(pair.public)
            override fun publicKey() = phoneControlPublicKey(pair.public)
            override fun signingOperation(): Signature = AuthGatedSignature().apply {
                initSign(pair.private)
            }
        }
        val keyHash = phoneControlHex(MessageDigest.getInstance("SHA-256").digest(keys.publicKey()))
        val calls = mutableListOf<String>()
        var wrongReceiver = false
        val remote: PhoneControlTransport = { _, path, body, maySend ->
            assertTrue(maySend())
            calls += path
            when (path) {
                "enrollment/offer" -> PhoneControlReply.Offer("11".repeat(16), "00123456", "aa".repeat(32), 1000)
                "enrollment/submit" -> {
                    assertEquals(phoneControlHex(keys.publicKey()), body.getString("enrolled_key"))
                    PhoneControlReply.Pending("11".repeat(16))
                }
                "enrollment/status" -> PhoneControlReply.Granted("11".repeat(16), "22".repeat(16), 3, keyHash, "aa".repeat(32))
                "challenge" -> PhoneControlReply.Challenge("22".repeat(16), 3,
                    (if (wrongReceiver) "bb" else "aa").repeat(32), "33".repeat(16), "44".repeat(32), 1000)
                "unlock" -> {
                    val proof = body.getJSONObject("proof")
                    val signed = PhoneControlProof(proof.getString("grant_id"), proof.getLong("grant_revision"),
                        proof.getString("enrolled_key_hash"), proof.getString("receiver_identity"),
                        proof.getString("challenge_id"), proof.getString("challenge"), proof.getString("working_key"))
                    assertTrue(Signature.getInstance("SHA256withECDSA").run {
                        initVerify(pair.public)
                        update(signed.transcript())
                        verify(phoneControlHex(proof.getString("signature")))
                    })
                    PhoneControlReply.Unlocked("22".repeat(16), 3, "55".repeat(16), 1000, 2000)
                }
                else -> throw AssertionError("unexpected request $path")
            }
        }
        val client = PhoneControlClient(context, keys, remote)
        val pending = (client.enroll { true } as PhoneControlResult.Value).value
        assertEquals("00123456", pending.matchingCode)
        val grant = requireNotNull((client.checkEnrollment { true } as PhoneControlResult.Value).value)
        assertEquals("22".repeat(16), grant.grantId)
        wrongReceiver = true
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.prepareUnlock { true })
        wrongReceiver = false
        val preparation = client.prepareUnlock { true }
        assertTrue("preparing the biometric prompt must not update an unauthenticated Keystore operation", preparation is PhoneControlResult.Value)
        val discarded = (preparation as PhoneControlResult.Value).value
        (discarded.signing as AuthGatedSignature).authenticated = true
        discarded.signing.update(discarded.proof.transcript())
        val obsoleteSignature = discarded.signing.sign()
        discarded.discard()
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE),
            client.finishUnlock(discarded, obsoleteSignature, 10) { true })
        val prepared = (client.prepareUnlock { true } as PhoneControlResult.Value).value
        (prepared.signing as AuthGatedSignature).authenticated = true
        prepared.signing.update(prepared.proof.transcript())
        val period = (client.finishUnlock(prepared, prepared.signing.sign(), 10) { true } as PhoneControlResult.Value).value
        assertTrue(period.isActive(11))
        assertEquals(listOf("enrollment/offer", "enrollment/submit", "enrollment/status", "challenge", "challenge", "challenge", "unlock"), calls)
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE), client.prepareUnlock { false })
        assertEquals(7, calls.size)
        val pairing = context.getSharedPreferences("control-pairing-fixture", Context.MODE_PRIVATE)
        pairing.edit().putString("url", "https://192.0.2.2").commit()
        assertTrue((client.status() as PhoneControlResult.Value).value is PhoneControlStatus.Granted)
        pairing.edit().putString("credential", "replacement").commit()
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.REVOKED), client.status())
        PhoneControlStore(context).saveGrant(grant)
        pairing.edit().putBoolean("paired", false).commit()
        assertEquals(PhoneControlResult.Value(PhoneControlStatus.Disabled), client.status())
        assertEquals(null, PhoneControlStore(context).grant())
    }
}
