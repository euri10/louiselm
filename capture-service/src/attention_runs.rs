//! Reconcile only the deterministic Park conditions emitted by the local controller.

use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{
    AttentionError, AttentionKey, AttentionKind, AttentionStore, AttentionSubjectKind,
    PersistedAttention, advance_generation,
};
use crate::{runs::RunStoreError, time::now_ms};

pub(super) fn is_local_park(key: &AttentionKey) -> bool {
    if key.subject_kind != AttentionSubjectKind::Run || key.kind != AttentionKind::RunParked {
        return false;
    }
    // Wire identity used by ui/attention.lua:run_parked, not every broker Park
    // on the same Run. Keep this derivation covered by the captured-key fixture.
    let digest = Sha256::digest(format!("run_parked:{}", key.subject_id));
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    key.source_operation_id == Uuid::from_bytes(bytes).to_string()
}

impl AttentionStore {
    /// Reads only durable lifecycle facts; never returns Run credentials or policy.
    pub(crate) fn broker_run_lifecycle(
        &self,
        id: &str,
    ) -> Result<serde_json::Value, AttentionError> {
        let unavailable = || RunStoreError::Invalid("Run lifecycle unavailable".into());
        let runs = self.runs.as_ref().ok_or_else(unavailable)?;
        let run = runs.find_view(id)?.ok_or_else(unavailable)?;
        if !matches!(
            run.state.as_str(),
            "admitted" | "active" | "parked" | "cold_parked" | "resuming" | "disposed"
        ) {
            return Err(unavailable().into());
        }
        Ok(serde_json::json!({"run_id":run.id,"revision":run.revision,"state":run.state}))
    }

    pub(super) fn local_park_resolved(&self, key: &AttentionKey) -> Result<bool, AttentionError> {
        let Some(runs) = &self.runs else {
            return Ok(false);
        };
        if !is_local_park(key) {
            return Ok(false);
        }
        let Some(run) = runs.find_view(&key.subject_id)? else {
            // Absence is not authoritative resolution (e.g. another controller's Run).
            return Ok(false);
        };
        match run.state.as_str() {
            "parked" | "cold_parked" if run.park_expires_at_ms > 0 => {
                Ok(run.park_expires_at_ms <= now_ms())
            }
            // A pending resume may fail; keep its unresolved condition until finalization.
            "resuming" => Ok(false),
            "active" | "disposed" => Ok(true),
            _ => Err(RunStoreError::Invalid("stored Run lifecycle state is invalid".into()).into()),
        }
    }

    pub(super) fn reconcile_local_parks(
        &self,
        state: &mut PersistedAttention,
    ) -> Result<(), AttentionError> {
        // Called under the Attention lock. Run reads see atomic record replacements;
        // no Run lock is acquired, so Run writers never wait on an Attention reader.
        let previous = state.items.len();
        let mut retained = Vec::with_capacity(previous);
        for item in state.items.drain(..) {
            if !self.local_park_resolved(&item.key())? {
                retained.push(item);
            }
        }
        state.items = retained;
        if state.items.len() != previous {
            advance_generation(state)?;
            self.persist(state)?;
        }
        Ok(())
    }
}
