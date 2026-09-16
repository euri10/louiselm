package dev.louiselm.capture

/** Parse the closed, content-free wake-up protocol before any effects. */
internal fun attentionWakeGeneration(data: Map<String, String>): Long? {
    if (data.keys != setOf("generation")) return null
    val value = data["generation"] ?: return null
    if (!value.matches(Regex("[1-9][0-9]{0,18}"))) return null
    return value.toLongOrNull()
}
