package dev.louiselm.capture

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [30])
class PhoneControlHistoryReplyTest {
    private val operation = "01".repeat(16)
    private val record = """{"operation_id":"$operation","receiver_identity":"${"02".repeat(32)}","controller_id":"${"03".repeat(16)}","subject_id":"${"04".repeat(16)}","action":3,"payload_hash":"${"05".repeat(32)}","grant_id":"${"06".repeat(16)}","grant_revision":1,"expected_revision":2,"recorded_at_ms":100,"updated_at_ms":101,"state":"unknown","evidence":"","accepted_evidence":"","dispatched_evidence":"","completed_evidence":""}"""

    private fun parse(value: String) = parsePhoneControlReply(value.toByteArray(Charsets.UTF_8))

    @Test
    fun acceptsBoundedHistoryFromAnUnavailableController() {
        val page = """{"v":2,"type":"action_history_page","request_id":"${"07".repeat(16)}","records":[$record],"next_after":""}"""
        val result = parse(page)
        assertTrue(result is PhoneControlReply.ActionHistoryPage)
        result as PhoneControlReply.ActionHistoryPage
        assertEquals("07".repeat(16), result.requestId)
        assertEquals(operation, result.records.single().operationId)
        assertEquals(PhoneActionState.UNKNOWN, result.records.single().state)
        assertEquals("", result.nextAfter)
        val one = """{"v":2,"type":"action_history","request_id":"${"07".repeat(16)}","record":$record}"""
        assertFalse(parse(one) is PhoneControlReply.Failure)
        val accepted = one.replace("\"state\":\"unknown\",\"evidence\":\"\"",
            "\"state\":\"accepted\",\"evidence\":\"${"09".repeat(32)}\"")
            .replace("\"accepted_evidence\":\"\"", "\"accepted_evidence\":\"${"09".repeat(32)}\"")
        assertEquals(PhoneActionState.ACCEPTED, (parse(accepted) as PhoneControlReply.ActionHistory).record.state)
    }

    @Test
    fun rejectsMalformedEvidenceAndNonAdvancingHistoryPages() {
        val page = """{"v":2,"type":"action_history_page","request_id":"${"07".repeat(16)}","records":[$record],"next_after":""}"""
        for (bad in listOf(
            page.replace("\"state\":\"unknown\"", "\"state\":\"accepted\""),
            page.replace("\"action\":3", "\"action\":1"),
            page.replace("\"updated_at_ms\":101", "\"updated_at_ms\":99"),
            page.replace("\"grant_revision\":1", "\"grant_revision\":0"),
            page.replace("\"next_after\":\"\"", "\"next_after\":\"${"08".repeat(16)}\""),
            page.replace("\"records\":[$record]", "\"records\":[$record,$record]"),
            page.replace("\"state\":\"unknown\"", "\"state\":\"unknown\",\"extra\":true"),
        )) {
            assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED), parse(bad))
        }
    }

    @Test
    fun distinguishesRetainedOwnerPhasesAndRejectsContradictions() {
        val accepted = "11".repeat(32)
        val dispatched = "22".repeat(32)
        val completed = "33".repeat(32)
        val one = """{"v":2,"type":"action_history","request_id":"${"07".repeat(16)}","record":$record}"""
        val dispatchedReply = one.replace("\"state\":\"unknown\",\"evidence\":\"\"", "\"state\":\"dispatched\",\"evidence\":\"$dispatched\"")
            .replace("\"accepted_evidence\":\"\"", "\"accepted_evidence\":\"$accepted\"")
            .replace("\"dispatched_evidence\":\"\"", "\"dispatched_evidence\":\"$dispatched\"")
        val parsed = (parse(dispatchedReply) as PhoneControlReply.ActionHistory).record
        assertEquals(PhoneActionState.DISPATCHED, parsed.state)
        assertEquals(accepted, parsed.acceptedEvidence)
        assertEquals(dispatched, parsed.dispatchedEvidence)
        val unknownReply = dispatchedReply.replace("\"state\":\"dispatched\",\"evidence\":\"$dispatched\"", "\"state\":\"unknown\",\"evidence\":\"\"")
        val unknown = (parse(unknownReply) as PhoneControlReply.ActionHistory).record
        assertEquals(PhoneActionState.UNKNOWN, unknown.state)
        assertEquals(dispatched, unknown.dispatchedEvidence)
        val completeReply = dispatchedReply.replace("\"state\":\"dispatched\",\"evidence\":\"$dispatched\"", "\"state\":\"completed\",\"evidence\":\"$completed\"")
            .replace("\"completed_evidence\":\"\"", "\"completed_evidence\":\"$completed\"")
        assertEquals(PhoneActionState.COMPLETED, (parse(completeReply) as PhoneControlReply.ActionHistory).record.state)
        assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.MALFORMED), parse(completeReply.replace("\"completed_evidence\":\"$completed\"", "\"completed_evidence\":\"${"44".repeat(32)}\"")))
        assertEquals(PhoneControlReply.Failure(PhoneControlOutcome.UNSUPPORTED), parse(one.replace("\"v\":2", "\"v\":1")))
    }
}
