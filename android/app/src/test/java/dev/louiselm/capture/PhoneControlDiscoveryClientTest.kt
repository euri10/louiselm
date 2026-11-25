package dev.louiselm.capture

import android.content.Context
import java.security.MessageDigest
import java.security.Signature
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [34], shadows = [PhoneControlPairingFixture::class], instrumentedPackages = ["dev.louiselm.capture"])
class PhoneControlDiscoveryClientTest {
    @Test
    fun signsScopedDiscoveryAndRejectsMismatchedRepliesAndLateDelivery() {
        val context = RuntimeEnvironment.getApplication()
        context.getSharedPreferences("phone-control-v1", Context.MODE_PRIVATE).edit().clear().commit()
        context.getSharedPreferences("control-pairing-fixture", Context.MODE_PRIVATE).edit().clear().commit()
        val enrolled = phoneControlWorkingKey()
        val working = phoneControlWorkingKey()
        val keys = object : PhoneControlKeyAccess {
            override fun enrollmentPublicKey() = phoneControlPublicKey(enrolled.public)
            override fun publicKey() = phoneControlPublicKey(enrolled.public)
            override fun signingOperation() = Signature.getInstance("SHA256withECDSA").apply { initSign(enrolled.private) }
        }
        val config = requireNotNull(PairingStore(context).load())
        val grant = PhoneControlGrant("11".repeat(16), 1, "aa".repeat(32), phoneControlPairingBinding(config),
            phoneControlHex(MessageDigest.getInstance("SHA-256").digest(keys.publicKey())))
        PhoneControlStore(context).saveGrant(grant)
        val period = PhoneControlPeriod(grant, "22".repeat(16), working.private, 0)
        val controller = "33".repeat(16)
        val otherController = "44".repeat(16)
        var mismatched = false
        var includeOther = false
        var deliver = true
        val transport: PhoneControlTransport = { _, _, body, _ ->
            val wire = body.getJSONObject("request")
            val action = wire.getInt("action")
            val id = wire.getString("operation_id")
            val request = PhoneControlRequest(grant.grantId, grant.revision, period.periodId, grant.receiverIdentity,
                wire.getString("controller_id"), wire.getString("subject_id"), phoneControlHex(wire.getString("payload")),
                id, wire.getLong("expected_revision"), wire.getInt("interaction") == 1, action)
            assertTrue(Signature.getInstance("SHA256withECDSA").run {
                initVerify(working.public); update(request.transcript()); verify(phoneControlHex(wire.getString("signature")))
            })
            if (action == 48) PhoneControlReply.ControllerList(id, listOf(controller, otherController), "")
            else PhoneControlReply.SessionList(if (mismatched) "66".repeat(16) else request.controllerId, id,
                if (request.controllerId == controller || includeOther)
                    listOf(PhoneControlSessionSummary("55".repeat(16), "same", "mock", "/work", "ready", false))
                else emptyList(), "")
        }
        val client = PhoneControlClient(context, keys, transport)
        assertTrue(client.listControllers(period, 1) { deliver } is PhoneControlResult.Value)
        assertTrue(client.listSessions(period, 1, controller) { deliver } is PhoneControlResult.Value)
        val browser = PhoneControlBrowser(client)
        val scoped = (browser.page(period, null, true) { deliver } as PhoneControlResult.Value).value
        assertEquals(listOf(PhoneControlTarget(controller, "55".repeat(16))), scoped.rows.map { it.target })
        includeOther = true
        val both = (browser.page(period, null, true) { deliver } as PhoneControlResult.Value).value
        assertEquals(listOf(controller, otherController), both.rows.map { it.target.controllerId })
        assertEquals(both.rows.first().target.subjectId, both.rows.last().target.subjectId)
        mismatched = true
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.MALFORMED), client.listSessions(period, 1, controller) { deliver })
        deliver = false
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.UNAVAILABLE), client.listControllers(period, 1) { deliver })
        period.lock()
        assertEquals(PhoneControlResult.Failure(PhoneControlOutcome.EXPIRED), client.listControllers(period, 1) { true })
    }
}
