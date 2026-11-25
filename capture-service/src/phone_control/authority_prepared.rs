//! Owner-authorized, scoped prepared-Run read delivery.

use std::time::Instant;

use sha2::{Digest, Sha256};

use super::super::{
    Frame, Request, fixed,
    prepared_owner::{OwnerPreparedError, PreparedReader},
    prepared_selection::project_detail,
    source::PhoneSourceError,
    verify_phone_control_frame,
};
use super::{Authority, ReadPermit, Refusal, State, StoredGrant, refusal};
use crate::{PairingRegistry, RunProjectBinding};

#[derive(Clone, Copy)]
struct ReadTiming {
    now: Instant,
    wall_ms: u64,
    started: Instant,
}

impl Authority {
    pub(in crate::phone_control) fn finish_prepared_delivery(
        &self,
        permit: &ReadPermit,
        pairing: &PairingRegistry,
        credential: &str,
        reader: Option<&PreparedReader>,
    ) -> Result<serde_json::Value, Refusal> {
        let now = Instant::now();
        let wall_ms = crate::time::now_ms();
        let started = Instant::now();
        let mut state = self.lock()?;
        let result = pairing.with_authenticated_device(credential, |device| {
            if device != Some(permit.device.as_str()) {
                return Err(Refusal::Revoked);
            }
            if Sha256::digest(&permit.frame).as_slice() != permit.frame_sha256 {
                return Err(Refusal::Unauthenticated);
            }
            verify_phone_control_frame(&permit.frame, &self.receiver_identity).map_err(refusal)?;
            let frame: Frame =
                serde_json::from_slice(&permit.frame).map_err(|_| Refusal::Malformed)?;
            let request = &frame.request;
            if !matches!(request.action, 52 | 53)
                || request.operation_id != permit.operation_id
                || request.grant_id != permit.grant_id
                || request.period_id != permit.period_id
            {
                return Err(Refusal::Unauthenticated);
            }
            let grant = state
                .grants
                .get(&permit.device)
                .cloned()
                .ok_or(Refusal::Revoked)?;
            if grant.grant_id != permit.grant_id
                || grant.grant_revision != request.grant_revision
                || grant.enrolled_key != frame.grant.enrolled_key
                || grant.receiver_identity != self.receiver_identity
            {
                return Err(Refusal::Revoked);
            }
            if !grant.scope.groups.iter().any(|group| group == "read") {
                return Err(Refusal::OutOfScope);
            }
            let reader = reader.ok_or(Refusal::Unsupported)?;
            let timing = ReadTiming {
                now,
                wall_ms,
                started,
            };
            if request.action == 52 {
                self.prepared_page(&mut state, permit, request, reader, &grant, timing)
            } else {
                self.prepared_detail(&mut state, permit, request, reader, &grant, timing)
            }
        });
        if !matches!(&result, Ok(Ok(_))) {
            state.pending_reads.remove(&permit.operation_id);
        }
        result.map_err(|_| Refusal::Unavailable)?
    }

    fn prepared_in_scope(
        &self,
        grant: &StoredGrant,
        binding: &RunProjectBinding,
    ) -> Result<bool, Refusal> {
        if !binding
            .agents
            .iter()
            .all(|agent| grant.scope.agents.contains(agent))
        {
            return Ok(false);
        }
        let projects = self.projects.as_ref().map_err(|_| Refusal::Unavailable)?;
        if let Some(roots) = grant.project_roots()? {
            projects
                .matches_scope(binding, roots, &grant.scope.agents)
                .map_err(|_| Refusal::Unavailable)
        } else {
            Ok(grant.legacy_project_confirmed(&binding.project_id)?
                && projects
                    .matches_current_parent(&binding.project_id, &grant.scope.projects)
                    .map_err(|_| Refusal::Unavailable)?)
        }
    }

    fn prepared_detail(
        &self,
        state: &mut State,
        permit: &ReadPermit,
        request: &Request,
        reader: &PreparedReader,
        grant: &StoredGrant,
        timing: ReadTiming,
    ) -> Result<serde_json::Value, Refusal> {
        reader.with_published(&request.subject_id, request.expected_revision, |saved, owner| {
            if !self.prepared_in_scope(grant, &saved.binding)? {
                return Err(Refusal::OutOfScope);
            }
            let detail = project_detail(saved, owner).map_err(|_| Refusal::Unavailable)?;
            self.finish_read_locked(state, permit, true, timing.now, timing.wall_ms, timing.started)?;
            Ok(serde_json::json!({"v":1,"type":"prepared_detail","request_id":request.operation_id,
                "registration_id":request.subject_id,"run_id":saved.binding.run_id,
                "revision":saved.revision,"detail":detail}))
        }).map_err(|error| prepared_read_refusal(&error))?
    }

    fn prepared_page(
        &self,
        state: &mut State,
        permit: &ReadPermit,
        request: &Request,
        reader: &PreparedReader,
        grant: &StoredGrant,
        timing: ReadTiming,
    ) -> Result<serde_json::Value, Refusal> {
        let after = if fixed::<16>(&request.payload).map_err(refusal)? == [0; 16] {
            ""
        } else {
            &request.payload
        };
        for (summary, binding) in reader
            .candidates()
            .map_err(|error| prepared_read_refusal(&error))?
        {
            if summary.id.as_str() <= after || !self.prepared_in_scope(grant, &binding)? {
                continue;
            }
            let result = reader.with_published(&summary.id, summary.revision, |saved, owner| {
                if saved.binding != binding || !self.prepared_in_scope(grant, &saved.binding)? {
                    return Err(Refusal::OutOfScope);
                }
                project_detail(saved, owner).map_err(|_| Refusal::Unavailable)?;
                self.finish_read_locked(state, permit, true, timing.now, timing.wall_ms, timing.started)?;
                Ok(serde_json::json!({"v":1,"type":"prepared_page","request_id":request.operation_id,
                    "items":[{"registration_id":summary.id,"run_id":saved.binding.run_id,"revision":saved.revision,"state":"prepared"}],
                    "next_after":summary.id}))
            }).map_err(|error| prepared_read_refusal(&error));
            match result {
                Ok(Ok(page)) => return Ok(page),
                Err(Refusal::StaleSubject) | Ok(Err(Refusal::OutOfScope)) => {}
                Err(error) | Ok(Err(error)) => return Err(error),
            }
        }
        self.finish_read_locked(
            state,
            permit,
            true,
            timing.now,
            timing.wall_ms,
            timing.started,
        )?;
        Ok(
            serde_json::json!({"v":1,"type":"prepared_page","request_id":request.operation_id,"items":[],"next_after":""}),
        )
    }
}

fn prepared_read_refusal(error: &OwnerPreparedError) -> Refusal {
    match error {
        OwnerPreparedError::Source(PhoneSourceError::Unsupported) => Refusal::Unsupported,
        OwnerPreparedError::Run(crate::RunStoreError::Invalid(_))
        | OwnerPreparedError::Prepared(crate::PreparedRunError::Invalid(_)) => {
            Refusal::StaleSubject
        }
        _ => Refusal::Unavailable,
    }
}
