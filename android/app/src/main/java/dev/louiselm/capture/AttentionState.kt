package dev.louiselm.capture

import android.annotation.SuppressLint
import android.content.Context
import android.content.SharedPreferences
import org.json.JSONArray
import org.json.JSONException
import org.json.JSONObject
import java.io.IOException

/** Firebase Installations uses 22 URL-safe base64 characters; reject tokens and malformed IDs. */
internal fun validAttentionInstallation(fid: String): Boolean =
    fid.length == 22 && fid.all { it in 'A'..'Z' || it in 'a'..'z' || it in '0'..'9' || it == '_' || it == '-' }

/** Exact authenticated receiver wire body; no callback metadata or inbox content. */
internal fun attentionInstallationBody(fid: String): ByteArray {
    require(validAttentionInstallation(fid)) { "invalid notification installation ID" }
    return JSONObject().put("fid", fid).toString().toByteArray(Charsets.UTF_8)
}

/** App-private, pairing-scoped push state. Call on a background thread.
 * Shared preferences serialize short state transitions; no network or presentation runs under its lock.
 * A failed durable write throws IOException and must not be acknowledged as delivered/seen.
 */
internal class AttentionState(
    context: Context,
    private val binding: () -> String? = PairingStore(context)::binding,
) {
    private val preferences = context.applicationContext.getSharedPreferences("attention-push", Context.MODE_PRIVATE)

    private fun currentBinding(): String? {
        val current = binding() ?: return null
        if (preferences.getString("owner", null) != current) {
            val enabled = preferences.getBoolean("enabled", false)
            persist(preferences.edit().clear().putString("owner", current).putBoolean("enabled", enabled))
        }
        return current
    }

    fun owner(): String? = synchronized(preferences) {
        currentBinding()?.takeIf { preferences.getBoolean("enabled", false) && !preferences.getBoolean("blocked", false) }
    }

    fun blocked(expected: String): Boolean = synchronized(preferences) {
        currentBinding() == expected && preferences.getBoolean("blocked", false)
    }

    fun enable(): Boolean = synchronized(preferences) {
        if (binding() == null) return false
        owner()
        // Revocation requires repairing the pairing, not repeatedly enabling the old credential.
        if (preferences.getBoolean("blocked", false)) return false
        persist(preferences.edit().putBoolean("enabled", true))
        true
    }

    fun block(expected: String): Boolean = synchronized(preferences) {
        if (currentBinding() != expected) return false
        persist(preferences.edit().putBoolean("blocked", true))
        true
    }
    fun installationPrepared(expected: String): Boolean = update(expected) { putBoolean("installation_prepared", true) }
    fun needsInstallationReset(expected: String): Boolean = synchronized(preferences) {
        owner() == expected && !preferences.getBoolean("installation_prepared", false)
    }

    fun registered(expected: String): Boolean = update(expected) { putBoolean("installation_registered", true) }
    fun readyOwner(): String? = synchronized(preferences) {
        owner()?.takeIf { preferences.getBoolean("installation_registered", false) }
    }

    fun seen(expected: String, generation: Long): Boolean = synchronized(preferences) {
        if (currentBinding() != expected) return false
        persist(preferences.edit().putLong("seen", maxOf(generation, preferences.getLong("seen", 0))))
        true
    }

    fun deliver(expected: String, generation: Long, show: () -> Boolean): Boolean {
        val previous: Long
        synchronized(preferences) {
            if (owner() != expected || generation <= preferences.getLong("seen", 0)) return false
            previous = preferences.getLong("notified", 0)
            if (generation <= previous) return false
            persist(preferences.edit().putLong("notified", generation))
        }
        // Reserve first to reject concurrent duplicates, but never call Android presentation under a lock.
        val delivered = owner() == expected && show()
        if (!delivered) synchronized(preferences) {
            if (owner() == expected && preferences.getLong("notified", 0) == generation) {
                persist(preferences.edit().putLong("notified", previous))
            }
        }
        return delivered
    }

    fun cache(expected: String, snapshot: AttentionSnapshot): Boolean = synchronized(preferences) {
        if (currentBinding() != expected || blocked(expected)) return false
        if (snapshot.generation < preferences.getLong("cached_generation", 0)) return false
        val payload = snapshot.encode()
        AttentionSnapshot.parse(payload)
        persist(preferences.edit().putString("snapshot", payload).putLong("cached_generation", snapshot.generation))
        true
    }

    fun cached(expected: String): AttentionSnapshot? = synchronized(preferences) {
        if (currentBinding() != expected || blocked(expected)) return null
        val payload = preferences.getString("snapshot", null) ?: return null
        try {
            AttentionSnapshot.parse(payload)
        } catch (error: JSONException) {
            throw IOException("stored Attention inbox is invalid", error)
        } catch (error: IllegalArgumentException) {
            throw IOException("stored Attention inbox is invalid", error)
        }
    }

    private fun update(expected: String, change: SharedPreferences.Editor.() -> Unit): Boolean = synchronized(preferences) {
        if (owner() != expected) return false
        persist(preferences.edit().apply(change))
        true
    }

    @SuppressLint("UseKtx") // Durable generation and ownership transitions require the commit Boolean.
    private fun persist(editor: SharedPreferences.Editor) {
        if (!editor.commit()) throw IOException("notification state could not be persisted")
    }
}

private fun AttentionSnapshot.encode(): String = JSONObject()
    .put("generation", generation)
    .put("items", JSONArray(items.map { item ->
        JSONObject()
            .put("subject_kind", item.subjectKind.name.lowercase())
            .put("subject_id", item.subjectId)
            .put("kind", item.kind.name.lowercase())
            .put("source_operation_id", item.sourceOperationId)
            .put("created_at_ms", item.createdAtMs)
            .put("eligible", item.eligible)
            .put("reason", item.kind.reason)
            .put("linked_run_id", item.linkedRunId)
            .put("stage", item.stage)
            .put("code", item.code?.name?.lowercase())
    }))
    .toString()
