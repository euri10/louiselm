//! Exact-input startup rehearsals with independently denied Provider spending.

use super::{
    BrokerError, GrantRequest, InstalledBroker, lock, read_record, record_name, sync_directory,
    write_new_record,
};
use crate::{
    Digest,
    launch_receipt::{LaunchFailure, ReceiptOutcome, SessionState},
    session_manifest::SessionInputManifest,
};
use serde::{Deserialize, Serialize};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::Mutex,
};

/// Authenticated operator operations; none activates the selected paid Run.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "operation", rename_all = "snake_case", deny_unknown_fields)]
pub enum Operation {
    /// Invalidate the previous pass before any new preparation can fail.
    Invalidate {
        /// Exact inactive selection; no source or authority is opened.
        selection: serde_json::Value,
    },
    /// Bind current inputs and approve a distinct, no-Provider startup Run.
    Begin {
        /// Explicit trusted operator plugin checkout, never selected candidate code.
        plugin_digest: String,
        /// Complete inactive operator selection, never logged or persisted.
        selection: serde_json::Value,
        /// Remeasured broker-staged source/cache/Agent manifest.
        manifest_id: String,
    },
    /// Verify every consumption after the original owners have disposed it.
    Finish {
        /// Separate rehearsal Run identity returned by Begin.
        id: String,
        /// Ordered, bounded startup and capture observations from the operator.
        observations: Vec<Observation>,
    },
    /// Read verified durable cleanup progress; never persists a passing result.
    Inspect {
        /// Exact separate rehearsal Run.
        id: String,
        /// Ordered startup observations for the workers attempted so far.
        observations: Vec<Observation>,
    },
    /// Require a current pass before showing the paid dispatch UI.
    Check {
        /// Current trusted plugin Lua bytes, measured again by the installed CLI.
        plugin_digest: String,
        /// Exact selection being dispatched.
        selection: serde_json::Value,
        /// Fresh source/cache preparation, granting no authority.
        manifest_id: String,
    },
}

impl std::fmt::Debug for Operation {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("RehearsalOperation { selection: [redacted] }")
    }
}

/// Operator-observed initialization; terminal proof remains broker-owned.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Observation {
    /// ACP initialization and Session creation completed.
    pub started: bool,
    /// The actual capture CLI durably attached this ACP Session.
    pub attached: bool,
    /// Only normalized lfkw5.2 phase/code; never stderr or prompts.
    pub failure: Option<LaunchFailure>,
}

/// Exact inputs whose change invalidates a pass.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Inactive paid Run, whose latest attempt this result belongs to.
    pub paid_run_id: String,
    /// Trusted plugin Lua used by the coordinator and paid operator.
    pub plugin_digest: String,
    /// Complete canonical operator selection digest, including prompts and order.
    pub selection_digest: String,
    /// Installed signed release, not the source checkout's version string.
    pub release_id: String,
    /// Current kernel boot.
    pub boot_id: String,
    /// Exact source snapshot, cache, Agent, skills and isolation inputs.
    pub manifest_id: String,
    /// Native canonical tracker export and pinned executable identity.
    pub tracker_digest: String,
}

/// Redacted outcome, also used as the one-command CLI response.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Report {
    /// Closed response schema.
    pub schema: String,
    /// `ready` is a plan only; only `passed` permits paid dispatch.
    pub status: String,
    /// Rehearsal attempt identity; it conveys no Run authority.
    pub id: String,
    /// Exact inputs checked on this boot.
    pub binding: Binding,
    /// Every attempted worker, in reviewed order.
    pub workers: Vec<Worker>,
    /// Separate, non-paid authority returned only by Begin.
    pub plan: Option<Plan>,
}

/// Exact independent authority for the startup coordinator.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    /// One independent UUID Run per worker, matching capture's single anchor.
    pub runs: Vec<TrialRun>,
}

/// One startup-only Run with the same broker and capture identity.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TrialRun {
    /// Separate active Run; all Provider attempts are durably denied.
    pub envelope: super::run_envelope::RunEnvelope,
    /// Existing Run authorization contract.
    pub authorization: super::run_envelope::RunAuthorization,
}

/// One worker's cryptographically checked disposition and ledger count.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Worker {
    /// Canonical Run UUID shared by the broker and capture ledger.
    pub run_id: String,
    /// Authenticated Start evidence, without quarantine or output taint.
    pub startup_verified: bool,
    /// Selected Bead in the operator's reviewed order.
    pub bead_id: String,
    /// Exact child identity, retained even when ACP never started.
    pub session_id: String,
    /// Actual operator startup/capture observations.
    pub observation: Observation,
    /// A consumed slot lacks authenticated terminal cleanup.
    pub recovery_required: bool,
    /// Verified terminal head; null never counts as cleanup.
    pub terminal_head: Option<crate::launch_receipt::ReceiptHead>,
    /// Exact Session's durable Provider reservations.
    pub provider_attempts: u32,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Record {
    id: String,
    binding: Binding,
    plan: Plan,
    workers: usize,
    bead_ids: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Selection {
    schema: String,
    envelope: super::run_envelope::RunEnvelope,
    #[serde(default)]
    execution: super::run_envelope::ExecutionLimit,
    beads: Vec<Bead>,
    manifest: SessionInputManifest,
    cache: PathBuf,
    plan: PathBuf,
    snapshot_parent: PathBuf,
    input_group: u32,
    worktree: Worktree,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Bead {
    id: String,
    prompt: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Worktree {
    path: PathBuf,
    journal_parent: PathBuf,
    head: String,
}

pub(super) struct Rehearsals {
    root: PathBuf,
    serial: Mutex<()>,
}

impl Rehearsals {
    pub(super) fn check_child(&self, grant: &GrantRequest) -> Result<(), BrokerError> {
        let Some(record): Option<Record> = read_record(&self.path(&grant.request.run_id)?)? else {
            return Ok(());
        };
        let index = record
            .plan
            .runs
            .iter()
            .position(|trial| trial.envelope.run_id == grant.request.run_id)
            .ok_or(BrokerError::RequestMismatch)?;
        if grant.request.session_input_manifest_id != record.binding.manifest_id
            || grant.request.session_id != format!("{}-worker-{}", record.id, index + 1)
        {
            return Err(BrokerError::RequestMismatch);
        }
        Ok(())
    }
    pub(super) fn open(root: &Path) -> Result<Self, BrokerError> {
        fs::create_dir_all(root).map_err(BrokerError::Storage)?;
        sync_directory(root.parent().ok_or(BrokerError::InvalidGrant)?)?;
        Ok(Self {
            root: root.into(),
            serial: Mutex::new(()),
        })
    }

    fn path(&self, id: &str) -> Result<PathBuf, BrokerError> {
        Ok(self.root.join(record_name(id)?))
    }

    fn current(&self, digest: &str) -> PathBuf {
        self.root.join(format!(
            "current-{}.json",
            Digest::of(digest.as_bytes()).hex()
        ))
    }

    fn publish_current(&self, paid_run_id: &str, id: &str) -> Result<(), BrokerError> {
        let mut file = tempfile::NamedTempFile::new_in(&self.root).map_err(BrokerError::Storage)?;
        file.write_all(&serde_json::to_vec(id).map_err(|_| BrokerError::InvalidGrant)?)
            .map_err(BrokerError::Storage)?;
        file.as_file().sync_all().map_err(BrokerError::Storage)?;
        file.persist(self.current(paid_run_id))
            .map_err(|error| BrokerError::Storage(error.error))?;
        sync_directory(&self.root)
    }
}

impl InstalledBroker {
    /// Rehearses only startup; the original supervisors own signing and cleanup.
    /// Blocking I/O belongs on the authenticated operator worker.
    /// # Errors
    /// Refuses malformed/changed inputs, missing current proof or unavailable storage.
    #[expect(
        clippy::too_many_lines,
        reason = "The authenticated phases share one serialized durable rehearsal transaction."
    )]
    pub fn rehearsal(&self, operation: &Operation) -> Result<Report, BrokerError> {
        let _serial = lock(&self.rehearsals.serial);
        match operation {
            Operation::Invalidate { selection } => {
                let selection_digest = Digest::of(
                    &serde_json::to_vec(selection).map_err(|_| BrokerError::InvalidGrant)?,
                )
                .to_string();
                let binding = Binding {
                    paid_run_id: selection["envelope"]["run_id"]
                        .as_str()
                        .filter(|id| super::is_record_identifier(id))
                        .ok_or(BrokerError::InvalidGrant)?
                        .into(),
                    plugin_digest: Digest::of(b"unprepared").to_string(),
                    selection_digest,
                    release_id: self.verifier.config().release_id.clone(),
                    boot_id: String::new(),
                    manifest_id: Digest::of(b"unprepared").to_string(),
                    tracker_digest: Digest::of(b"unprepared").to_string(),
                };
                self.rehearsals
                    .publish_current(&binding.paid_run_id, "unprepared")?;
                Ok(Report {
                    schema: "louiselm.run-rehearsal/1".into(),
                    status: "invalidated".into(),
                    id: "unprepared".into(),
                    binding,
                    workers: Vec::new(),
                    plan: None,
                })
            }
            Operation::Begin {
                plugin_digest,
                selection,
                manifest_id,
            } => {
                let paid = selection["envelope"]["run_id"]
                    .as_str()
                    .filter(|id| super::is_record_identifier(id))
                    .ok_or(BrokerError::InvalidGrant)?;
                // Do not rely on the client's separate pre-preparation invalidation.
                self.rehearsals.publish_current(paid, "unprepared")?;
                let (selection, binding) =
                    self.rehearsal_binding(selection, manifest_id, plugin_digest)?;
                let uuid = fs::read_to_string("/proc/sys/kernel/random/uuid")
                    .map_err(BrokerError::Storage)?;
                let id = format!("rehearsal-{}", uuid.trim());
                let mut runs = Vec::new();
                for bead in &selection.beads {
                    let uuid = fs::read_to_string("/proc/sys/kernel/random/uuid")
                        .map_err(BrokerError::Storage)?;
                    let mut envelope = selection.envelope.clone();
                    envelope.run_id = uuid.trim().into();
                    envelope.max_sessions = 1;
                    let scope = envelope
                        .bead_scope
                        .as_mut()
                        .ok_or(BrokerError::InvalidGrant)?;
                    scope.issue_ids = vec![bead.id.clone()];
                    scope.max_mutations = 1;
                    if self.run_envelopes.current(&envelope.run_id)?.is_some() {
                        return Err(BrokerError::DuplicateAuthorization);
                    }
                    let draft = super::run_envelope::RunDraft {
                        schema: "louiselm.broker.run-draft/1".into(),
                        envelope,
                        execution: super::run_envelope::ExecutionLimit::Duration {
                            duration_ms: 1_800_000,
                        },
                    };
                    let envelope = draft.activate(super::now_ms()?)?;
                    // Durable denial precedes any launch/Run authority, including lost replies.
                    self.service
                        .provider_requests
                        .deny_rehearsal(&envelope.run_id)?;
                    let authorization = self.authorize_run(&envelope)?;
                    runs.push(TrialRun {
                        envelope,
                        authorization,
                    });
                }
                let record = Record {
                    id: id.clone(),
                    binding,
                    plan: Plan { runs },
                    workers: selection.beads.len(),
                    bead_ids: selection.beads.into_iter().map(|bead| bead.id).collect(),
                };
                write_new_record(&self.rehearsals.path(&id)?, &record)?;
                // Each Run's grant lookup pins the same complete immutable attempt.
                for trial in &record.plan.runs {
                    write_new_record(&self.rehearsals.path(&trial.envelope.run_id)?, &record)?;
                }
                // Starting a new attempt invalidates the old pass immediately.
                self.rehearsals
                    .publish_current(&record.binding.paid_run_id, &id)?;
                Ok(Report {
                    schema: "louiselm.run-rehearsal/1".into(),
                    status: "ready".into(),
                    id,
                    binding: record.binding,
                    workers: Vec::new(),
                    plan: Some(record.plan),
                })
            }
            Operation::Finish { id, observations } | Operation::Inspect { id, observations } => {
                let record: Record =
                    read_record(&self.rehearsals.path(id)?)?.ok_or(BrokerError::InvalidGrant)?;
                let mut report = self.rehearsal_result(&record, observations)?;
                if matches!(operation, Operation::Finish { .. }) {
                    write_new_record(&self.rehearsals.path(&format!("{id}-result"))?, &report)?;
                } else {
                    report.status = "observed".into();
                }
                Ok(report)
            }
            Operation::Check {
                plugin_digest,
                selection,
                manifest_id,
            } => {
                let (_, binding) = self.rehearsal_binding(selection, manifest_id, plugin_digest)?;
                let id: String = read_record(&self.rehearsals.current(&binding.paid_run_id))?
                    .ok_or(BrokerError::InvalidGrant)?;
                let record: Record =
                    read_record(&self.rehearsals.path(&id)?)?.ok_or(BrokerError::InvalidGrant)?;
                let saved: Report = read_record(&self.rehearsals.path(&format!("{id}-result"))?)?
                    .ok_or(BrokerError::InvalidGrant)?;
                if record.binding != binding || saved.binding != binding || saved.status != "passed"
                {
                    return Err(BrokerError::InvalidGrant);
                }
                let observations = saved
                    .workers
                    .into_iter()
                    .map(|worker| worker.observation)
                    .collect::<Vec<_>>();
                let report = self.rehearsal_result(&record, &observations)?;
                if report.status != "passed" {
                    return Err(BrokerError::InvalidGrant);
                }
                Ok(report)
            }
        }
    }

    fn rehearsal_binding(
        &self,
        value: &serde_json::Value,
        manifest_id: &str,
        plugin_digest: &str,
    ) -> Result<(Selection, Binding), BrokerError> {
        let current = crate::launcher_install::public_runtime_config(&self.preparation_paths)
            .map_err(BrokerError::InstallationAuthority)?;
        if current.release_id != self.verifier.config().release_id
            || current.conformance != crate::conformance::admission::Enforcement::Enforced
        {
            return Err(BrokerError::InvalidGrant);
        }
        let selection: Selection =
            serde_json::from_value(value.clone()).map_err(|_| BrokerError::InvalidGrant)?;
        Digest::parse(plugin_digest).map_err(|_| BrokerError::InvalidGrant)?;
        let mut ids = selection
            .beads
            .iter()
            .map(|bead| bead.id.clone())
            .collect::<Vec<_>>();
        ids.sort();
        let draft = super::run_envelope::RunDraft {
            schema: "louiselm.broker.run-draft/1".into(),
            envelope: selection.envelope.clone(),
            execution: selection.execution.clone(),
        };
        draft.activate(super::now_ms()?)?;
        if selection.schema != "louiselm.operator.bead-run/2"
            || !(3..=5).contains(&ids.len())
            || ids.windows(2).any(|pair| pair[0] == pair[1])
            || selection
                .envelope
                .bead_scope
                .as_ref()
                .is_none_or(|scope| scope.issue_ids != ids)
            || selection.envelope.max_sessions
                != 2 * u32::try_from(ids.len()).map_err(|_| BrokerError::InvalidGrant)?
            || selection.envelope.provider_requests.is_none()
        {
            return Err(BrokerError::InvalidGrant);
        }
        self.check_run_operator(&selection.envelope)?;
        self.check_unstarted_run(&selection.envelope.run_id)?;
        if self
            .run_envelopes
            .current(&selection.envelope.run_id)?
            .is_some()
        {
            return Err(BrokerError::InvalidGrant);
        }
        let plan_root = crate::workspace::filesystem::open_directory(
            selection.plan.parent().ok_or(BrokerError::InvalidGrant)?,
        )?;
        let plan_name = selection
            .plan
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or(BrokerError::InvalidGrant)?;
        let plan = crate::workspace::filesystem::read_source(&plan_root, plan_name, 65536)?
            .ok_or(BrokerError::InvalidGrant)?;
        if Digest::of(&plan.bytes).to_string() != selection.envelope.verification_plan_digest {
            return Err(BrokerError::RequestMismatch);
        }
        let digest = Digest::parse(manifest_id).map_err(|_| BrokerError::InvalidGrant)?;
        let input = self
            .service
            .verification_inputs
            .parent()
            .ok_or(BrokerError::InvalidGrant)?
            .join("workspace-inputs")
            .join(digest.hex());
        let loaded = crate::workspace::launch_inputs::load(&input, &digest)?;
        let preview = crate::workspace::launch_inputs::inspect(&input, &digest)?;
        let mut expected = selection.manifest.clone();
        expected
            .source_snapshot_digest
            .clone_from(&preview.source.snapshot_digest);
        expected
            .source_base_digest
            .clone_from(&preview.source.base_digest);
        if expected.canonical_bytes() != loaded.manifest.canonical_bytes()
            || preview.source.base_commit != selection.worktree.head
        {
            return Err(BrokerError::RequestMismatch);
        }
        let binding = Binding {
            paid_run_id: selection.envelope.run_id.clone(),
            plugin_digest: plugin_digest.into(),
            selection_digest: Digest::of(
                &serde_json::to_vec(value).map_err(|_| BrokerError::InvalidGrant)?,
            )
            .to_string(),
            release_id: self.verifier.config().release_id.clone(),
            boot_id: fs::read_to_string("/proc/sys/kernel/random/boot_id")
                .map_err(BrokerError::Storage)?
                .trim()
                .into(),
            manifest_id: manifest_id.into(),
            tracker_digest: self.service.rehearsal_tracker_digest()?.to_string(),
        };
        Ok((selection, binding))
    }

    fn rehearsal_result(
        &self,
        record: &Record,
        observations: &[Observation],
    ) -> Result<Report, BrokerError> {
        if observations.len() > record.workers
            || observations
                .iter()
                .any(|observation| !valid_observation(observation))
        {
            return Err(BrokerError::InvalidGrant);
        }
        let workers = observations
            .iter()
            .enumerate()
            .map(|(index, observation)| self.rehearsal_worker(record, index, observation))
            .collect::<Result<Vec<_>, _>>()?;
        let count = record.plan.runs.iter().try_fold(0, |count, trial| {
            self.service
                .authorizations()
                .session_count_for_run(&trial.envelope.run_id)
                .map(|added| count + added)
        })?;
        let recovery =
            count > observations.len() || workers.iter().any(|worker| worker.recovery_required);
        let unspent = record.plan.runs.iter().try_fold(true, |unspent, trial| {
            self.service
                .provider_requests
                .spent(&trial.envelope.run_id)
                .map(|count| unspent && count == 0)
        })?;
        let passed = unspent
            && workers.len() == record.workers
            && workers.iter().all(|worker| {
                worker.startup_verified
                    && worker.observation.started
                    && worker.observation.attached
                    && worker.observation.failure.is_none()
                    && worker.provider_attempts == 0
                    && worker.terminal_head.is_some()
            })
            && record.binding.release_id == self.verifier.config().release_id
            && record.binding.boot_id
                == fs::read_to_string("/proc/sys/kernel/random/boot_id")
                    .map_err(BrokerError::Storage)?
                    .trim()
            && record.binding.tracker_digest
                == self.service.rehearsal_tracker_digest()?.to_string();
        Ok(Report {
            schema: "louiselm.run-rehearsal/1".into(),
            status: if recovery {
                "recovery_required"
            } else if passed {
                "passed"
            } else {
                "failed"
            }
            .into(),
            id: record.id.clone(),
            binding: record.binding.clone(),
            workers,
            plan: None,
        })
    }

    fn rehearsal_worker(
        &self,
        record: &Record,
        index: usize,
        observation: &Observation,
    ) -> Result<Worker, BrokerError> {
        let id = format!("{}-worker-{}", record.id, index + 1);
        let run_id = &record
            .plan
            .runs
            .get(index)
            .ok_or(BrokerError::InvalidGrant)?
            .envelope
            .run_id;
        let mut worker = Worker {
            run_id: run_id.clone(),
            startup_verified: false,
            bead_id: record
                .bead_ids
                .get(index)
                .ok_or(BrokerError::InvalidGrant)?
                .clone(),
            session_id: id.clone(),
            observation: observation.clone(),
            recovery_required: false,
            terminal_head: None,
            provider_attempts: self
                .service
                .provider_requests
                .spent_by_session(run_id, &id)?,
        };
        match self.service.inspect(&id) {
            Ok(Some(view)) => {
                worker.startup_verified = view.start_evidence.is_some()
                    && view.output_taint.is_none()
                    && !self.service.lifecycle.is_quarantined(&id)?;
                let held = self
                    .service
                    .authorizations()
                    .consumed_for_session(&id)?
                    .ok_or(BrokerError::InvalidGrant)?;
                if &held.run_id != run_id
                    || view.launch_evidence.as_ref().is_some_and(|evidence| {
                        evidence.session_input_manifest_id != record.binding.manifest_id
                    })
                {
                    return Err(BrokerError::RequestMismatch);
                }
                let chain = self
                    .service
                    .receipts()
                    .inspection_chain(&held.launch_authorization());
                match chain {
                    Ok(chain)
                        if chain.last().is_some_and(|receipt| {
                            receipt.payload.resulting_state == SessionState::Terminal
                        }) =>
                    {
                        worker.terminal_head = view.broker_head;
                        if let Some(ReceiptOutcome::FailedLaunch { failure, .. }) =
                            chain.last().map(|receipt| &receipt.payload.outcome)
                        {
                            worker.observation.failure = Some(failure.clone());
                        }
                    }
                    _ => worker.recovery_required = true,
                }
            }
            Ok(None) => {
                if worker.observation.failure.is_none() {
                    worker.observation.failure = Some(LaunchFailure {
                        phase: "authorization".into(),
                        code: "refused".into(),
                    });
                }
            }
            Err(_) => worker.recovery_required = true,
        }
        if worker.recovery_required && worker.observation.failure.is_none() {
            worker.observation.failure = Some(LaunchFailure {
                phase: "receipt".into(),
                code: "cleanup_unproven".into(),
            });
        }
        Ok(worker)
    }
}

fn valid_observation(observation: &Observation) -> bool {
    (!observation.attached || observation.started)
        && observation.failure.as_ref().is_none_or(|failure| {
            matches!(
                failure.phase.as_str(),
                "launcher"
                    | "authorization"
                    | "workspace_prepare"
                    | "integration_measure"
                    | "tracker_prepare"
                    | "sandbox_prepare"
                    | "trace_attach"
                    | "bootstrap_handoff"
                    | "namespace_trace"
                    | "host_identity"
                    | "namespace_identity"
                    | "cgroup_admission"
                    | "conformance"
                    | "receipt"
                    | "worker_startup"
                    | "capture_attach"
            ) && matches!(
                failure.code.as_str(),
                "timeout"
                    | "io"
                    | "refused"
                    | "unavailable"
                    | "spawn_failed"
                    | "cleanup_unproven"
                    | "initialization_failed"
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::unwrap_used,
        reason = "Disposable durable latest-attempt fixture."
    )]
    fn a_new_attempt_invalidates_the_paid_run_across_restart() {
        let root = tempfile::tempdir().unwrap();
        let store = Rehearsals::open(root.path()).unwrap();
        store.publish_current("paid-one", "first-pass").unwrap();
        store.publish_current("paid-two", "other-pass").unwrap();
        store.publish_current("paid-one", "unprepared").unwrap();
        drop(store);
        let store = Rehearsals::open(root.path()).unwrap();
        assert_eq!(
            read_record::<String>(&store.current("paid-one"))
                .unwrap()
                .as_deref(),
            Some("unprepared")
        );
        assert_eq!(
            read_record::<String>(&store.current("paid-two"))
                .unwrap()
                .as_deref(),
            Some("other-pass")
        );
    }

    #[test]
    fn startup_categories_are_closed_and_capture_requires_startup() {
        let mut observation = Observation {
            started: false,
            attached: false,
            failure: Some(LaunchFailure {
                phase: "tracker_prepare".into(),
                code: "unavailable".into(),
            }),
        };
        assert!(valid_observation(&observation));
        observation.failure = Some(LaunchFailure {
            phase: "workspace_prepare".into(),
            code: "unavailable".into(),
        });
        assert!(valid_observation(&observation));
        observation.attached = true;
        assert!(!valid_observation(&observation));
        observation.attached = false;
        observation.failure = Some(LaunchFailure {
            phase: "private stderr".into(),
            code: "unavailable".into(),
        });
        assert!(!valid_observation(&observation));
    }
}
