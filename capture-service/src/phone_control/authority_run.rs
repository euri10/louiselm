//! Current scoped durable Run projections; this is not live-controller authority.

use serde_json::{Value, json};

use super::super::{
    MAX_INTEGER, Request, fixed,
    prepared_owner::{OwnerPreparedError, OwnerRunReader},
    source::PhoneSourceError,
};
use super::{Authority, OwnerReadTiming, ReadPermit, Refusal, State, StoredGrant, refusal};
use crate::RunView;

impl Authority {
    pub(super) fn run_detail(
        &self,
        state: &mut State,
        permit: &ReadPermit,
        request: &Request,
        reader: &OwnerRunReader,
        grant: &StoredGrant,
        timing: OwnerReadTiming,
    ) -> Result<Value, Refusal> {
        let id =
            uuid::Uuid::from_bytes(fixed::<16>(&request.subject_id).map_err(refusal)?).to_string();
        let candidate = reader
            .run_binding(&id)
            .map_err(|error| run_refusal(&error))?
            .ok_or(Refusal::StaleSubject)?;
        if !self.binding_in_scope(grant, &candidate)? {
            return Err(Refusal::OutOfScope);
        }
        reader.with_bound_run(&id, request.expected_revision, |binding, view| {
            if binding != &candidate { return Err(Refusal::StaleSubject); }
            if !self.binding_in_scope(grant, binding)? { return Err(Refusal::OutOfScope); }
            let projection = project(view, timing.wall_ms)?;
            self.finish_read_locked(state, permit, true, timing.now, timing.wall_ms, timing.started)?;
            Ok(json!({"v":1,"type":"run_detail","request_id":request.operation_id,"run":projection}))
        }).map_err(|error| run_refusal(&error))?
    }

    pub(super) fn run_page(
        &self,
        state: &mut State,
        permit: &ReadPermit,
        request: &Request,
        reader: &OwnerRunReader,
        grant: &StoredGrant,
        timing: OwnerReadTiming,
    ) -> Result<Value, Refusal> {
        let cursor = fixed::<17>(&request.payload).map_err(refusal)?;
        let after = if cursor[0] == 0 {
            None
        } else {
            let mut bytes = [0; 16];
            bytes.copy_from_slice(&cursor[1..]);
            Some(uuid::Uuid::from_bytes(bytes).to_string())
        };
        for id in reader.run_ids().map_err(|error| run_refusal(&error))? {
            if after.as_ref().is_some_and(|after| id <= *after) {
                continue;
            }
            let Some(candidate) = reader
                .run_binding(&id)
                .map_err(|error| run_refusal(&error))?
            else {
                continue;
            };
            if !self.binding_in_scope(grant, &candidate)? {
                continue;
            }
            let result = reader.with_bound_run(&id, 0, |binding, view| {
                if binding != &candidate { return Err(Refusal::StaleSubject); }
                if !self.binding_in_scope(grant, binding)? { return Err(Refusal::OutOfScope); }
                project(view, timing.wall_ms)?;
                self.finish_read_locked(state, permit, true, timing.now, timing.wall_ms, timing.started)?;
                Ok(json!({"v":1,"type":"run_page","request_id":request.operation_id,
                    "items":[{"run_id":view.id,"revision":view.revision,"state":view.state}],"next_after":view.id}))
            }).map_err(|error| run_refusal(&error));
            match result {
                Ok(Ok(page)) => return Ok(page),
                Err(Refusal::StaleSubject)
                | Ok(Err(Refusal::StaleSubject | Refusal::OutOfScope)) => {}
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
            json!({"v":1,"type":"run_page","request_id":request.operation_id,"items":[],"next_after":""}),
        )
    }
}

fn project(view: &RunView, wall_ms: u64) -> Result<Value, Refusal> {
    if !matches!(
        view.state.as_str(),
        "admitted" | "active" | "parked" | "cold_parked" | "resuming" | "disposed"
    ) || view.revision == 0
        || view.revision > MAX_INTEGER
        || view.generated_work_ceiling == 0
        || view.generated_work_ceiling > MAX_INTEGER
        || view.generated_work_consumed > MAX_INTEGER
        || view.generated_work_reserved > MAX_INTEGER
        || view.park_expires_at_ms > MAX_INTEGER
        || view
            .resume_deadline_ms
            .is_some_and(|value| value > MAX_INTEGER)
        || view.session_id.is_some() != view.load_session.is_some()
        || (view.state == "resuming"
            && (view.resume_operation_id.is_none() || view.resume_deadline_ms.is_none()))
    {
        return Err(Refusal::Unavailable);
    }
    let used = view
        .generated_work_consumed
        .checked_add(view.generated_work_reserved)
        .ok_or(Refusal::Unavailable)?;
    let remaining = view
        .generated_work_ceiling
        .checked_sub(used)
        .ok_or(Refusal::Unavailable)?;
    let pending =
        u64::try_from(view.pending_mutation_ids.len()).map_err(|_| Refusal::Unavailable)?;
    if pending > MAX_INTEGER {
        return Err(Refusal::Unavailable);
    }
    Ok(
        json!({"run_id":view.id,"revision":view.revision,"state":view.state,
        "freshness":"durable_current","controller":"not_checked","has_session":view.session_id.is_some(),
        "load_session":view.load_session,"work":{"ceiling":view.generated_work_ceiling,
            "consumed":view.generated_work_consumed,"reserved":view.generated_work_reserved,"remaining":remaining},
        "pending_mutations":pending,"park_expires_at_ms":view.park_expires_at_ms,
        "park_expired":matches!(view.state.as_str(), "parked" | "cold_parked")
            && view.park_expires_at_ms != 0 && wall_ms >= view.park_expires_at_ms,
        "resume_pending":view.state == "resuming",
        "resume_deadline_ms":if view.state == "resuming" { view.resume_deadline_ms } else { None }}),
    )
}

fn run_refusal(error: &OwnerPreparedError) -> Refusal {
    match error {
        OwnerPreparedError::Source(PhoneSourceError::Unsupported) => Refusal::Unsupported,
        OwnerPreparedError::Run(crate::RunStoreError::Invalid(_)) => Refusal::StaleSubject,
        _ => Refusal::Unavailable,
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "test fixtures and assertions"
    )]
    use super::project;
    use crate::RunView;

    fn view(state: &str) -> RunView {
        RunView {
            id: "11111111-1111-4111-8111-111111111111".into(),
            revision: 1,
            state: state.into(),
            session_id: Some("codex/acp".into()),
            load_session: Some(false),
            generated_work_ceiling: 5,
            generated_work_consumed: 2,
            generated_work_reserved: 1,
            pending_mutation_ids: vec!["private-id".into()],
            triggering_mutation_id: None,
            park_expires_at_ms: 2000,
            resume_operation_id: None,
            resume_deadline_ms: None,
        }
    }

    #[test]
    fn projection_reports_durable_lifecycle_without_promising_recovery_or_live_controller() {
        for state in ["admitted", "active", "parked", "cold_parked", "disposed"] {
            let projection = project(&view(state), 2500).unwrap();
            assert_eq!(projection["state"], state);
            assert_eq!(projection["controller"], "not_checked");
            assert_eq!(projection["freshness"], "durable_current");
            assert_eq!(projection["load_session"], false);
            assert_eq!(projection["work"]["remaining"], 2);
            assert_eq!(
                projection["park_expired"],
                state == "parked" || state == "cold_parked"
            );
            assert!(!projection.to_string().contains("private-id"));
            assert!(!projection.to_string().contains("codex/acp"));
        }
        let mut resuming = view("resuming");
        assert!(project(&resuming, 1000).is_err());
        resuming.resume_operation_id = Some("operation".into());
        resuming.resume_deadline_ms = Some(3000);
        assert_eq!(project(&resuming, 1000).unwrap()["resume_pending"], true);
        resuming.state = "cold_parked".into();
        assert_eq!(project(&resuming, 1000).unwrap()["resume_pending"], false);
        assert_eq!(
            project(&resuming, 1000).unwrap()["resume_deadline_ms"],
            serde_json::Value::Null
        );
        resuming.generated_work_reserved = 9;
        assert!(project(&resuming, 1000).is_err());
    }
}
