//! Durable at-most-once mutation attempts. Interrupted effects stay unknown.
//! Only request digests are retained; payloads belong in canonical Beads.

use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

use serde::{Deserialize, Serialize};

use super::tracker_runner::{TrackerInvocation, TrackerRunner};
use super::{
    BrokerError, is_record_identifier, lock, read_record, sync_directory, write_new_record,
};
use crate::{
    Digest,
    beads_mutation::{
        ApprovedBeadsMutations, BeadsMutationKind, BeadsMutationOutcome, BeadsMutationRequest,
        BeadsMutationStatus,
    },
};

mod control;
mod escalation;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Binding {
    pub(super) session_id: String,
    pub(super) run_id: String,
    pub(super) agent_id: String,
    pub(super) envelope_revision: u64,
    pub(super) controller_uid: u32,
}

impl Binding {
    fn valid(&self) -> bool {
        self.controller_uid != 0
            && is_record_identifier(&self.session_id)
            && is_record_identifier(&self.run_id)
            && is_record_identifier(&self.agent_id)
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    binding: Binding,
    request_id: String,
    request_digest: String,
    project_digest: String,
    operation_id: String,
    created_at_ms: u64,
}

impl Record {
    fn valid(&self) -> bool {
        is_record_identifier(&self.request_id)
            && Digest::parse(&self.request_digest)
                .is_ok_and(|digest| digest.to_string() == self.request_digest)
            && Digest::parse(&self.project_digest)
                .is_ok_and(|digest| digest.to_string() == self.project_digest)
            && self.created_at_ms > 0
            && self.binding.valid()
            && super::attention::canonical_uuid(&self.operation_id)
    }

    fn status(&self, outcome: BeadsMutationOutcome) -> BeadsMutationStatus {
        BeadsMutationStatus {
            request_id: self.request_id.clone(),
            operation_id: self.operation_id.clone(),
            outcome,
        }
    }
}

/// Trusted composition-root configuration, never supplied by a Session.
pub(super) struct TrackerConfig {
    pub(super) program: PathBuf,
    pub(super) program_digest: Digest,
    pub(super) workspace_root: PathBuf,
    pub(super) scratch: PathBuf,
}

impl TrackerConfig {
    pub(super) fn project_digest(&self) -> String {
        Digest::of(self.workspace_root.as_os_str().as_encoded_bytes()).to_string()
    }
}

pub(super) struct BeadsMutations {
    root: PathBuf,
    writing: Mutex<()>,
}

impl BeadsMutations {
    pub(super) fn open(root: &Path) -> Result<Self, BrokerError> {
        for directory in [
            "requests",
            "outcomes",
            "scratch",
            "escalations",
            "decisions",
        ] {
            fs::create_dir_all(root.join(directory)).map_err(BrokerError::Storage)?;
        }
        sync_directory(root)?;
        if let Some(parent) = root.parent() {
            sync_directory(parent)?;
        }
        Ok(Self {
            root: root.into(),
            writing: Mutex::new(()),
        })
    }

    pub(super) fn scratch(&self) -> PathBuf {
        self.root.join("scratch")
    }

    /// Records intent before spending authority. An unresolved intent is never replayed:
    /// a crash before invocation is indistinguishable from one after br commits.
    pub(super) fn accept(
        &self,
        binding: &Binding,
        request: &BeadsMutationRequest,
        permission: &ApprovedBeadsMutations,
        now_ms: u64,
        runner: &dyn TrackerRunner,
        tracker: &TrackerConfig,
    ) -> Result<BeadsMutationStatus, BrokerError> {
        let clock = std::time::Instant::now();
        if !permission.permits(request, now_ms)
            || permission.project_digest != tracker.project_digest()
        {
            return Err(BrokerError::InvalidGrant);
        }
        let request_digest =
            Digest::of(&serde_json::to_vec(request).map_err(|_| BrokerError::InvalidGrant)?)
                .to_string();
        let _guard = lock(&self.writing);
        let path = self.request_path(&binding.session_id, &request.request_id);
        if let Some(record) = read_record::<Record>(&path)? {
            if !record.valid()
                || record.binding != *binding
                || record.request_digest != request_digest
                || record.request_id != request.request_id
                || record.project_digest != tracker.project_digest()
            {
                return Err(BrokerError::RequestMismatch);
            }
            let outcome = read_record(&self.outcome_path(&record.operation_id))?
                .unwrap_or(BeadsMutationOutcome::Unknown);
            return Ok(record.status(outcome));
        }
        self.check_budget(binding, permission.max_mutations)?;
        let record = Record {
            binding: binding.clone(),
            request_id: request.request_id.clone(),
            request_digest,
            project_digest: tracker.project_digest(),
            operation_id: operation_uuid()?,
            created_at_ms: now_ms,
        };
        if !record.valid() {
            return Err(BrokerError::InvalidGrant);
        }
        // Keep br scratch private and scoped to this call. No persistent payload copy.
        let scratch = tempfile::tempdir_in(&tracker.scratch).map_err(BrokerError::Storage)?;
        let invocation = build_invocation(&request.kind, binding, tracker, scratch.path());
        let current =
            now_ms.saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX));
        if !permission.permits(request, current) {
            return Err(BrokerError::Expired);
        }
        write_new_record(&path, &record)?;
        // Any runner error leaves durable uncertainty, including a successful write
        // whose exit status or cleanup could not be proved. Never call br again.
        let output = runner.run(&invocation)?;
        let outcome = if output.exit_code == Some(0) {
            BeadsMutationOutcome::Completed
        } else {
            BeadsMutationOutcome::Failed {
                exit_code: output.exit_code,
            }
        };
        write_new_record(&self.outcome_path(&record.operation_id), &outcome)?;
        Ok(record.status(outcome))
    }

    fn check_budget(&self, binding: &Binding, limit: u32) -> Result<(), BrokerError> {
        // ponytail: serialized scan; add an index only if retained history makes this costly.
        let mut used = 0_u32;
        for (total, entry) in fs::read_dir(self.root.join("requests"))
            .map_err(BrokerError::Storage)?
            .enumerate()
        {
            if total >= 4095 {
                return Err(BrokerError::InvalidGrant);
            }
            let path = entry.map_err(BrokerError::Storage)?.path();
            let record: Record = read_record(&path)?.ok_or(BrokerError::InvalidGrant)?;
            if !record.valid() {
                return Err(BrokerError::InvalidGrant);
            }
            if record.binding.session_id == binding.session_id {
                used = used.checked_add(1).ok_or(BrokerError::InvalidGrant)?;
                if used >= limit {
                    return Err(BrokerError::BeadsBudgetExhausted);
                }
            }
        }
        Ok(())
    }

    fn request_path(&self, session: &str, request: &str) -> PathBuf {
        self.root.join("requests").join(format!(
            "{}.json",
            Digest::of(format!("{session}\0{request}").as_bytes()).hex()
        ))
    }

    fn outcome_path(&self, operation: &str) -> PathBuf {
        self.root.join("outcomes").join(format!("{operation}.json"))
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DecisionRecord {
    decision: crate::beads_mutation::BeadsControlDecision,
    operator_uid: u32,
    decided_at_ms: u64,
}

impl BeadsMutations {
    fn decision_path(&self, operation: &str) -> PathBuf {
        self.root
            .join("decisions")
            .join(format!("{operation}.json"))
    }

    fn decision(&self, operation: &str) -> Result<Option<DecisionRecord>, BrokerError> {
        let record: Option<DecisionRecord> = read_record(&self.decision_path(operation))?;
        if record.as_ref().is_some_and(|value| {
            !value.decision.valid() || value.operator_uid == 0 || value.decided_at_ms == 0
        }) {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(record)
    }
}

fn build_invocation(
    kind: &BeadsMutationKind,
    binding: &Binding,
    tracker: &TrackerConfig,
    scratch: &Path,
) -> TrackerInvocation {
    let actor = format!("{}/{}", binding.agent_id, binding.session_id);
    let issue: OsString = kind.issue_id().into();
    let mut arguments = match kind {
        BeadsMutationKind::CommentAdd { text, .. } => vec![
            "comments".into(),
            "add".into(),
            issue,
            format!("--message={text}").into(),
        ],
        BeadsMutationKind::Claim { .. } => vec!["update".into(), issue, "--claim".into()],
        BeadsMutationKind::StatusUpdate { status, .. } => {
            vec!["update".into(), issue, format!("--status={status}").into()]
        }
        BeadsMutationKind::LabelAdd { label, .. } => {
            vec!["label".into(), "add".into(), issue, label.into()]
        }
        BeadsMutationKind::LabelRemove { label, .. } => {
            vec!["label".into(), "remove".into(), issue, label.into()]
        }
        BeadsMutationKind::DependencyAdd {
            depends_on_id,
            dependency_type,
            ..
        } => vec![
            "dep".into(),
            "add".into(),
            issue,
            depends_on_id.into(),
            format!("--type={}", dependency_type.name()).into(),
        ],
        BeadsMutationKind::DependencyRemove {
            depends_on_id,
            dependency_type,
            ..
        } => {
            vec![
                "dep".into(),
                "remove".into(),
                issue,
                depends_on_id.into(),
                format!("--type={}", dependency_type.name()).into(),
            ]
        }
        BeadsMutationKind::Close {
            reason, verdict, ..
        } => vec![
            "close".into(),
            issue,
            format!(
                "--reason={}:{} {reason}",
                verdict.kind.name(),
                verdict.reference
            )
            .into(),
        ],
    };
    arguments.extend([
        "--actor".into(),
        actor.into(),
        "--db".into(),
        tracker
            .workspace_root
            .join(".beads/beads.db")
            .into_os_string(),
        "--json".into(),
    ]);
    TrackerInvocation {
        program: tracker.program.clone(),
        program_digest: tracker.program_digest.clone(),
        arguments,
        environment: vec![(OsString::from("TMPDIR"), scratch.as_os_str().to_owned())],
        current_dir: tracker.workspace_root.clone(),
    }
}

fn operation_uuid() -> Result<String, BrokerError> {
    let mut bytes = [0_u8; 16];
    let mut offset = 0;
    while offset < bytes.len() {
        let count =
            rustix::rand::getrandom(&mut bytes[offset..], rustix::rand::GetRandomFlags::empty())
                .map_err(|error| BrokerError::Storage(error.into()))?;
        if count == 0 {
            return Err(BrokerError::InvalidGrant);
        }
        offset += count;
    }
    bytes[6] = (bytes[6] & 15) | 64;
    bytes[8] = (bytes[8] & 63) | 128;
    let hex = format!("{:032x}", u128::from_be_bytes(bytes));
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

#[cfg(test)]
#[path = "beads_mutation_tests.rs"]
mod tests;
