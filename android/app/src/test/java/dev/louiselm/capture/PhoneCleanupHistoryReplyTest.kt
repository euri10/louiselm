package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneCleanupHistoryReplyTest {
    @Test
    fun removedDetailsAreExplicitAndCannotContainPhaseEvidence() {
        val id = "01".repeat(16)
        val record = """{"operation_id":"$id","receiver_identity":"${"02".repeat(32)}","controller_id":"${"03".repeat(16)}","subject_id":"${"04".repeat(16)}","action":3,"payload_hash":"${"05".repeat(32)}","grant_id":"${"06".repeat(16)}","grant_revision":1,"expected_revision":2,"recorded_at_ms":100,"updated_at_ms":101,"state":"details_removed","evidence":"","accepted_evidence":"","dispatched_evidence":"","completed_evidence":""}"""
        val page = """{"v":2,"type":"action_history_page","request_id":"${"07".repeat(16)}","records":[$record],"next_after":""}"""
        fun parse(value: String) = parsePhoneControlReply(value.toByteArray(Charsets.UTF_8))
        val reply = parse(page)
        assertTrue(reply is PhoneControlReply.ActionHistoryPage)
        val parsed = (reply as PhoneControlReply.ActionHistoryPage).records.single()
        assertEquals(id, parsed.operationId)
        assertEquals("DETAILS_REMOVED", parsed.state.name)
        for (field in listOf("evidence", "accepted_evidence", "dispatched_evidence", "completed_evidence")) {
            val invalid = page.replace("\"$field\":\"\"", "\"$field\":\"${"11".repeat(32)}\"")
            assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED), parse(invalid))
        }
        assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED), parse(page.replace("\"action\":3", "\"action\":4")))
    }
}
