package dev.louiselm.capture

import java.security.Signature
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

class PhoneControlPeriodTest {
    @Test
    fun signedReadsUseTheTemporaryKeyUntilLockOrExpiry() {
        val key = phoneControlWorkingKey()
        val grant = PhoneControlGrant("11".repeat(16), 2, "22".repeat(32), "33".repeat(32), "44".repeat(32))
        val period = PhoneControlPeriod(grant, "55".repeat(16), key.private, 1000)
        val request = PhoneControlRequest(grant.grantId, 2, "55".repeat(16), grant.receiverIdentity,
            "66".repeat(16), "77".repeat(16), phoneControlReadPayload(0, 20), "88".repeat(16), 0, true)
        val signature = requireNotNull(period.sign(request, 1001))
        assertTrue(Signature.getInstance("SHA256withECDSA").run {
            initVerify(key.public)
            update(request.transcript())
            verify(signature)
        })
        assertNotNull(period.sign(request, 300_999))
        assertNull(period.sign(request, 301_000))
        assertNull(period.sign(request, 1002)) // Expiry irreversibly discards the key.
    }

    @Test
    fun onlyConfirmedInteractionCanRefreshIdleAndNeverTheAbsoluteLimit() {
        val key = phoneControlWorkingKey()
        val grant = PhoneControlGrant("11".repeat(16), 1, "22".repeat(32), "33".repeat(32), "44".repeat(32))
        val period = PhoneControlPeriod(grant, "55".repeat(16), key.private, 0)
        repeat(13) { index -> assertTrue(period.confirmInteraction(index * 299_999L)) }
        assertTrue(period.isActive(3_599_999))
        assertEquals(false, period.confirmInteraction(3_600_000))
        assertEquals(false, period.isActive(1))
        val locked = PhoneControlPeriod(grant, "55".repeat(16), key.private, 0)
        locked.lock()
        assertEquals(false, locked.isActive(1))
    }
}
