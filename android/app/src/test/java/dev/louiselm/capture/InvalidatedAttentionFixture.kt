package dev.louiselm.capture

import org.robolectric.annotation.Implementation
import org.robolectric.annotation.Implements

// Producer contract: skills-core/tests/broker/posture.rs emits evidence_invalidated;
// capture-service/src/attention.rs serializes the private inbox item below.
// Synthetic identifiers; this is a structural fixture, not a physical-device capture.
internal val invalidatedAttentionPayload = """
    {"generation":7,"items":[{
      "subject_kind":"session","subject_id":"invalidated-session",
      "kind":"skill_unverified",
      "source_operation_id":"11111111-2222-4333-8444-555555555555",
      "created_at_ms":123,"eligible":true,
      "reason":"Skill supply is unverified",
      "linked_run_id":null,"stage":null,"code":"evidence_invalidated"
    }]}
""".trimIndent()

// Replace only credential storage and transport; Activity scheduling, parsing,
// cache and presentation remain real. No receiver or Keystore is accessed.
@Implements(PairingStore::class, isInAndroidSdk = false)
class AttentionFixturePairing {
    @Implementation
    fun binding(): String = "fixture-pair"

    @Implementation(methodName = "load")
    internal fun load(): PairingConfig = PairingConfig("https://192.0.2.1", "aa".repeat(32), "fixture", "dummy")
}

@Implements(PinnedHttps::class, isInAndroidSdk = false)
class AttentionFixtureReceiver {
    @Implementation(methodName = "fetchAttention")
    internal fun fetchAttention(@Suppress("UNUSED_PARAMETER") config: PairingConfig): AttentionFetch =
        parseAttentionResponse(invalidatedAttentionPayload)
}
