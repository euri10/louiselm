//! Native-owner messages used by the authenticated Send path.

use serde::Deserialize;
use serde_json::{Value, json};

use super::super::{
    ActionEvidence, ActionEvidenceError, ActionEvidenceStore, ActionIdentity, ActionOutcome,
    ActionReservation, Request,
    authority::{History, ReadPermit, Refusal},
    evidence::same_identity,
    fixed, owner_journal, personal_startup,
    voice_job::VoiceOwnerScope,
};
use super::{HttpState, history_response, voice_upload};

pub(super) async fn finish_send(
    state: HttpState,
    credential: String,
    permit: ReadPermit,
    request: Request,
) -> Result<Value, Refusal> {
    let result = dispatch_send(&state, &credential, &permit, &request).await;
    let authority = state.authority.clone();
    let cleanup = tokio::task::spawn_blocking(move || authority.discard_voice_permit(&permit))
        .await
        .map_err(|_| Refusal::Unavailable)?;
    cleanup?;
    result
}

async fn dispatch_send(
    state: &HttpState,
    credential: &str,
    permit: &ReadPermit,
    request: &Request,
) -> Result<Value, Refusal> {
    let (identity, reservation) = reserve_for_send(state, credential, permit, request).await?;
    if reservation == ActionReservation::Existing {
        return send_receipt(state, credential, permit, &identity, None).await;
    }
    // The reservation is durable before any prompt-bearing frame leaves capture.
    preflight_send(state, credential, permit, &identity).await?;
    let outcome = if let Some(fenced) = prepare_owner(state, permit, request).await? {
        fenced
    } else {
        commit_owner(state, credential, permit, request, &identity).await?
    };
    send_receipt(state, credential, permit, &identity, Some(outcome)).await
}

async fn reserve_for_send(
    state: &HttpState,
    credential: &str,
    permit: &ReadPermit,
    request: &Request,
) -> Result<(ActionIdentity, ActionReservation), Refusal> {
    let scope = voice_upload::owner_scope(&state.controllers, request).await?;
    let query_state = state.clone();
    let query_credential = credential.to_owned();
    let query_permit = permit.clone();
    let operation_id = request.operation_id.clone();
    let query_scope = VoiceOwnerScope {
        project: scope.project.clone(),
        agent: scope.agent.clone(),
    };
    let unknown = tokio::task::spawn_blocking(move || {
        let store = query_state
            .evidence
            .as_ref()
            .as_ref()
            .map_err(|_| Refusal::Unavailable)?;
        personal_startup::check_archive_health(
            store,
            query_state.authority.receiver_identity(),
            &query_state.pairing,
        )
        .map_err(|_| Refusal::Unavailable)?;
        query_state.authority.authorize_send(
            &query_permit,
            &query_state.pairing,
            &query_credential,
            std::path::Path::new(&query_scope.project),
            &query_scope.agent,
        )?;
        if store
            .is_reserved(&operation_id)
            .map_err(|_| Refusal::Unavailable)?
        {
            return Ok(Vec::new());
        }
        let records = store
            .unresolved(query_state.authority.receiver_identity())
            .map_err(|_| Refusal::Unavailable)?;
        let mut history = History::Page {
            records,
            next: None,
        };
        refresh_history(store, &mut history)?;
        let History::Page { records, .. } = history else {
            return Err(Refusal::Unavailable);
        };
        Ok(records
            .into_iter()
            .filter(|record| {
                matches!(
                    record.outcome,
                    ActionOutcome::Unknown
                        | ActionOutcome::Accepted { .. }
                        | ActionOutcome::Dispatched { .. }
                )
            })
            .map(|record| record.identity)
            .collect())
    })
    .await
    .map_err(|_| Refusal::Unavailable)??;
    let independent = if unknown.is_empty() {
        Vec::new()
    } else {
        owner_independence(state, request, &unknown).await?
    };
    let state = state.clone();
    let credential = credential.to_owned();
    let permit = permit.clone();
    let request = request.clone();
    tokio::task::spawn_blocking(move || {
        reserve_send(&state, &credential, &permit, &request, &scope, &independent)
    })
    .await
    .map_err(|_| Refusal::Unavailable)?
}

async fn preflight_send(
    state: &HttpState,
    credential: &str,
    permit: &ReadPermit,
    identity: &ActionIdentity,
) -> Result<(), Refusal> {
    let state = state.clone();
    let credential = credential.to_owned();
    let permit = permit.clone();
    let identity = identity.clone();
    tokio::task::spawn_blocking(move || {
        let store = state
            .evidence
            .as_ref()
            .as_ref()
            .map_err(|_| Refusal::Unavailable)?;
        personal_startup::check_archive_health(
            store,
            state.authority.receiver_identity(),
            &state.pairing,
        )
        .map_err(|_| Refusal::Unavailable)?;
        let (current, _) = state.authority.authorize_send(
            &permit,
            &state.pairing,
            &credential,
            std::path::Path::new(&identity.project),
            &identity.agent,
        )?;
        if !same_identity(&current, &identity)
            || !state.controllers.write_capable(&identity.controller_id)
        {
            return Err(Refusal::Unavailable);
        }
        Ok(())
    })
    .await
    .map_err(|_| Refusal::Unavailable)?
}

fn cut(state: &HttpState, controller: &str) -> Result<(), Refusal> {
    state
        .controllers
        .cut_controller(controller)
        .map_err(|_| Refusal::Unavailable)
}

async fn prepare_owner(
    state: &HttpState,
    permit: &ReadPermit,
    request: &Request,
) -> Result<Option<ActionOutcome>, Refusal> {
    let result = state
        .controllers
        .dispatch(&request.controller_id, permit.frame.clone())
        .await;
    if let Ok(bytes) = result {
        if owner_prepared(
            &bytes,
            &request.controller_id,
            &request.subject_id,
            &request.operation_id,
        )
        .is_ok()
        {
            return Ok(None);
        }
        if let Ok(fenced @ ActionOutcome::FencedNoEffect { .. }) =
            owner_outcome(&bytes, &request.operation_id)
        {
            return Ok(Some(fenced));
        }
    }
    cut(state, &request.controller_id)?;
    Err(Refusal::Unavailable)
}

async fn commit_owner(
    state: &HttpState,
    credential: &str,
    permit: &ReadPermit,
    request: &Request,
    identity: &ActionIdentity,
) -> Result<ActionOutcome, Refusal> {
    let gate_id = match super::super::authority::random_hex(16) {
        Ok(gate_id) => gate_id,
        Err(error) => {
            cut(state, &request.controller_id)?;
            return Err(error);
        }
    };
    let gate_state = state.clone();
    let gate_credential = credential.to_owned();
    let gate_permit = permit.clone();
    let gate_identity = identity.clone();
    let gate = gate_id.clone();
    let remaining = tokio::task::spawn_blocking(move || {
        gate_state.controllers.register_gate(
            &gate_identity.controller_id,
            &gate_identity.operation_id,
            &gate,
            |generation, binding| {
                let store = gate_state
                    .evidence
                    .as_ref()
                    .as_ref()
                    .map_err(|_| Refusal::Unavailable)?;
                personal_startup::check_archive_health_under_barrier(
                    store,
                    gate_state.authority.receiver_identity(),
                    &gate_state.pairing,
                    generation,
                    binding,
                )
                .map_err(|_| Refusal::Unavailable)?;
                let (current, remaining) = gate_state.authority.authorize_send(
                    &gate_permit,
                    &gate_state.pairing,
                    &gate_credential,
                    std::path::Path::new(&gate_identity.project),
                    &gate_identity.agent,
                )?;
                if !same_identity(&current, &gate_identity) {
                    return Err(Refusal::Unavailable);
                }
                Ok(remaining)
            },
        )
    })
    .await
    .unwrap_or(Err(Refusal::Unavailable));
    let remaining = match remaining {
        Ok(remaining) => remaining,
        Err(error) => {
            cut(state, &request.controller_id)?;
            return Err(error);
        }
    };
    let Ok(commit) = serde_json::to_vec(&json!({"v":2,"type":"action_commit",
        "controller_id":request.controller_id,"operation_id":request.operation_id,
        "gate_id":gate_id,"remaining_ms":remaining}))
    else {
        cut(state, &request.controller_id)?;
        return Err(Refusal::Unavailable);
    };
    let outcome = state
        .controllers
        .dispatch(&request.controller_id, commit)
        .await
        .and_then(|bytes| owner_outcome(&bytes, &request.operation_id));
    if let Ok(outcome) = outcome {
        return Ok(outcome);
    }
    cut(state, &request.controller_id)?;
    Err(Refusal::Unavailable)
}

async fn send_receipt(
    state: &HttpState,
    credential: &str,
    permit: &ReadPermit,
    identity: &ActionIdentity,
    outcome: Option<ActionOutcome>,
) -> Result<Value, Refusal> {
    let state = state.clone();
    let credential = credential.to_owned();
    let permit = permit.clone();
    let identity = identity.clone();
    tokio::task::spawn_blocking(move || {
        let store = state
            .evidence
            .as_ref()
            .as_ref()
            .map_err(|_| Refusal::Unavailable)?;
        let admitted = matches!(outcome, Some(ActionOutcome::Accepted { .. }));
        if let Some(outcome) = outcome {
            store
                .report(
                    &identity,
                    &outcome,
                    crate::time::now_ms().max(identity.recorded_at_ms),
                )
                .map_err(|_| Refusal::Unavailable)?;
        }
        let record = store
            .lookup(&identity.operation_id)
            .map_err(|_| Refusal::Unavailable)?;
        state.authority.finish_send_delivery(
            &permit,
            &state.pairing,
            &credential,
            admitted,
            || state.controllers.contains(&identity.controller_id),
        )?;
        Ok(history_response(
            &History::Record(Box::new(record)),
            &identity.operation_id,
        ))
    })
    .await
    .map_err(|_| Refusal::Unavailable)?
}

pub(super) fn reserve_send(
    state: &HttpState,
    credential: &str,
    permit: &ReadPermit,
    request: &Request,
    scope: &VoiceOwnerScope,
    independent: &[String],
) -> Result<(ActionIdentity, ActionReservation), Refusal> {
    let store = state
        .evidence
        .as_ref()
        .as_ref()
        .map_err(|_| Refusal::Unavailable)?;
    personal_startup::check_archive_health(
        store,
        state.authority.receiver_identity(),
        &state.pairing,
    )
    .map_err(|_| Refusal::Unavailable)?;
    if !state.controllers.write_capable(&request.controller_id) {
        return Err(Refusal::Unavailable);
    }
    let (identity, _) = state.authority.authorize_send(
        permit,
        &state.pairing,
        credential,
        std::path::Path::new(&scope.project),
        &scope.agent,
    )?;
    if identity.controller_id != request.controller_id
        || identity.subject_id != request.subject_id
        || identity.operation_id != request.operation_id
        || identity.payload_hash != request.payload_hash
    {
        return Err(Refusal::Unavailable);
    }
    let generation = store.bound_generation().ok_or(Refusal::Unavailable)?;
    let reserved = store
        .reserve_session_prompt(generation, &identity, independent)
        .map_err(|error| match error {
            ActionEvidenceError::Malformed => Refusal::Malformed,
            ActionEvidenceError::Conflict => Refusal::Unauthenticated,
            ActionEvidenceError::Unresolved | ActionEvidenceError::Unavailable => {
                Refusal::Unavailable
            }
        })?;
    Ok((identity, reserved))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IndependenceReply {
    v: u64,
    #[serde(rename = "type")]
    kind: String,
    controller_id: String,
    subject_id: String,
    operation_id: String,
    independent: Vec<String>,
}

pub(super) fn validate_independence(
    bytes: &[u8],
    request: &Request,
    unknown: &[ActionIdentity],
) -> Result<Vec<String>, Refusal> {
    let reply: IndependenceReply =
        serde_json::from_slice(bytes).map_err(|_| Refusal::Unavailable)?;
    if reply.v != 1
        || reply.kind != "action_independence"
        || reply.controller_id != request.controller_id
        || reply.subject_id != request.subject_id
        || reply.operation_id != request.operation_id
        || reply.independent.len() > unknown.len()
    {
        return Err(Refusal::Unavailable);
    }
    let mut seen = std::collections::BTreeSet::new();
    for operation in &reply.independent {
        if !seen.insert(operation)
            || !unknown.iter().any(|old| {
                old.operation_id == *operation
                    && old.action == 3
                    && (old.controller_id != request.controller_id
                        || old.subject_id != request.subject_id)
            })
        {
            return Err(Refusal::Unavailable);
        }
    }
    Ok(reply.independent)
}

async fn owner_independence(
    state: &HttpState,
    request: &Request,
    unknown: &[ActionIdentity],
) -> Result<Vec<String>, Refusal> {
    let candidates: Vec<_> = unknown.iter().filter(|old| old.action == 3).map(|old| {
        json!({"operation_id":old.operation_id,"controller_id":old.controller_id,"subject_id":old.subject_id})
    }).collect();
    if candidates.is_empty() {
        return Err(Refusal::Unavailable);
    }
    let probe = serde_json::to_vec(&json!({"v":1,"type":"action_independence",
        "controller_id":request.controller_id,"subject_id":request.subject_id,
        "operation_id":request.operation_id,"unknowns":candidates}))
    .map_err(|_| Refusal::Unavailable)?;
    let reply = state
        .controllers
        .dispatch(&request.controller_id, probe)
        .await?;
    validate_independence(&reply, request, unknown)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerResult {
    v: u64,
    #[serde(rename = "type")]
    kind: String,
    operation_id: String,
    state: String,
    code: String,
    evidence: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerPrepared {
    v: u64,
    #[serde(rename = "type")]
    kind: String,
    controller_id: String,
    subject_id: String,
    operation_id: String,
}

pub(super) fn owner_prepared(
    bytes: &[u8],
    controller: &str,
    subject: &str,
    operation: &str,
) -> Result<(), Refusal> {
    let prepared: OwnerPrepared =
        serde_json::from_slice(bytes).map_err(|_| Refusal::Unavailable)?;
    if prepared.v != 1
        || prepared.kind != "action_prepared"
        || prepared.controller_id != controller
        || prepared.subject_id != subject
        || prepared.operation_id != operation
    {
        return Err(Refusal::Unavailable);
    }
    Ok(())
}

pub(super) fn owner_outcome(bytes: &[u8], operation_id: &str) -> Result<ActionOutcome, Refusal> {
    let result: OwnerResult = serde_json::from_slice(bytes).map_err(|_| Refusal::Unavailable)?;
    if result.v != 1
        || result.kind != "action_result"
        || result.operation_id != operation_id
        || fixed::<32>(&result.evidence).is_err()
    {
        return Err(Refusal::Unavailable);
    }
    match (result.state.as_str(), result.code.as_str()) {
        ("accepted", "") => Ok(ActionOutcome::Accepted {
            evidence: result.evidence,
        }),
        ("fenced_no_effect", "expired" | "stale_subject" | "busy" | "unavailable") => {
            Ok(ActionOutcome::FencedNoEffect {
                evidence: result.evidence,
            })
        }
        _ => Err(Refusal::Unavailable),
    }
}

pub(super) fn archive_owner_phases(
    archived: &ActionEvidence,
    owner: &ActionEvidence,
    mut report: impl FnMut(&ActionOutcome) -> Result<(), Refusal>,
) -> Result<(), Refusal> {
    if !same_identity(&archived.identity, &owner.identity) {
        return Err(Refusal::Unavailable);
    }
    if matches!(
        archived.outcome,
        ActionOutcome::Completed { .. }
            | ActionOutcome::Rejected { .. }
            | ActionOutcome::FencedNoEffect { .. }
    ) && archived.outcome != owner.outcome
    {
        return Err(Refusal::Unavailable);
    }
    for phase in archived.phases.iter().chain(
        matches!(
            archived.outcome,
            ActionOutcome::Accepted { .. }
                | ActionOutcome::Dispatched { .. }
                | ActionOutcome::Completed { .. }
        )
        .then_some(&archived.outcome),
    ) {
        if !owner.phases.contains(phase) && owner.outcome != *phase {
            return Err(Refusal::Unavailable);
        }
    }
    for phase in &owner.phases {
        report(phase)?;
    }
    report(&owner.outcome)
}

pub(super) fn refresh_history(
    store: &ActionEvidenceStore,
    history: &mut History,
) -> Result<(), Refusal> {
    let records = match history {
        History::Record(record) => std::slice::from_mut(record.as_mut()),
        History::Page { records, .. } => records.as_mut_slice(),
    };
    let pending: Vec<_> = records
        .iter()
        .enumerate()
        .filter(|(_, record)| {
            record.identity.action == 3
                && !matches!(
                    record.outcome,
                    ActionOutcome::Completed { .. }
                        | ActionOutcome::Rejected { .. }
                        | ActionOutcome::FencedNoEffect { .. }
                )
        })
        .map(|(index, record)| (index, record.identity.clone()))
        .collect();
    if pending.is_empty() {
        return Ok(());
    }
    let Some(path) = store.owner_journal_path() else {
        return Ok(());
    };
    let identities: Vec<_> = pending
        .iter()
        .map(|(_, identity)| identity.clone())
        .collect();
    // Close the owner journal before acquiring the archive's generation barrier.
    let owners = match owner_journal::inspect_existing(&path, &identities) {
        Ok(owners) => owners,
        Err(ActionEvidenceError::Unavailable) => return Ok(()),
        Err(_) => return Err(Refusal::Unavailable),
    };
    for ((index, _), owner) in pending.into_iter().zip(owners) {
        let Some(owner) = owner else {
            continue;
        };
        let archived = &mut records[index];
        if owner.outcome == archived.outcome && owner.phases == archived.phases {
            continue;
        }
        let at_ms = crate::time::now_ms().max(archived.updated_at_ms);
        archive_owner_phases(archived, &owner, |phase| {
            store
                .report(&archived.identity, phase, at_ms)
                .map_err(|_| Refusal::Unavailable)
        })?;
        *archived = store
            .lookup(&archived.identity.operation_id)
            .map_err(|_| Refusal::Unavailable)?;
    }
    Ok(())
}
