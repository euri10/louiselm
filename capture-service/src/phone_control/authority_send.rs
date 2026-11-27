//! Current authority for a phone prompt and the native owner's exact scope.

use std::{path::Path, time::Instant};

use super::{
    Authority, History, Proof, ReadPermit, Refusal, Request, elapsed_now, key, refusal,
    validate_proof_shape, verify_proof,
};
use crate::{PairingRegistry, phone_control::ActionIdentity};

impl Authority {
    pub(in crate::phone_control) fn finish_send_delivery(
        &self,
        permit: &ReadPermit,
        pairing: &PairingRegistry,
        credential: &str,
        admitted: bool,
        controller_attached: impl FnOnce() -> bool,
    ) -> Result<(), Refusal> {
        let mut permit = permit.clone();
        permit.interaction &= admitted;
        self.finish_read_delivery(&permit, true, pairing, credential, controller_attached)
    }

    pub(in crate::phone_control) fn validate_unlock(
        &self,
        device: &str,
        proof: &Proof,
    ) -> Result<bool, Refusal> {
        let now = Instant::now();
        validate_proof_shape(proof).map_err(refusal)?;
        let state = self.lock()?;
        let grant = state.grants.get(device).ok_or(Refusal::Unauthenticated)?;
        grant.require_approved_scope()?;
        if grant.grant_id != proof.grant_id
            || grant.grant_revision != proof.grant_revision
            || grant.receiver_identity != self.receiver_identity
            || proof.receiver_identity != self.receiver_identity
        {
            return Err(Refusal::Unauthenticated);
        }
        let pending = state
            .challenges
            .get(device)
            .ok_or(Refusal::Unauthenticated)?;
        if elapsed_now(now, now)? >= pending.deadline {
            return Err(Refusal::Expired);
        }
        if pending.challenge.challenge_id != proof.challenge_id
            || pending.challenge.challenge != proof.challenge
        {
            return Err(Refusal::Unauthenticated);
        }
        let enrolled = key(&grant.enrolled_key).map_err(|_| Refusal::Unavailable)?;
        verify_proof(proof, &enrolled).map_err(refusal)?;
        if elapsed_now(now, now)? >= pending.deadline {
            return Err(Refusal::Expired);
        }
        // Even an expired period may have a gate winner still completing.
        Ok(state.periods.contains_key(device))
    }

    pub(in crate::phone_control) fn send_capable(
        &self,
        device: &str,
        grant_id: &str,
        grant_revision: u64,
        period_id: &str,
        pairing: &PairingRegistry,
        credential: &str,
    ) -> Result<bool, Refusal> {
        let now = Instant::now();
        let mut state = self.lock()?;
        let result = pairing.with_authenticated_device(credential, |paired| {
            if paired != Some(device) {
                return Err(Refusal::Revoked);
            }
            let grant = state.grants.get(device).ok_or(Refusal::Revoked)?;
            grant.require_approved_scope()?;
            if grant.grant_id != grant_id
                || grant.grant_revision != grant_revision
                || grant.receiver_identity != self.receiver_identity
            {
                return Err(Refusal::Revoked);
            }
            let allowed = grant
                .scope
                .groups
                .iter()
                .any(|group| group == "operate_sessions");
            let lease = state.periods.get_mut(device).ok_or(Refusal::Revoked)?;
            if lease.public.period_id != period_id {
                return Err(Refusal::Revoked);
            }
            let checked = elapsed_now(now, now)?;
            if checked >= lease.idle_deadline || checked >= lease.absolute_deadline {
                return Err(Refusal::Expired);
            }
            lease.sleep.check(&self.sleep_now()?)?;
            Ok(allowed)
        });
        result.map_err(|_| Refusal::Unavailable)?
    }

    pub(in crate::phone_control) fn confirm_history(
        &self,
        device: &str,
        request: &Request,
        history: &History,
        pairing: &PairingRegistry,
        credential: &str,
    ) -> Result<(), Refusal> {
        let now = Instant::now();
        let mut state = self.lock()?;
        // The initial signed history lookup already consumed this operation.
        let result = pairing.with_authenticated_device(credential, |paired| {
            if paired != Some(device) {
                return Err(Refusal::Revoked);
            }
            if request.action != 2 || !state.operations.contains(&request.operation_id) {
                return Err(Refusal::Unauthenticated);
            }
            let grant = state.grants.get(device).cloned().ok_or(Refusal::Revoked)?;
            grant.require_approved_scope()?;
            if grant.grant_id != request.grant_id
                || grant.grant_revision != request.grant_revision
                || grant.receiver_identity != self.receiver_identity
            {
                return Err(Refusal::Revoked);
            }
            if !grant.scope.groups.iter().any(|group| group == "read") {
                return Err(Refusal::OutOfScope);
            }
            let records = match history {
                History::Record(record) => {
                    if record.identity.controller_id != request.controller_id
                        || record.identity.subject_id != request.subject_id
                    {
                        return Err(Refusal::StaleSubject);
                    }
                    std::slice::from_ref(record.as_ref())
                }
                History::Page { records, .. } => records.as_slice(),
            };
            if records.iter().any(|record| {
                record.identity.receiver_identity != self.receiver_identity
                    || !grant.scope.agents.contains(&record.identity.agent)
                    || !grant
                        .scope
                        .projects
                        .iter()
                        .any(|root| Path::new(&record.identity.project).starts_with(root))
            }) {
                return Err(Refusal::OutOfScope);
            }
            let lease = state.periods.get_mut(device).ok_or(Refusal::Revoked)?;
            if lease.public.period_id != request.period_id {
                return Err(Refusal::Revoked);
            }
            let checked = elapsed_now(now, now)?;
            if checked >= lease.idle_deadline || checked >= lease.absolute_deadline {
                return Err(Refusal::Expired);
            }
            lease.sleep.check(&self.sleep_now()?)
        });
        result.map_err(|_| Refusal::Unavailable)?
    }

    pub(in crate::phone_control) fn authorize_send(
        &self,
        permit: &ReadPermit,
        pairing: &PairingRegistry,
        credential: &str,
        project: &Path,
        agent: &str,
    ) -> Result<(ActionIdentity, u64), Refusal> {
        let now = Instant::now();
        let mut state = self.lock()?;
        // Cutoff takes the generation barrier first; do not acquire it here.
        let result = pairing.with_authenticated_device(credential, |device| {
            if device != Some(permit.device.as_str()) {
                return Err(Refusal::Revoked);
            }
            let (frame, grant) = self.check_voice_request(&mut state, permit, now, now)?;
            if frame.request.action != 3 {
                return Err(Refusal::Unsupported);
            }
            self.check_owner_scope(&grant, project, agent)?;
            if std::fs::canonicalize(project).map_err(|_| Refusal::Unavailable)? != project {
                return Err(Refusal::Unavailable);
            }
            self.check_pending_read_locked(&mut state, permit, Instant::now(), Instant::now())?;
            let lease = state
                .periods
                .get_mut(&permit.device)
                .ok_or(Refusal::Revoked)?;
            let remaining = lease
                .idle_deadline
                .min(lease.absolute_deadline)
                .saturating_duration_since(Instant::now())
                .as_millis();
            let remaining = u64::try_from(remaining)
                .map_err(|_| Refusal::Unavailable)?
                .min(lease.sleep.remaining_ms(&self.sleep_now()?)?)
                .min(5_000);
            if remaining == 0 {
                return Err(Refusal::Expired);
            }
            let request = frame.request;
            let identity = ActionIdentity {
                operation_id: request.operation_id,
                receiver_identity: request.receiver_identity,
                controller_id: request.controller_id,
                subject_id: request.subject_id,
                action: 3,
                payload_hash: request.payload_hash,
                grant_id: request.grant_id,
                grant_revision: request.grant_revision,
                period_id: request.period_id,
                expected_revision: request.expected_revision,
                project: project.to_str().ok_or(Refusal::Malformed)?.to_owned(),
                agent: agent.to_owned(),
                recorded_at_ms: crate::time::now_ms(),
            };
            Ok((identity, remaining))
        });
        result.map_err(|_| Refusal::Unavailable)?
    }
}
