package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Test

class AttentionWakeTest {
    @Test
    fun onlyCanonicalPositiveGenerationsAreAccepted() {
        assertEquals(42L, attentionWakeGeneration(mapOf("generation" to "42")))
        assertEquals(Long.MAX_VALUE, attentionWakeGeneration(mapOf("generation" to Long.MAX_VALUE.toString())))
        for (value in listOf("", "0", "-1", "+1", "01", " 1", "1.0", "9223372036854775808", "1\n")) {
            assertNull(value, attentionWakeGeneration(mapOf("generation" to value)))
        }
    }

    @Test
    fun remoteTextAndUnknownFieldsCannotBecomeNotifications() {
        assertNull(attentionWakeGeneration(emptyMap()))
        assertNull(attentionWakeGeneration(mapOf("generation" to "1", "title" to "remote text")))
        assertNull(attentionWakeGeneration(mapOf("generation" to "1", "run" to "private-work")))
    }
}
