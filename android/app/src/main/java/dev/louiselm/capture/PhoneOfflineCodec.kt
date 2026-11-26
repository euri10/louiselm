package dev.louiselm.capture

import org.json.JSONArray
import org.json.JSONObject

/** Encode a closed owner page without live settings, revalidating the network bounds. */
internal fun encodePhoneOfflinePage(page: PhoneControlReply.SessionPage): ByteArray {
    val entries = JSONArray()
    for (entry in page.entries) {
        val item = JSONObject().put("seq", entry.seq).put("truncated", entry.truncated)
        when (entry) {
            is PhoneControlEntry.Message -> item.put("kind", "message").put("role", entry.role).put("text", entry.text)
            is PhoneControlEntry.Tool -> item.put("kind", "tool").put("id", entry.id).put("title", entry.title)
                .put("status", entry.status).put("content", entry.content)
            is PhoneControlEntry.Question -> {
                val options = JSONArray()
                for (option in entry.options) options.put(JSONObject().put("label", option.label)
                    .put("description", option.description).put("preview", option.preview))
                item.put("kind", "question").put("request_id", entry.requestId).put("question", entry.question)
                    .put("options", options).put("multi_select", entry.multiSelect)
            }
            is PhoneControlEntry.Plan -> item.put("kind", "plan").put("request_id", entry.requestId).put("text", entry.text)
        }
        entries.put(item)
    }
    val source = page.source
    val json = JSONObject().put("v", 2).put("type", "session_page")
        .put("controller_id", page.controllerId).put("subject_id", page.subjectId).put("operation_id", page.operationId)
        .put("revision", page.revision).put("status", page.status).put("entries", entries)
        .put("next_before", page.nextBefore).put("complete", page.complete)
        .put("source", JSONObject().put("replay", source.replay).put("archive", source.archive)
            .put("redaction", source.redaction).put("missing", source.missing).put("truncated", source.truncated))
    val encoded = json.toString().toByteArray(Charsets.UTF_8)
    require(parsePhoneControlReply(encoded) == page.copy(settings = PhoneSessionSettings(), currentPermission = null)) { "invalid offline Session page" }
    return encoded
}
