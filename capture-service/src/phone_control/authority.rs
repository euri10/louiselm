//! Durable grants and ephemeral phone-control authority.

#[path = "authority_prepared.rs"]
mod prepared;
#[path = "authority_run.rs"]
mod run;
#[path = "authority_selection.rs"]
mod selection;
#[path = "sleep_clock.rs"]
mod sleep_clock;
#[path = "authority_voice.rs"]
mod voice;
#[path = "authority_voice_job.rs"]
mod voice_job;

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use super::source::{PhoneSourceError, PhoneSourceStore, SourceProof};
use super::source_checkout::CheckoutProof;
use super::{
    ActionEvidence, ActionEvidenceStore, Frame, GrantRecord, HistoryQuery, MAX_FRAME, MAX_INTEGER,
    PeriodRecord, PhoneControlError, Proof, Request, fixed, key, validate_history_request_shape,
    validate_list, validate_proof_shape, validate_request_shape, verify_phone_control_frame,
    verify_proof,
};
use crate::{
    PairingRegistry, PhoneProjectError, PhoneProjectPage, PhoneProjectRootPage, PhoneProjectStore,
    hex::lower_hex, permissions::set_private_permissions,
};

const OFFER_LIFETIME: Duration = Duration::from_secs(120);
const IDLE_LIFETIME: Duration = Duration::from_secs(300);
const ABSOLUTE_LIFETIME: Duration = Duration::from_secs(3_600);
const MAX_STORED_BYTES: u64 = 1_048_576;

#[derive(Clone, Copy)]
struct OwnerReadTiming {
    now: Instant,
    wall_ms: u64,
    started: Instant,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Refusal {
    Unauthenticated,
    Expired,
    Revoked,
    OutOfScope,
    StaleSubject,
    Malformed,
    Unsupported,
    Unavailable,
}

impl Refusal {
    pub(super) fn code(self) -> &'static str {
        match self {
            Self::Unauthenticated => "unauthenticated",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
            Self::OutOfScope => "out_of_scope",
            Self::StaleSubject => "stale_subject",
            Self::Malformed => "malformed",
            Self::Unsupported => "unsupported",
            Self::Unavailable => "unavailable",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Scope {
    pub(super) projects: Vec<String>,
    pub(super) agents: Vec<String>,
    pub(super) groups: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct StoredGrant {
    pub(super) device_id: String,
    pub(super) enrollment_id: String,
    pub(super) grant_id: String,
    pub(super) grant_revision: u64,
    pub(super) enrolled_key: String,
    pub(super) receiver_identity: String,
    pub(super) scope: Scope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub(super) project_root_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    project_roots_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    grant_scope_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_scope_approval_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    confirmed_project_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    legacy_confirmations_sha256: Option<String>,
    #[serde(skip)]
    project_scope_valid: bool,
    #[serde(skip)]
    legacy_confirmations_valid: bool,
    #[serde(skip)]
    scope_approved: bool,
}

impl StoredGrant {
    fn require_approved_scope(&self) -> Result<(), Refusal> {
        if self.scope_approved {
            Ok(())
        } else {
            Err(Refusal::Unavailable)
        }
    }

    pub(super) fn project_roots(&self) -> Result<Option<&[String]>, Refusal> {
        if !self.project_scope_valid {
            return Err(Refusal::Unavailable);
        }
        Ok(self
            .project_roots_sha256
            .as_ref()
            .map(|_| self.project_root_ids.as_slice()))
    }

    fn legacy_project_confirmed(&self, project_id: &str) -> Result<bool, Refusal> {
        if self.project_roots()?.is_some() || !self.legacy_confirmations_valid {
            return Err(Refusal::Unavailable);
        }
        Ok(self.confirmed_project_ids.iter().any(|id| id == project_id))
    }
}

#[derive(Serialize)]
pub(super) struct GrantInspection {
    pub(super) grant_id: String,
    pub(super) grant_revision: u64,
    pub(super) enrolled_key: String,
    pub(super) receiver_identity: String,
    pub(super) scope: Scope,
    pub(super) scope_digest: String,
    pub(super) scope_approved: bool,
    pub(super) project_confirmations_valid: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LegacyScopeApproval {
    pub(super) grant_id: String,
    pub(super) grant_revision: u64,
    pub(super) scope_digest: String,
    pub(super) enrolled_key: String,
    pub(super) scope: Scope,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct StoredState {
    grants: BTreeMap<String, StoredGrant>,
    revisions: BTreeMap<String, u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct EnrollmentOffer {
    pub(super) enrollment_id: String,
    pub(super) matching_code: String,
    pub(super) expires_at_ms: u64,
}

struct PendingEnrollment {
    offer: EnrollmentOffer,
    deadline: Instant,
    failures: u8,
    enrolled_key: Option<String>,
}

pub(super) enum EnrollmentStatus {
    Pending {
        offer: EnrollmentOffer,
        enrolled_key: Option<String>,
    },
    Granted(Box<StoredGrant>),
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct IssuedChallenge {
    pub(super) grant_id: String,
    pub(super) grant_revision: u64,
    pub(super) receiver_identity: String,
    pub(super) challenge_id: String,
    pub(super) challenge: String,
    pub(super) expires_at_ms: u64,
}

struct PendingChallenge {
    challenge: IssuedChallenge,
    deadline: Instant,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Lease {
    pub(super) period_id: String,
    pub(super) idle_expires_at_ms: u64,
    pub(super) absolute_expires_at_ms: u64,
}

struct WorkingLease {
    public: Lease,
    proof: Proof,
    idle_deadline: Instant,
    absolute_deadline: Instant,
    sleep: sleep_clock::Lease,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ReadPermit {
    device: String,
    grant_id: String,
    period_id: String,
    operation_id: String,
    interaction: bool,
    interaction_started: Instant,
    sleep_started: sleep_clock::Sample,
    frame_sha256: [u8; 32],
    pub(super) frame: Vec<u8>,
}

#[derive(Debug, Eq, PartialEq)]
pub(super) enum History {
    Record(Box<ActionEvidence>),
    Page {
        records: Vec<ActionEvidence>,
        next: Option<String>,
    },
}

#[derive(Default)]
struct State {
    failed: bool,
    damaged_preserved: bool,
    grants: BTreeMap<String, StoredGrant>,
    revisions: BTreeMap<String, u64>,
    enrollments: BTreeMap<String, PendingEnrollment>,
    challenges: BTreeMap<String, PendingChallenge>,
    periods: BTreeMap<String, WorkingLease>,
    operations: BTreeSet<String>,
    pending_reads: BTreeSet<String>,
}

pub(super) struct Authority {
    path: PathBuf,
    receiver_identity: String,
    projects: Result<PhoneProjectStore, PhoneProjectError>,
    sleep_clock: Arc<dyn Fn() -> io::Result<sleep_clock::Sample> + Send + Sync>,
    state: Mutex<State>,
}

impl Authority {
    fn sleep_now(&self) -> Result<sleep_clock::Sample, Refusal> {
        (self.sleep_clock)().map_err(|error| {
            if error.kind() == io::ErrorKind::Unsupported {
                Refusal::Unsupported
            } else {
                Refusal::Unavailable
            }
        })
    }

    fn source_store(&self) -> Result<PhoneSourceStore, Refusal> {
        let parent = self.path.parent().ok_or(Refusal::Unavailable)?;
        PhoneSourceStore::open(parent.join("run-sources")).map_err(source_refusal)
    }

    pub(super) fn freeze_run_source(
        &self,
        project_id: &str,
        run_id: &str,
        snapshot: &str,
        includes: &[String],
    ) -> Result<SourceProof, Refusal> {
        let projects = self.projects.as_ref().map_err(|_| Refusal::Unavailable)?;
        self.source_store()?
            .freeze(projects, project_id, run_id, Path::new(snapshot), includes)
            .map_err(source_refusal)
    }

    pub(super) fn inspect_run_source(&self, run_id: &str) -> Result<SourceProof, Refusal> {
        self.source_store()?.read(run_id).map_err(source_refusal)
    }

    pub(super) fn prepare_run_checkout(
        &self,
        run_id: &str,
        source_id: &str,
        output: &str,
    ) -> Result<CheckoutProof, Refusal> {
        self.source_store()?
            .prepare_checkout(run_id, source_id, Path::new(output))
            .map_err(source_refusal)
    }

    pub(super) fn inspect_run_checkout(&self, run_id: &str) -> Result<CheckoutProof, Refusal> {
        self.source_store()?
            .read_checkout(run_id)
            .map_err(source_refusal)
    }

    pub(super) fn project_roots(&self, after: &str) -> Result<PhoneProjectRootPage, Refusal> {
        self.projects
            .as_ref()
            .map_err(|_| Refusal::Unavailable)?
            .roots_after(after)
            .map_err(|error| project_refusal(&error))
    }

    pub(super) fn projects(&self, after: &str) -> Result<PhoneProjectPage, Refusal> {
        self.projects
            .as_ref()
            .map_err(|_| Refusal::Unavailable)?
            .projects_after(after)
            .map_err(|error| project_refusal(&error))
    }

    pub(super) fn link_project_roots(
        &self,
        project_id: &str,
        roots: &[String],
    ) -> Result<(), Refusal> {
        self.projects
            .as_ref()
            .map_err(|_| Refusal::Unavailable)?
            .link_project_roots(project_id, roots)
            .map_err(|error| project_refusal(&error))
    }

    pub(super) fn register_project(&self, path: &str, roots: &[String]) -> Result<String, Refusal> {
        self.projects
            .as_ref()
            .map_err(|_| Refusal::Unavailable)?
            .register_project(Path::new(path), roots)
            .map_err(|error| project_refusal(&error))
    }

    pub(super) fn register_root(&self, path: &str) -> Result<String, Refusal> {
        self.projects
            .as_ref()
            .map_err(|_| Refusal::Unavailable)?
            .register_root(Path::new(path))
            .map_err(|error| project_refusal(&error))
    }

    pub(super) fn inspect_grant(
        &self,
        pairing: &PairingRegistry,
        device: &str,
    ) -> Result<GrantInspection, Refusal> {
        let state = self.lock()?;
        require_paired(pairing, device)?;
        let grant = state.grants.get(device).ok_or(Refusal::Revoked)?;
        Ok(GrantInspection {
            grant_id: grant.grant_id.clone(),
            grant_revision: grant.grant_revision,
            enrolled_key: grant.enrolled_key.clone(),
            receiver_identity: grant.receiver_identity.clone(),
            scope: grant.scope.clone(),
            scope_digest: grant_scope_hash(grant).map_err(|_| Refusal::Unavailable)?,
            scope_approved: grant.scope_approved,
            project_confirmations_valid: grant.legacy_confirmations_valid,
        })
    }

    pub(super) fn approve_legacy_scope(
        &self,
        pairing: &PairingRegistry,
        device: &str,
        mut approval: LegacyScopeApproval,
    ) -> Result<(Scope, bool), Refusal> {
        fixed::<16>(&approval.grant_id).map_err(|_| Refusal::Malformed)?;
        fixed::<32>(&approval.scope_digest).map_err(|_| Refusal::Malformed)?;
        if approval.grant_revision == 0 || approval.grant_revision > MAX_INTEGER {
            return Err(Refusal::Malformed);
        }
        key(&approval.enrolled_key).map_err(|_| Refusal::Malformed)?;
        validate_scope(&approval.scope)?;
        for project in &mut approval.scope.projects {
            let path = fs::canonicalize(&*project).map_err(|_| Refusal::Unavailable)?;
            if !path.is_dir() {
                return Err(Refusal::Malformed);
            }
            path.to_str().ok_or(Refusal::Malformed)?.clone_into(project);
        }
        validate_scope(&approval.scope)?;
        let mut state = self.lock()?;
        require_paired(pairing, device)?;
        let mut grants = state.grants.clone();
        let grant = grants.get_mut(device).ok_or(Refusal::Revoked)?;
        if grant.grant_id != approval.grant_id
            || grant.grant_revision != approval.grant_revision
            || grant.receiver_identity != self.receiver_identity
            || grant.enrolled_key != approval.enrolled_key
            || grant_scope_hash(grant).map_err(|_| Refusal::Unavailable)? != approval.scope_digest
        {
            return Err(Refusal::StaleSubject);
        }
        if grant.scope_approved
            || grant.project_roots_sha256.is_some()
            || !grant.project_root_ids.is_empty()
        {
            return Err(Refusal::Unsupported);
        }
        let preserve_confirmations =
            grant.legacy_confirmations_valid && grant.legacy_confirmations_sha256.is_some();
        grant.scope = approval.scope;
        let seal = grant_scope_hash(grant).map_err(|_| Refusal::Unavailable)?;
        grant.grant_scope_sha256 = Some(seal.clone());
        grant.legacy_scope_approval_sha256 = Some(seal);
        grant.scope_approved = true;
        if preserve_confirmations {
            grant.legacy_confirmations_sha256 =
                Some(legacy_confirmations_hash(grant).map_err(|_| Refusal::Unavailable)?);
        }
        let approved_scope = grant.scope.clone();
        let project_confirmations_valid = grant.legacy_confirmations_valid;
        let revisions = state.revisions.clone();
        self.persist_or_seal(&mut state, &grants, &revisions)?;
        state.grants = grants;
        state.challenges.remove(device);
        state.periods.remove(device);
        Ok((approved_scope, project_confirmations_valid))
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "The desktop must bind recovery to the exact paired grant, reviewed scope and selected project."
    )]
    pub(super) fn confirm_legacy_project(
        &self,
        pairing: &PairingRegistry,
        device: &str,
        grant_id: &str,
        grant_revision: u64,
        scope_digest: &str,
        project_id: &str,
        recover: bool,
    ) -> Result<(), Refusal> {
        fixed::<16>(grant_id).map_err(|_| Refusal::Malformed)?;
        fixed::<16>(project_id).map_err(|_| Refusal::Malformed)?;
        fixed::<32>(scope_digest).map_err(|_| Refusal::Malformed)?;
        let mut state = self.lock()?;
        require_paired(pairing, device)?;
        let mut grants = state.grants.clone();
        let grant = grants.get_mut(device).ok_or(Refusal::Revoked)?;
        grant.require_approved_scope()?;
        let current_digest = grant_scope_hash(grant).map_err(|_| Refusal::Unavailable)?;
        if grant.grant_id != grant_id
            || grant.grant_revision != grant_revision
            || grant.receiver_identity != self.receiver_identity
            || current_digest != scope_digest
        {
            return Err(Refusal::StaleSubject);
        }
        if grant.project_roots()?.is_some() {
            return Err(Refusal::Unsupported);
        }
        if !grant.scope.groups.iter().any(|group| group == "read") {
            return Err(Refusal::OutOfScope);
        }
        if recover && grant.legacy_confirmations_valid {
            return Err(Refusal::Unsupported);
        }
        if !recover && grant.legacy_project_confirmed(project_id)? {
            return Ok(());
        }
        let within_scope = self
            .projects
            .as_ref()
            .map_err(|_| Refusal::Unavailable)?
            .matches_current_parent(project_id, &grant.scope.projects)
            .map_err(|_| Refusal::Unavailable)?;
        if !within_scope {
            return Err(Refusal::OutOfScope);
        }
        if recover {
            grant.confirmed_project_ids.clear();
        }
        if grant.confirmed_project_ids.len() >= 50 {
            return Err(Refusal::Unavailable);
        }
        grant.confirmed_project_ids.push(project_id.to_owned());
        grant.confirmed_project_ids.sort();
        grant.legacy_confirmations_sha256 =
            Some(legacy_confirmations_hash(grant).map_err(|_| Refusal::Unavailable)?);
        grant.legacy_confirmations_valid = true;
        let revisions = state.revisions.clone();
        self.persist_or_seal(&mut state, &grants, &revisions)?;
        state.grants = grants;
        Ok(())
    }

    fn lock(&self) -> Result<MutexGuard<'_, State>, Refusal> {
        let state = self.state.lock().map_err(|_| Refusal::Unavailable)?;
        if state.failed {
            return Err(Refusal::Unavailable);
        }
        Ok(state)
    }

    fn persist_or_seal(
        &self,
        state: &mut State,
        grants: &BTreeMap<String, StoredGrant>,
        revisions: &BTreeMap<String, u64>,
    ) -> Result<(), Refusal> {
        if !state.damaged_preserved
            && state
                .grants
                .values()
                .any(|grant| !grant.legacy_confirmations_valid)
        {
            preserve_damaged(&self.path).map_err(|_| Refusal::Unavailable)?;
            state.damaged_preserved = true;
        }
        if persist(&self.path, grants, revisions).is_err() {
            state.failed = true;
            state.enrollments.clear();
            state.challenges.clear();
            state.periods.clear();
            state.pending_reads.clear();
            return Err(Refusal::Unavailable);
        }
        Ok(())
    }

    pub(super) fn open(path: &Path, receiver_identity: &str) -> Result<Self, Refusal> {
        Self::open_with_clock(path, receiver_identity, Arc::new(sleep_clock::sample))
    }

    pub(super) fn unavailable(path: &Path, receiver_identity: &str) -> Result<Self, Refusal> {
        fixed::<32>(receiver_identity).map_err(|_| Refusal::Malformed)?;
        let parent = path.parent().ok_or(Refusal::Unavailable)?;
        Ok(Self {
            path: path.to_owned(),
            receiver_identity: receiver_identity.to_owned(),
            projects: PhoneProjectStore::open(parent.join("project-identities")),
            sleep_clock: Arc::new(sleep_clock::sample),
            state: Mutex::new(State {
                failed: true,
                ..State::default()
            }),
        })
    }

    fn open_with_clock(
        path: &Path,
        receiver_identity: &str,
        sleep_clock: Arc<dyn Fn() -> io::Result<sleep_clock::Sample> + Send + Sync>,
    ) -> Result<Self, Refusal> {
        fixed::<32>(receiver_identity).map_err(|_| Refusal::Malformed)?;
        let parent = path.parent().ok_or(Refusal::Unavailable)?;
        fs::create_dir_all(parent).map_err(|_| Refusal::Unavailable)?;
        set_private_permissions(parent, true).map_err(|_| Refusal::Unavailable)?;
        let stored = if path.try_exists().map_err(|_| Refusal::Unavailable)? {
            let mut bytes = Vec::new();
            File::open(path)
                .map_err(|_| Refusal::Unavailable)?
                .take(MAX_STORED_BYTES + 1)
                .read_to_end(&mut bytes)
                .map_err(|_| Refusal::Unavailable)?;
            if bytes.len() as u64 > MAX_STORED_BYTES {
                return Err(Refusal::Unavailable);
            }
            let mut stored: StoredState =
                serde_json::from_slice(&bytes).map_err(|_| Refusal::Unavailable)?;
            if stored
                .revisions
                .values()
                .any(|revision| *revision == 0 || *revision > MAX_INTEGER)
            {
                return Err(Refusal::Unavailable);
            }
            for (device, grant) in &mut stored.grants {
                if device != &grant.device_id
                    || fixed::<16>(&grant.enrollment_id).is_err()
                    || fixed::<16>(&grant.grant_id).is_err()
                    || fixed::<32>(&grant.receiver_identity).is_err()
                    || key(&grant.enrolled_key).is_err()
                    || grant.grant_revision == 0
                    || grant.grant_revision > MAX_INTEGER
                    || stored.revisions.get(device) != Some(&grant.grant_revision)
                    || validate_scope(&grant.scope).is_err()
                {
                    return Err(Refusal::Unavailable);
                }
                if let Some(seal) = &grant.grant_scope_sha256 {
                    if grant_scope_hash(grant).map_err(|_| Refusal::Unavailable)? != *seal {
                        return Err(Refusal::Unavailable);
                    }
                } else if grant.project_roots_sha256.is_some() {
                    return Err(Refusal::Unavailable);
                }
                grant.scope_approved = grant.project_roots_sha256.is_some()
                    || (grant.project_root_ids.is_empty()
                        && grant.grant_scope_sha256.is_some()
                        && grant.legacy_scope_approval_sha256 == grant.grant_scope_sha256);
                grant.project_scope_valid = project_scope_valid(grant);
                grant.legacy_confirmations_valid = legacy_confirmations_valid(grant);
            }
            stored
        } else {
            StoredState::default()
        };
        Ok(Self {
            path: path.to_owned(),
            receiver_identity: receiver_identity.to_owned(),
            projects: PhoneProjectStore::open(parent.join("project-identities")),
            sleep_clock,
            state: Mutex::new(State {
                grants: stored.grants,
                revisions: stored.revisions,
                ..State::default()
            }),
        })
    }

    pub(super) fn start(
        &self,
        device: &str,
        now: Instant,
        wall_ms: u64,
    ) -> Result<EnrollmentOffer, Refusal> {
        let expires_at_ms = expiry_ms(wall_ms, OFFER_LIFETIME)?;
        let deadline = now
            .checked_add(OFFER_LIFETIME)
            .ok_or(Refusal::Unavailable)?;
        let offer = EnrollmentOffer {
            enrollment_id: random_hex(16)?,
            matching_code: random_code()?,
            expires_at_ms,
        };
        let mut state = self.lock()?;
        state.enrollments.insert(
            device.to_owned(),
            PendingEnrollment {
                offer: offer.clone(),
                deadline,
                failures: 0,
                enrolled_key: None,
            },
        );
        Ok(offer)
    }

    pub(super) fn submit(
        &self,
        device: &str,
        enrollment_id: &str,
        code: &str,
        enrolled_key: &str,
        now: Instant,
    ) -> Result<(), Refusal> {
        let started = Instant::now();
        let mut state = self.lock()?;
        let offer = state
            .enrollments
            .get_mut(device)
            .ok_or(Refusal::Unauthenticated)?;
        if elapsed_now(now, started)? >= offer.deadline {
            state.enrollments.remove(device);
            return Err(Refusal::Expired);
        }
        let malformed = code.len() != 8
            || !code.bytes().all(|byte| byte.is_ascii_digit())
            || key(enrolled_key).is_err();
        let matches = !malformed
            && offer.offer.enrollment_id == enrollment_id
            && bool::from(offer.offer.matching_code.as_bytes().ct_eq(code.as_bytes()));
        if !matches {
            offer.failures += 1;
            if offer.failures >= 5 {
                state.enrollments.remove(device);
            }
            return Err(if malformed {
                Refusal::Malformed
            } else {
                Refusal::Unauthenticated
            });
        }
        offer.enrolled_key = Some(enrolled_key.to_owned());
        Ok(())
    }

    pub(super) fn approve(
        &self,
        device: &str,
        enrollment_id: &str,
        enrolled_key: &str,
        mut scope: Scope,
        selected_roots: &[String],
        now: Instant,
    ) -> Result<StoredGrant, Refusal> {
        let started = Instant::now();
        validate_scope(&scope)?;
        if (!selected_roots.is_empty() && selected_roots.len() != scope.projects.len())
            || selected_roots.iter().any(|id| fixed::<16>(id).is_err())
            || selected_roots.iter().collect::<BTreeSet<_>>().len() != selected_roots.len()
        {
            return Err(Refusal::Malformed);
        }
        for project in &mut scope.projects {
            let path = fs::canonicalize(&*project).map_err(|_| Refusal::Unavailable)?;
            if !path.is_dir() {
                return Err(Refusal::Malformed);
            }
            path.to_str().ok_or(Refusal::Malformed)?.clone_into(project);
        }
        validate_scope(&scope)?;
        let mut state = self.lock()?;
        let offer = state
            .enrollments
            .get(device)
            .ok_or(Refusal::Unauthenticated)?;
        if elapsed_now(now, started)? >= offer.deadline {
            state.enrollments.remove(device);
            return Err(Refusal::Expired);
        }
        if offer.offer.enrollment_id != enrollment_id
            || offer.enrolled_key.as_deref() != Some(enrolled_key)
        {
            return Err(Refusal::Unauthenticated);
        }
        let revision = state
            .revisions
            .get(device)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(Refusal::Unavailable)?;
        if revision > MAX_INTEGER {
            return Err(Refusal::Unavailable);
        }
        let projects = self.projects.as_ref().map_err(|_| Refusal::Unavailable)?;
        let project_root_ids = if selected_roots.is_empty() {
            scope
                .projects
                .iter()
                .map(|project| {
                    projects
                        .register_root(Path::new(project))
                        .map_err(|_| Refusal::Unavailable)
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            for (id, project) in selected_roots.iter().zip(&scope.projects) {
                let current = projects
                    .check_current_root(id)
                    .map_err(|_| Refusal::Unavailable)?;
                if current != Path::new(project) {
                    return Err(Refusal::OutOfScope);
                }
            }
            selected_roots.to_vec()
        };
        let mut grant = StoredGrant {
            device_id: device.to_owned(),
            enrollment_id: enrollment_id.to_owned(),
            grant_id: random_hex(16)?,
            grant_revision: revision,
            enrolled_key: enrolled_key.to_owned(),
            receiver_identity: self.receiver_identity.clone(),
            scope,
            project_root_ids,
            project_roots_sha256: None,
            grant_scope_sha256: None,
            legacy_scope_approval_sha256: None,
            confirmed_project_ids: Vec::new(),
            legacy_confirmations_sha256: None,
            project_scope_valid: true,
            legacy_confirmations_valid: true,
            scope_approved: true,
        };
        grant.grant_scope_sha256 =
            Some(grant_scope_hash(&grant).map_err(|_| Refusal::Unavailable)?);
        grant.project_roots_sha256 =
            Some(project_scope_hash(&grant).map_err(|_| Refusal::Unavailable)?);
        let mut grants = state.grants.clone();
        grants.insert(device.to_owned(), grant.clone());
        let mut revisions = state.revisions.clone();
        revisions.insert(device.to_owned(), revision);
        self.persist_or_seal(&mut state, &grants, &revisions)?;
        state.grants = grants;
        state.revisions = revisions;
        state.enrollments.remove(device);
        state.challenges.remove(device);
        state.periods.remove(device);
        Ok(grant)
    }

    pub(super) fn challenge(
        &self,
        device: &str,
        grant_id: &str,
        revision: u64,
        now: Instant,
        wall_ms: u64,
    ) -> Result<IssuedChallenge, Refusal> {
        let mut state = self.lock()?;
        let grant = state.grants.get(device).ok_or(Refusal::Revoked)?;
        grant.require_approved_scope()?;
        if grant.grant_id != grant_id
            || grant.grant_revision != revision
            || grant.receiver_identity != self.receiver_identity
        {
            return Err(Refusal::Revoked);
        }
        let challenge = IssuedChallenge {
            grant_id: grant_id.to_owned(),
            grant_revision: revision,
            receiver_identity: self.receiver_identity.clone(),
            challenge_id: random_hex(16)?,
            challenge: random_hex(32)?,
            expires_at_ms: expiry_ms(wall_ms, OFFER_LIFETIME)?,
        };
        let deadline = now
            .checked_add(OFFER_LIFETIME)
            .ok_or(Refusal::Unavailable)?;
        state.challenges.insert(
            device.to_owned(),
            PendingChallenge {
                challenge: challenge.clone(),
                deadline,
            },
        );
        Ok(challenge)
    }

    pub(super) fn unlock(
        &self,
        device: &str,
        proof: &Proof,
        now: Instant,
        wall_ms: u64,
    ) -> Result<Lease, Refusal> {
        let started = Instant::now();
        let sleep_started = self.sleep_now()?;
        validate_proof_shape(proof).map_err(refusal)?;
        let mut state = self.lock()?;
        let grant = state.grants.get(device).ok_or(Refusal::Revoked)?;
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
        if elapsed_now(now, started)? >= pending.deadline {
            state.challenges.remove(device);
            return Err(Refusal::Expired);
        }
        if pending.challenge.challenge_id != proof.challenge_id
            || pending.challenge.challenge != proof.challenge
        {
            return Err(Refusal::Unauthenticated);
        }
        let enrolled = key(&grant.enrolled_key).map_err(|_| Refusal::Unavailable)?;
        verify_proof(proof, &enrolled).map_err(refusal)?;
        if elapsed_now(now, started)? >= pending.deadline {
            state.challenges.remove(device);
            return Err(Refusal::Expired);
        }
        let public = Lease {
            period_id: random_hex(16)?,
            idle_expires_at_ms: expiry_ms(wall_ms, IDLE_LIFETIME)?,
            absolute_expires_at_ms: expiry_ms(wall_ms, ABSOLUTE_LIFETIME)?,
        };
        let idle_deadline = now.checked_add(IDLE_LIFETIME).ok_or(Refusal::Unavailable)?;
        let absolute_deadline = now
            .checked_add(ABSOLUTE_LIFETIME)
            .ok_or(Refusal::Unavailable)?;
        let mut sleep = sleep_clock::Lease::new(&sleep_started)?;
        sleep.check(&self.sleep_now()?)?;
        state.challenges.remove(device);
        state.periods.insert(
            device.to_owned(),
            WorkingLease {
                public: public.clone(),
                proof: proof.clone(),
                idle_deadline,
                absolute_deadline,
                sleep,
            },
        );
        Ok(public)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one admission checks the signed frame, period, scope and replay together"
    )]
    pub(super) fn begin_read(
        &self,
        device: &str,
        request: &Request,
        now: Instant,
    ) -> Result<ReadPermit, Refusal> {
        let started = Instant::now();
        let sleep_started = self.sleep_now()?;
        match request.action {
            1 => validate_request_shape(request).map_err(refusal)?,
            52 => super::validate_prepared_list_request_shape(request).map_err(refusal)?,
            53 => super::validate_prepared_request_shape(request).map_err(refusal)?,
            54 => super::validate_run_list_request_shape(request).map_err(refusal)?,
            55 | 56 => super::validate_run_request_shape(request).map_err(refusal)?,
            57 => {
                super::run_session_query(request).map_err(refusal)?;
            }
            80 => {
                super::saved_review_query(request).map_err(refusal)?;
            }
            81 | 82 => {
                super::voice_query(request).map_err(refusal)?;
            }
            83..=85 => {
                super::voice_job::voice_job_command(request).map_err(refusal)?;
            }
            _ => super::validate_discovery_request_shape(request).map_err(refusal)?,
        }
        let mut state = self.lock()?;
        let grant = state.grants.get(device).cloned().ok_or(Refusal::Revoked)?;
        grant.require_approved_scope()?;
        if grant.grant_id != request.grant_id
            || grant.grant_revision != request.grant_revision
            || grant.receiver_identity != self.receiver_identity
        {
            return Err(Refusal::Revoked);
        }
        let lease = state
            .periods
            .get_mut(device)
            .ok_or(Refusal::Unauthenticated)?;
        if lease.public.period_id != request.period_id {
            return Err(Refusal::Unauthenticated);
        }
        let checked_now = elapsed_now(now, started)?;
        if checked_now >= lease.idle_deadline || checked_now >= lease.absolute_deadline {
            return Err(Refusal::Expired);
        }
        lease.sleep.check(&self.sleep_now()?)?;
        let frame = Frame {
            v: 1,
            kind: match request.action {
                1 => "read_session",
                48 => "list_controllers",
                49 => "list_sessions",
                50 => "list_selection",
                52 => "list_prepared",
                53 => "read_prepared",
                54 => "list_runs",
                55 => "read_run",
                56 => "read_run_live",
                57 => "read_run_session",
                80 => "read_saved_review",
                81 => "voice_history",
                82 => "voice_result",
                83 => "voice_admit",
                84 => "voice_cancel",
                85 => "voice_retry",
                _ => return Err(Refusal::Unsupported),
            }
            .into(),
            controller_id: request.controller_id.clone(),
            grant: GrantRecord {
                grant_id: grant.grant_id.clone(),
                grant_revision: grant.grant_revision,
                enrolled_key: grant.enrolled_key.clone(),
                projects: grant.scope.projects.clone(),
                agents: grant.scope.agents.clone(),
                groups: grant.scope.groups.clone(),
            },
            period: PeriodRecord {
                period_id: lease.public.period_id.clone(),
                challenge_id: lease.proof.challenge_id.clone(),
                idle_expires_at_ms: lease.public.idle_expires_at_ms,
                absolute_expires_at_ms: lease.public.absolute_expires_at_ms,
            },
            proof: lease.proof.clone(),
            request: request.clone(),
        };
        let frame = serde_json::to_vec(&frame).map_err(|_| Refusal::Unavailable)?;
        if frame.len() >= MAX_FRAME {
            return Err(Refusal::Malformed);
        }
        verify_phone_control_frame(&frame, &self.receiver_identity).map_err(refusal)?;
        let checked_now = elapsed_now(now, started)?;
        if checked_now >= lease.idle_deadline || checked_now >= lease.absolute_deadline {
            return Err(Refusal::Expired);
        }
        lease.sleep.check(&self.sleep_now()?)?;
        let group = if matches!(request.action, 83..=85) {
            "operate_sessions"
        } else {
            "read"
        };
        if !grant.scope.groups.iter().any(|value| value == group) {
            return Err(Refusal::OutOfScope);
        }
        if !state.operations.insert(request.operation_id.clone()) {
            return Err(Refusal::Unauthenticated);
        }
        state.pending_reads.insert(request.operation_id.clone());
        let frame_sha256 = Sha256::digest(&frame).into();
        Ok(ReadPermit {
            device: device.to_owned(),
            grant_id: request.grant_id.clone(),
            period_id: request.period_id.clone(),
            operation_id: request.operation_id.clone(),
            interaction: request.interaction == 1,
            interaction_started: now,
            sleep_started,
            frame_sha256,
            frame,
        })
    }

    #[cfg(test)]
    pub(super) fn finish_read(
        &self,
        permit: &ReadPermit,
        success: bool,
        now: Instant,
        wall_ms: u64,
    ) -> Result<(), Refusal> {
        let started = Instant::now();
        let mut state = self.lock()?;
        self.finish_read_locked(&mut state, permit, success, now, wall_ms, started)
    }

    pub(super) fn finish_read_delivery(
        &self,
        permit: &ReadPermit,
        success: bool,
        pairing: &PairingRegistry,
        credential: &str,
        controller_attached: impl FnOnce() -> bool,
    ) -> Result<(), Refusal> {
        let now = Instant::now();
        let wall_ms = crate::time::now_ms();
        let started = Instant::now();
        let mut state = self.lock()?;
        // Forget takes the authority lock before the pairing lock, too.
        let result = pairing.with_authenticated_device(credential, |device| {
            if device != Some(permit.device.as_str()) {
                state.pending_reads.remove(&permit.operation_id);
                return Err(Refusal::Revoked);
            }
            if !controller_attached() {
                state.pending_reads.remove(&permit.operation_id);
                return Err(Refusal::StaleSubject);
            }
            self.finish_read_locked(&mut state, permit, success, now, wall_ms, started)
        });
        if result.is_err() {
            state.pending_reads.remove(&permit.operation_id);
        }
        result.map_err(|_| Refusal::Unavailable)?
    }

    fn finish_read_locked(
        &self,
        state: &mut State,
        permit: &ReadPermit,
        success: bool,
        now: Instant,
        wall_ms: u64,
        started: Instant,
    ) -> Result<(), Refusal> {
        if !state.pending_reads.remove(&permit.operation_id) {
            return Err(Refusal::Unauthenticated);
        }
        let grant = state.grants.get(&permit.device).ok_or(Refusal::Revoked)?;
        grant.require_approved_scope()?;
        if grant.grant_id != permit.grant_id || grant.receiver_identity != self.receiver_identity {
            return Err(Refusal::Revoked);
        }
        let lease = state
            .periods
            .get_mut(&permit.device)
            .ok_or(Refusal::Revoked)?;
        if lease.public.period_id != permit.period_id {
            return Err(Refusal::Revoked);
        }
        let checked_now = elapsed_now(now, started)?;
        if checked_now >= lease.idle_deadline || checked_now >= lease.absolute_deadline {
            return Err(Refusal::Expired);
        }
        let sleep_now = self.sleep_now()?;
        lease.sleep.check(&sleep_now)?;
        if !success {
            return Err(Refusal::Unavailable);
        }
        if permit.interaction {
            lease.sleep.renew_idle(&permit.sleep_started, &sleep_now)?;
            let requested = permit
                .interaction_started
                .checked_add(IDLE_LIFETIME)
                .ok_or(Refusal::Unavailable)?;
            lease.idle_deadline = lease
                .idle_deadline
                .max(requested)
                .min(lease.absolute_deadline);
            let elapsed_ms = sleep_now
                .centiseconds
                .checked_sub(permit.sleep_started.centiseconds)
                .and_then(|value| value.checked_add(1))
                .and_then(|value| value.checked_mul(10))
                .ok_or(Refusal::Unavailable)?;
            let expires = expiry_ms(wall_ms.saturating_sub(elapsed_ms), IDLE_LIFETIME)?;
            lease.public.idle_expires_at_ms = lease
                .public
                .idle_expires_at_ms
                .max(expires)
                .min(lease.public.absolute_expires_at_ms);
        }
        Ok(())
    }

    fn check_pending_read_locked(
        &self,
        state: &mut State,
        permit: &ReadPermit,
        now: Instant,
        started: Instant,
    ) -> Result<(), Refusal> {
        if !state.pending_reads.contains(&permit.operation_id) {
            return Err(Refusal::Unauthenticated);
        }
        let grant = state.grants.get(&permit.device).ok_or(Refusal::Revoked)?;
        grant.require_approved_scope()?;
        if grant.grant_id != permit.grant_id || grant.receiver_identity != self.receiver_identity {
            return Err(Refusal::Revoked);
        }
        let lease = state
            .periods
            .get_mut(&permit.device)
            .ok_or(Refusal::Revoked)?;
        if lease.public.period_id != permit.period_id {
            return Err(Refusal::Revoked);
        }
        let checked_now = elapsed_now(now, started)?;
        if checked_now >= lease.idle_deadline || checked_now >= lease.absolute_deadline {
            return Err(Refusal::Expired);
        }
        lease.sleep.check(&self.sleep_now()?)
    }

    pub(super) fn read_history(
        &self,
        store: &ActionEvidenceStore,
        device: &str,
        request: &Request,
        now: Instant,
    ) -> Result<History, Refusal> {
        let started = Instant::now();
        let query = validate_history_request_shape(request).map_err(refusal)?;
        let mut state = self.lock()?;
        let grant = state.grants.get(device).cloned().ok_or(Refusal::Revoked)?;
        grant.require_approved_scope()?;
        if grant.grant_id != request.grant_id
            || grant.grant_revision != request.grant_revision
            || grant.receiver_identity != self.receiver_identity
        {
            return Err(Refusal::Revoked);
        }
        let lease = state
            .periods
            .get_mut(device)
            .ok_or(Refusal::Unauthenticated)?;
        if lease.public.period_id != request.period_id {
            return Err(Refusal::Unauthenticated);
        }
        let checked_now = elapsed_now(now, started)?;
        if checked_now >= lease.idle_deadline || checked_now >= lease.absolute_deadline {
            return Err(Refusal::Expired);
        }
        lease.sleep.check(&self.sleep_now()?)?;
        let frame = Frame {
            v: 1,
            kind: "read_action_history".into(),
            controller_id: request.controller_id.clone(),
            grant: GrantRecord {
                grant_id: grant.grant_id.clone(),
                grant_revision: grant.grant_revision,
                enrolled_key: grant.enrolled_key.clone(),
                projects: grant.scope.projects.clone(),
                agents: grant.scope.agents.clone(),
                groups: grant.scope.groups.clone(),
            },
            period: PeriodRecord {
                period_id: lease.public.period_id.clone(),
                challenge_id: lease.proof.challenge_id.clone(),
                idle_expires_at_ms: lease.public.idle_expires_at_ms,
                absolute_expires_at_ms: lease.public.absolute_expires_at_ms,
            },
            proof: lease.proof.clone(),
            request: request.clone(),
        };
        frame.verify(&self.receiver_identity).map_err(refusal)?;
        if !grant.scope.groups.iter().any(|group| group == "read") {
            return Err(Refusal::OutOfScope);
        }
        let scope = grant.scope.clone();
        // Authority precedes evidence; the evidence store never acquires authority.
        if store
            .is_reserved(&request.operation_id)
            .map_err(|_| Refusal::Unavailable)?
            || !state.operations.insert(request.operation_id.clone())
        {
            return Err(Refusal::Unauthenticated);
        }
        let result = match query {
            HistoryQuery::One(operation_id) => {
                let evidence = store
                    .lookup(&operation_id)
                    .map_err(|_| Refusal::Unavailable)?;
                let identity = &evidence.identity;
                if identity.receiver_identity != self.receiver_identity
                    || identity.controller_id != request.controller_id
                    || identity.subject_id != request.subject_id
                {
                    return Err(Refusal::StaleSubject);
                }
                if !scope.agents.contains(&identity.agent)
                    || !scope
                        .projects
                        .iter()
                        .any(|project| Path::new(&identity.project).starts_with(project))
                {
                    return Err(Refusal::OutOfScope);
                }
                History::Record(Box::new(evidence))
            }
            HistoryQuery::Page { after, limit } => {
                let (records, next) = store
                    .page(
                        after.as_deref(),
                        limit,
                        &self.receiver_identity,
                        &scope.projects,
                        &scope.agents,
                    )
                    .map_err(|_| Refusal::Unavailable)?;
                History::Page { records, next }
            }
        };
        let lease = state.periods.get_mut(device).ok_or(Refusal::Revoked)?;
        let checked_now = elapsed_now(now, started)?;
        if checked_now >= lease.idle_deadline || checked_now >= lease.absolute_deadline {
            return Err(Refusal::Expired);
        }
        lease.sleep.check(&self.sleep_now()?)?;
        Ok(result)
    }

    pub(super) fn disable(&self, device: &str) -> Result<(), Refusal> {
        let mut state = self.lock()?;
        if !state.grants.contains_key(device) {
            return Err(Refusal::Revoked);
        }
        let mut grants = state.grants.clone();
        grants.remove(device);
        let revisions = state.revisions.clone();
        self.persist_or_seal(&mut state, &grants, &revisions)?;
        state.grants = grants;
        state.enrollments.remove(device);
        state.challenges.remove(device);
        state.periods.remove(device);
        Ok(())
    }

    pub(super) fn cancel(&self, device: &str, enrollment_id: &str) -> Result<(), Refusal> {
        let mut state = self.lock()?;
        if state
            .enrollments
            .get(device)
            .is_none_or(|offer| offer.offer.enrollment_id != enrollment_id)
        {
            return Err(Refusal::Unauthenticated);
        }
        state.enrollments.remove(device);
        Ok(())
    }

    pub(super) fn enrollment(
        &self,
        device: &str,
        enrollment_id: Option<&str>,
        now: Instant,
    ) -> Result<EnrollmentStatus, Refusal> {
        let started = Instant::now();
        let mut state = self.lock()?;
        if let Some(offer) = state.enrollments.get(device) {
            if elapsed_now(now, started)? >= offer.deadline {
                state.enrollments.remove(device);
                return Err(Refusal::Expired);
            }
            if enrollment_id.is_none_or(|id| id == offer.offer.enrollment_id) {
                return Ok(EnrollmentStatus::Pending {
                    offer: offer.offer.clone(),
                    enrolled_key: offer.enrolled_key.clone(),
                });
            }
        }
        if let Some(grant) = state.grants.get(device)
            && enrollment_id == Some(grant.enrollment_id.as_str())
            && grant.receiver_identity == self.receiver_identity
        {
            return Ok(EnrollmentStatus::Granted(Box::new(grant.clone())));
        }
        Err(Refusal::Unauthenticated)
    }

    pub(super) fn receiver_identity(&self) -> &str {
        &self.receiver_identity
    }

    pub(super) fn forget(&self, pairing: &PairingRegistry, device: &str) -> Result<(), Refusal> {
        let mut state = self.state.lock().map_err(|_| Refusal::Unavailable)?;
        if state.failed {
            // Pairing revocation fences this phone even when grants cannot be trusted or written.
            return pairing.revoke(device).map_err(|_| Refusal::Unavailable);
        }
        let mut persistence = Ok(());
        if state.grants.contains_key(device) {
            let mut grants = state.grants.clone();
            grants.remove(device);
            let revisions = state.revisions.clone();
            persistence = self.persist_or_seal(&mut state, &grants, &revisions);
            if persistence.is_ok() {
                state.grants = grants;
            }
        }
        state.enrollments.remove(device);
        state.challenges.remove(device);
        state.periods.remove(device);
        pairing.revoke(device).map_err(|_| Refusal::Unavailable)?;
        persistence
    }
}

fn elapsed_now(observed: Instant, started: Instant) -> Result<Instant, Refusal> {
    observed
        .checked_add(started.elapsed())
        .ok_or(Refusal::Unavailable)
}

pub(super) fn refusal(error: PhoneControlError) -> Refusal {
    match error {
        PhoneControlError::Unauthenticated => Refusal::Unauthenticated,
        PhoneControlError::Malformed => Refusal::Malformed,
        PhoneControlError::Unsupported => Refusal::Unsupported,
        PhoneControlError::Unavailable => Refusal::Unavailable,
    }
}

fn expiry_ms(wall_ms: u64, duration: Duration) -> Result<u64, Refusal> {
    let millis = u64::try_from(duration.as_millis()).map_err(|_| Refusal::Unavailable)?;
    wall_ms
        .checked_add(millis)
        .filter(|value| *value <= MAX_INTEGER)
        .ok_or(Refusal::Unavailable)
}

pub(super) fn random_hex(length: usize) -> Result<String, Refusal> {
    let mut bytes = vec![0; length];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| Refusal::Unavailable)?;
    Ok(lower_hex(&bytes))
}

fn random_code() -> Result<String, Refusal> {
    const RANGE: u32 = 100_000_000;
    loop {
        let mut bytes = [0; 4];
        SystemRandom::new()
            .fill(&mut bytes)
            .map_err(|_| Refusal::Unavailable)?;
        let value = u32::from_be_bytes(bytes);
        if value < u32::MAX - (u32::MAX % RANGE) {
            return Ok(format!("{:08}", value % RANGE));
        }
    }
}

fn require_paired(pairing: &PairingRegistry, device: &str) -> Result<(), Refusal> {
    if pairing
        .status()
        .map_err(|_| Refusal::Unavailable)?
        .devices
        .iter()
        .any(|entry| entry.device_id == device)
    {
        Ok(())
    } else {
        Err(Refusal::Revoked)
    }
}

fn project_refusal(error: &PhoneProjectError) -> Refusal {
    match error {
        PhoneProjectError::Invalid("project is outside approved root") => Refusal::OutOfScope,
        PhoneProjectError::Invalid(
            "invalid root cursor"
            | "invalid project cursor"
            | "invalid approved roots"
            | "directory path must be absolute"
            | "directory path is invalid",
        ) => Refusal::Malformed,
        _ => Refusal::Unavailable,
    }
}

fn source_refusal(error: PhoneSourceError) -> Refusal {
    match error {
        PhoneSourceError::Unsupported => Refusal::Unsupported,
        PhoneSourceError::Conflict | PhoneSourceError::Invalid("source identity is stale") => {
            Refusal::StaleSubject
        }
        PhoneSourceError::Invalid(
            "Run UUID is not canonical"
            | "invalid selected source inputs"
            | "invalid source identity"
            | "snapshot path must be absolute",
        ) => Refusal::Malformed,
        PhoneSourceError::Project(error) => project_refusal(&error),
        _ => Refusal::Unavailable,
    }
}

fn grant_scope_hash(grant: &StoredGrant) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(&(
        &grant.device_id,
        &grant.enrollment_id,
        &grant.grant_id,
        grant.grant_revision,
        &grant.enrolled_key,
        &grant.receiver_identity,
        &grant.scope,
    ))?;
    Ok(lower_hex(&Sha256::digest(bytes)))
}

fn project_scope_hash(grant: &StoredGrant) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(&(&grant.device_id, &grant.grant_id, &grant.project_root_ids))?;
    Ok(lower_hex(&Sha256::digest(bytes)))
}

fn legacy_confirmations_hash(grant: &StoredGrant) -> Result<String, serde_json::Error> {
    let bytes = serde_json::to_vec(&(
        &grant.device_id,
        &grant.grant_id,
        grant.grant_revision,
        &grant.grant_scope_sha256,
        &grant.confirmed_project_ids,
    ))?;
    Ok(lower_hex(&Sha256::digest(bytes)))
}

fn legacy_confirmations_valid(grant: &StoredGrant) -> bool {
    let Some(seal) = &grant.legacy_confirmations_sha256 else {
        return grant.confirmed_project_ids.is_empty();
    };
    grant.grant_scope_sha256.is_some()
        && grant.confirmed_project_ids.len() <= 50
        && grant
            .confirmed_project_ids
            .iter()
            .all(|id| fixed::<16>(id).is_ok())
        && grant
            .confirmed_project_ids
            .windows(2)
            .all(|pair| pair[0] < pair[1])
        && legacy_confirmations_hash(grant).is_ok_and(|actual| &actual == seal)
}

fn project_scope_valid(grant: &StoredGrant) -> bool {
    let Some(seal) = &grant.project_roots_sha256 else {
        return grant.project_root_ids.is_empty();
    };
    grant.project_root_ids.len() == grant.scope.projects.len()
        && grant
            .project_root_ids
            .iter()
            .all(|id| fixed::<16>(id).is_ok())
        && grant.project_root_ids.iter().collect::<BTreeSet<_>>().len()
            == grant.project_root_ids.len()
        && project_scope_hash(grant).is_ok_and(|actual| &actual == seal)
}

fn validate_scope(scope: &Scope) -> Result<(), Refusal> {
    for values in [&scope.projects, &scope.agents, &scope.groups] {
        validate_list(values).map_err(|_| Refusal::Malformed)?;
        if values.iter().any(String::is_empty) {
            return Err(Refusal::Malformed);
        }
    }
    if scope
        .projects
        .iter()
        .any(|project| !Path::new(project).is_absolute())
    {
        return Err(Refusal::Malformed);
    }
    if scope.groups.iter().any(|group| {
        !matches!(
            group.as_str(),
            "read" | "operate_sessions" | "control_runs" | "saved_results"
        )
    }) {
        return Err(Refusal::Unsupported);
    }
    Ok(())
}

fn preserve_damaged(path: &Path) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("grant path has no parent"))?;
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MAX_STORED_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_STORED_BYTES {
        return Err(io::Error::other("phone-control grant store is too large"));
    }
    let id = uuid::Uuid::new_v4();
    let temporary = parent.join(format!(".grants-damaged-{id}.tmp"));
    let snapshot = parent.join(format!("grants-damaged-{id}.json"));
    let result = (|| {
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        set_private_permissions(&temporary, false)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::hard_link(&temporary, snapshot)?;
        fs::remove_file(&temporary)?;
        File::open(parent)?.sync_all()
    })();
    if temporary.exists() {
        // A partial snapshot is never authority; failure leaves the original file untouched.
        let _ = fs::remove_file(temporary);
    }
    result
}

fn persist(
    path: &Path,
    grants: &BTreeMap<String, StoredGrant>,
    revisions: &BTreeMap<String, u64>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("grant path has no parent"))?;
    let bytes = serde_json::to_vec(&StoredState {
        grants: grants.clone(),
        revisions: revisions.clone(),
    })?;
    if u64::try_from(bytes.len()).map_err(io::Error::other)? >= MAX_STORED_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "phone-control grant store is too large",
        ));
    }
    let temporary = parent.join(format!(".phone-control-{}", uuid::Uuid::new_v4()));
    let result = (|| {
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)?;
        set_private_permissions(&temporary, false)?;
        file.write_all(&bytes)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()
    })();
    if temporary.exists() {
        // A failed temporary write is never consulted as authority on restart.
        let _ = fs::remove_file(temporary);
    }
    result
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]
mod tests {
    use std::{
        path::Path,
        sync::{
            Arc, Mutex,
            atomic::{AtomicU64, Ordering},
        },
        time::{Duration, Instant},
    };

    use aws_lc_rs::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
    };
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    use super::super::{ActionEvidenceStore, ActionIdentity, Proof, Request, fixed, prefix};
    use super::{
        Authority, GrantInspection, History, IssuedChallenge, Lease, LegacyScopeApproval, Refusal,
        Scope, StoredGrant,
    };
    use crate::hex::lower_hex;

    const FIXTURE: &str =
        include_str!("../../../android/app/src/test/resources/phone-control-v1.json");

    fn unhex(text: &str) -> Vec<u8> {
        text.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    fn signed_proof(grant: &StoredGrant, challenge: &IssuedChallenge) -> Proof {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let enrolled_hash = fixture["proof"]["enrolled_key_hash"].as_str().unwrap();
        let working_key = fixture["keys"]["working"]["sec1"].as_str().unwrap();
        let mut transcript = prefix(1);
        transcript.extend(fixed::<16>(&grant.grant_id).unwrap());
        transcript.extend(grant.grant_revision.to_be_bytes());
        transcript.extend(fixed::<32>(enrolled_hash).unwrap());
        transcript.extend(fixed::<32>(&challenge.receiver_identity).unwrap());
        transcript.extend(fixed::<16>(&challenge.challenge_id).unwrap());
        transcript.extend(fixed::<32>(&challenge.challenge).unwrap());
        transcript.extend(fixed::<65>(working_key).unwrap());
        let key = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_ASN1_SIGNING,
            &unhex(fixture["keys"]["enrolled"]["pkcs8"].as_str().unwrap()),
        )
        .unwrap();
        let signature = key.sign(&SystemRandom::new(), &transcript).unwrap();
        Proof {
            grant_id: grant.grant_id.clone(),
            grant_revision: grant.grant_revision,
            enrolled_key_hash: enrolled_hash.to_owned(),
            receiver_identity: challenge.receiver_identity.clone(),
            challenge_id: challenge.challenge_id.clone(),
            challenge: challenge.challenge.clone(),
            working_key: working_key.to_owned(),
            signature: lower_hex(signature.as_ref()),
        }
    }

    fn signed_request(
        grant: &StoredGrant,
        lease: &Lease,
        interaction: u8,
        operation: u128,
    ) -> Request {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let controller = fixture["request"]["controller_id"].as_str().unwrap();
        let subject = fixture["request"]["subject_id"].as_str().unwrap();
        let payload = fixture["request"]["payload"].as_str().unwrap();
        let payload_hash = fixture["request"]["payload_hash"].as_str().unwrap();
        let operation_id = format!("{operation:032x}");
        let mut transcript = prefix(2);
        transcript.extend(fixed::<16>(&grant.grant_id).unwrap());
        transcript.extend(grant.grant_revision.to_be_bytes());
        transcript.extend(fixed::<16>(&lease.period_id).unwrap());
        transcript.extend(fixed::<32>(&grant.receiver_identity).unwrap());
        transcript.extend(fixed::<16>(controller).unwrap());
        transcript.extend(fixed::<16>(subject).unwrap());
        transcript.push(1);
        transcript.extend(fixed::<32>(payload_hash).unwrap());
        transcript.extend(fixed::<16>(&operation_id).unwrap());
        transcript.extend(0_u64.to_be_bytes());
        transcript.push(interaction);
        let key = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_ASN1_SIGNING,
            &unhex(fixture["keys"]["working"]["pkcs8"].as_str().unwrap()),
        )
        .unwrap();
        let signature = key.sign(&SystemRandom::new(), &transcript).unwrap();
        Request {
            grant_id: grant.grant_id.clone(),
            grant_revision: grant.grant_revision,
            period_id: lease.period_id.clone(),
            receiver_identity: grant.receiver_identity.clone(),
            controller_id: controller.to_owned(),
            subject_id: subject.to_owned(),
            action: 1,
            payload: payload.to_owned(),
            payload_hash: payload_hash.to_owned(),
            operation_id,
            expected_revision: 0,
            interaction: u64::from(interaction),
            signature: lower_hex(signature.as_ref()),
        }
    }

    fn signed_history(
        grant: &StoredGrant,
        lease: &Lease,
        original: &ActionIdentity,
        operation: u128,
    ) -> Request {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let mut request = signed_request(grant, lease, 0, operation);
        request.action = 2;
        request.controller_id.clone_from(&original.controller_id);
        request.subject_id.clone_from(&original.subject_id);
        request.payload = format!("00{}", original.operation_id);
        request.payload_hash = lower_hex(&Sha256::digest(unhex(&request.payload)));
        let mut bytes = prefix(2);
        bytes.extend(fixed::<16>(&request.grant_id).unwrap());
        bytes.extend(request.grant_revision.to_be_bytes());
        bytes.extend(fixed::<16>(&request.period_id).unwrap());
        bytes.extend(fixed::<32>(&request.receiver_identity).unwrap());
        bytes.extend(fixed::<16>(&request.controller_id).unwrap());
        bytes.extend(fixed::<16>(&request.subject_id).unwrap());
        bytes.push(2);
        bytes.extend(fixed::<32>(&request.payload_hash).unwrap());
        bytes.extend(fixed::<16>(&request.operation_id).unwrap());
        bytes.extend(request.expected_revision.to_be_bytes());
        bytes.push(0);
        let key = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_ASN1_SIGNING,
            &unhex(fixture["keys"]["working"]["pkcs8"].as_str().unwrap()),
        )
        .unwrap();
        request.signature = lower_hex(key.sign(&SystemRandom::new(), &bytes).unwrap().as_ref());
        request
    }

    fn unlocked(authority: &Authority, grant: &StoredGrant, now: Instant) -> Lease {
        let challenge = authority
            .challenge("phone", &grant.grant_id, grant.grant_revision, now, 1_000)
            .unwrap();
        authority
            .unlock("phone", &signed_proof(grant, &challenge), now, 1_000)
            .unwrap()
    }

    fn clocked_authority(directory: &Path) -> (Authority, Arc<AtomicU64>) {
        let ticks = Arc::new(AtomicU64::new(10_000));
        let source = Arc::clone(&ticks);
        let authority = Authority::open_with_clock(
            &directory.join("grants"),
            &"01".repeat(32),
            Arc::new(move || {
                Ok(super::sleep_clock::Sample {
                    centiseconds: source.load(Ordering::SeqCst),
                    namespace: "time:[1]".into(),
                })
            }),
        )
        .unwrap();
        (authority, ticks)
    }

    fn approved(authority: &Authority, project: &Path, now: Instant) -> StoredGrant {
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
        let offer = authority.start("phone", now, 1_000).unwrap();
        authority
            .submit(
                "phone",
                &offer.enrollment_id,
                &offer.matching_code,
                key,
                now,
            )
            .unwrap();
        authority
            .approve(
                "phone",
                &offer.enrollment_id,
                key,
                Scope {
                    projects: vec![project.to_str().unwrap().to_owned()],
                    agents: vec!["codex".into()],
                    groups: vec!["read".into()],
                },
                &[],
                now,
            )
            .unwrap()
    }

    fn legacy_approval(review: &GrantInspection, scope: Scope) -> LegacyScopeApproval {
        LegacyScopeApproval {
            grant_id: review.grant_id.clone(),
            grant_revision: review.grant_revision,
            scope_digest: review.scope_digest.clone(),
            enrolled_key: review.enrolled_key.clone(),
            scope,
        }
    }

    #[test]
    fn new_grant_retains_private_root_identity_across_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("grants");
        let receiver = "01".repeat(32);
        let authority = Authority::open(&path, &receiver).unwrap();
        let grant = approved(&authority, temporary.path(), Instant::now());
        assert_eq!(grant.project_root_ids.len(), 1);
        assert_eq!(grant.project_root_ids[0].len(), 32);
        drop(authority);

        let authority = Authority::open(&path, &receiver).unwrap();
        let state = authority.lock().unwrap();
        let restored = state.grants.get("phone").unwrap();
        assert_eq!(restored.project_root_ids, grant.project_root_ids);
        assert_eq!(restored.scope.projects, grant.scope.projects);
    }

    #[test]
    fn second_phone_can_explicitly_select_an_existing_current_root() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let authority =
            Authority::open(&temporary.path().join("grants"), &"01".repeat(32)).unwrap();
        let first = approved(&authority, temporary.path(), now);
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
        let offer = authority.start("second", now, 1_000).unwrap();
        authority
            .submit(
                "second",
                &offer.enrollment_id,
                &offer.matching_code,
                key,
                now,
            )
            .unwrap();
        let scope = Scope {
            projects: first.scope.projects.clone(),
            agents: vec!["codex".into()],
            groups: vec!["read".into()],
        };
        let foreign = temporary.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        let mut wrong_scope = scope.clone();
        wrong_scope.projects = vec![foreign.to_str().unwrap().into()];
        assert_eq!(
            authority.approve(
                "second",
                &offer.enrollment_id,
                key,
                wrong_scope,
                &first.project_root_ids,
                now
            ),
            Err(Refusal::OutOfScope)
        );
        let second = authority
            .approve(
                "second",
                &offer.enrollment_id,
                key,
                scope,
                &first.project_root_ids,
                now,
            )
            .unwrap();
        assert_eq!(second.project_root_ids, first.project_root_ids);
    }

    #[test]
    fn unavailable_project_identity_store_does_not_disable_session_reads() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("grants");
        let receiver = "01".repeat(32);
        let now = Instant::now();
        let authority = Authority::open(&path, &receiver).unwrap();
        let grant = approved(&authority, temporary.path(), now);
        drop(authority);
        std::fs::remove_file(temporary.path().join("project-identities/projects.json")).unwrap();

        let authority = Authority::open(&path, &receiver).unwrap();
        assert!(authority.projects.is_err());
        let lease = unlocked(&authority, &grant, now);
        let request = signed_request(&grant, &lease, 0, 1234);
        let permit = authority.begin_read("phone", &request, now).unwrap();
        assert_eq!(permit.grant_id, grant.grant_id);
    }

    #[test]
    fn unsealed_legacy_scope_cannot_issue_a_challenge() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("grants");
        let receiver = "01".repeat(32);
        let now = Instant::now();
        let authority = Authority::open(&path, &receiver).unwrap();
        let grant = approved(&authority, temporary.path(), now);
        drop(authority);
        let mut persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let legacy = persisted["grants"]["phone"].as_object_mut().unwrap();
        legacy.remove("project_root_ids");
        legacy.remove("project_roots_sha256");
        legacy.remove("grant_scope_sha256");
        legacy["scope"]["agents"] = serde_json::json!(["codex", "foreign"]);
        std::fs::write(&path, serde_json::to_vec(&persisted).unwrap()).unwrap();

        let authority = Authority::open(&path, &receiver).unwrap();
        assert_eq!(
            authority.challenge("phone", &grant.grant_id, grant.grant_revision, now, 1_000),
            Err(Refusal::Unavailable)
        );
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "The regression follows one paired grant through corruption, explicit review, restart and revocation."
    )]
    fn legacy_scope_reapproval_replaces_untrusted_scope_without_repairing() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("grants");
        let receiver = "01".repeat(32);
        let now = Instant::now();
        let pairing = crate::PairingRegistry::open(temporary.path().join("pairing")).unwrap();
        let offer = pairing
            .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
            .unwrap();
        let phone = pairing.consume(&offer.token, "phone", 1_001).unwrap();
        let authority = Authority::open(&path, &receiver).unwrap();
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
        let offer = authority.start(&phone.device_id, now, 1_000).unwrap();
        authority
            .submit(
                &phone.device_id,
                &offer.enrollment_id,
                &offer.matching_code,
                key,
                now,
            )
            .unwrap();
        let selected_scope = Scope {
            projects: vec![temporary.path().to_str().unwrap().into()],
            agents: vec!["codex".into()],
            groups: vec!["read".into()],
        };
        let grant = authority
            .approve(
                &phone.device_id,
                &offer.enrollment_id,
                key,
                selected_scope.clone(),
                &[],
                now,
            )
            .unwrap();
        let foreign_project = tempfile::tempdir().unwrap();
        drop(authority);
        let mut persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let legacy = persisted["grants"][&phone.device_id]
            .as_object_mut()
            .unwrap();
        legacy.remove("project_root_ids");
        legacy.remove("project_roots_sha256");
        legacy.remove("grant_scope_sha256");
        legacy["scope"]["projects"] = serde_json::json!([
            temporary.path().to_str().unwrap(),
            foreign_project.path().to_str().unwrap()
        ]);
        legacy["scope"]["agents"] = serde_json::json!(["codex", "foreign"]);
        legacy["scope"]["groups"] = serde_json::json!(["read", "operate_sessions"]);
        std::fs::write(&path, serde_json::to_vec(&persisted).unwrap()).unwrap();

        let authority = Authority::open(&path, &receiver).unwrap();
        assert_eq!(
            authority.challenge(
                &phone.device_id,
                &grant.grant_id,
                grant.grant_revision,
                now,
                1_000
            ),
            Err(Refusal::Unavailable)
        );
        assert_eq!(
            pairing.authenticate_device(&phone.credential).unwrap(),
            Some(phone.device_id.clone())
        );
        let review = authority.inspect_grant(&pairing, &phone.device_id).unwrap();
        assert!(!review.scope_approved);
        assert!(review.scope.agents.contains(&"foreign".into()));
        assert!(
            review
                .scope
                .projects
                .contains(&foreign_project.path().to_str().unwrap().into())
        );
        let mut stale = legacy_approval(&review, selected_scope.clone());
        stale.scope_digest = "00".repeat(32);
        assert_eq!(
            authority.approve_legacy_scope(&pairing, &phone.device_id, stale),
            Err(Refusal::StaleSubject)
        );
        let mut wrong_key = legacy_approval(&review, selected_scope.clone());
        wrong_key.enrolled_key = fixture["keys"]["working"]["sec1"].as_str().unwrap().into();
        assert_eq!(
            authority.approve_legacy_scope(&pairing, &phone.device_id, wrong_key),
            Err(Refusal::StaleSubject)
        );
        assert_eq!(
            authority.approve_legacy_scope(
                &pairing,
                &phone.device_id,
                legacy_approval(&review, selected_scope.clone())
            ),
            Ok((selected_scope.clone(), true))
        );
        drop(authority);

        let authority = Authority::open(&path, &receiver).unwrap();
        let review = authority.inspect_grant(&pairing, &phone.device_id).unwrap();
        assert!(review.scope_approved);
        assert_eq!(review.scope, selected_scope);
        assert_eq!(review.grant_id, grant.grant_id);
        assert_eq!(review.grant_revision, grant.grant_revision);
        let challenge = authority
            .challenge(
                &phone.device_id,
                &grant.grant_id,
                grant.grant_revision,
                now,
                1_000,
            )
            .unwrap();
        let lease = authority
            .unlock(
                &phone.device_id,
                &signed_proof(&grant, &challenge),
                now,
                1_000,
            )
            .unwrap();
        let request = signed_request(&grant, &lease, 0, 4567);
        let permit = authority
            .begin_read(&phone.device_id, &request, now)
            .unwrap();
        let frame: Value = serde_json::from_slice(&permit.frame).unwrap();
        assert_eq!(
            frame["grant"]["projects"],
            serde_json::json!(selected_scope.projects)
        );
        assert_eq!(frame["grant"]["agents"], serde_json::json!(["codex"]));
        assert_eq!(frame["grant"]["groups"], serde_json::json!(["read"]));
        authority.disable(&phone.device_id).unwrap();
        assert_eq!(
            authority.finish_read(&permit, true, now, 1_000),
            Err(Refusal::Revoked)
        );
        assert_eq!(
            pairing.authenticate_device(&phone.credential).unwrap(),
            Some(phone.device_id.clone())
        );
        assert_eq!(
            authority.approve_legacy_scope(
                &pairing,
                &phone.device_id,
                legacy_approval(&review, selected_scope)
            ),
            Err(Refusal::Revoked)
        );
    }

    #[test]
    fn legacy_path_only_grant_does_not_acquire_a_project_identity() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("grants");
        let receiver = "01".repeat(32);
        let authority = Authority::open(&path, &receiver).unwrap();
        approved(&authority, temporary.path(), Instant::now());
        drop(authority);
        let mut persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        persisted["grants"]["phone"]
            .as_object_mut()
            .unwrap()
            .remove("project_root_ids");
        persisted["grants"]["phone"]
            .as_object_mut()
            .unwrap()
            .remove("project_roots_sha256");
        persisted["grants"]["phone"]
            .as_object_mut()
            .unwrap()
            .remove("grant_scope_sha256");
        std::fs::write(&path, serde_json::to_vec(&persisted).unwrap()).unwrap();

        let authority = Authority::open(&path, &receiver).unwrap();
        let state = authority.lock().unwrap();
        assert_eq!(state.grants["phone"].project_roots(), Ok(None));
    }

    #[test]
    fn corrupted_grant_root_identity_is_not_accepted() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("grants");
        let receiver = "01".repeat(32);
        let authority = Authority::open(&path, &receiver).unwrap();
        let grant = approved(&authority, temporary.path(), Instant::now());
        let foreign = temporary.path().join("foreign");
        std::fs::create_dir(&foreign).unwrap();
        let foreign_id = authority
            .projects
            .as_ref()
            .unwrap()
            .register_root(&foreign)
            .unwrap();
        drop(authority);
        let mut persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        persisted["grants"]["phone"]["project_root_ids"][0] = foreign_id.clone().into();
        std::fs::write(&path, serde_json::to_vec(&persisted).unwrap()).unwrap();

        let authority = Authority::open(&path, &receiver).unwrap();
        let state = authority.lock().unwrap();
        assert_eq!(
            state.grants["phone"].project_roots(),
            Err(Refusal::Unavailable)
        );
        drop(state);
        let now = Instant::now();
        let lease = unlocked(&authority, &grant, now);
        let request = signed_request(&grant, &lease, 0, 5678);
        let permit = authority.begin_read("phone", &request, now).unwrap();
        assert_eq!(permit.grant_id, grant.grant_id);
    }

    #[test]
    fn corrupted_base_grant_scope_is_not_used_for_session_access() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("grants");
        let receiver = "01".repeat(32);
        let authority = Authority::open(&path, &receiver).unwrap();
        approved(&authority, temporary.path(), Instant::now());
        drop(authority);
        let mut persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        persisted["grants"]["phone"]["scope"]["agents"] = serde_json::json!(["codex", "foreign"]);
        std::fs::write(&path, serde_json::to_vec(&persisted).unwrap()).unwrap();
        assert!(matches!(
            Authority::open(&path, &receiver),
            Err(Refusal::Unavailable)
        ));
    }

    #[test]
    #[expect(
        clippy::too_many_lines,
        reason = "One legacy lifecycle proves per-phone confirmation, restart, revocation and corruption handling."
    )]
    fn legacy_project_confirmation_is_exact_per_phone_and_survives_restart() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("grants");
        let receiver = "01".repeat(32);
        let now = Instant::now();
        let approved_root = temporary.path().join("approved");
        std::fs::create_dir(&approved_root).unwrap();
        let pairing = crate::PairingRegistry::open(temporary.path().join("pairing")).unwrap();
        let offer = pairing
            .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
            .unwrap();
        let phone = pairing.consume(&offer.token, "phone", 1_001).unwrap();
        let authority = Authority::open(&path, &receiver).unwrap();
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
        let offer = authority.start(&phone.device_id, now, 1_000).unwrap();
        authority
            .submit(
                &phone.device_id,
                &offer.enrollment_id,
                &offer.matching_code,
                key,
                now,
            )
            .unwrap();
        let grant = authority
            .approve(
                &phone.device_id,
                &offer.enrollment_id,
                key,
                Scope {
                    projects: vec![approved_root.to_str().unwrap().into()],
                    agents: vec!["codex".into()],
                    groups: vec!["read".into()],
                },
                &[],
                now,
            )
            .unwrap();
        let original_project = approved_root.join("original-project");
        std::fs::create_dir(&original_project).unwrap();
        let project_id = authority
            .projects
            .as_ref()
            .unwrap()
            .register_project(&original_project, &grant.project_root_ids)
            .unwrap();
        let second_offer = pairing
            .issue("https://127.0.0.1:7391", &receiver, 2_000, 10_000)
            .unwrap();
        let second_phone = pairing
            .consume(&second_offer.token, "second", 2_001)
            .unwrap();
        let second_enrollment = authority
            .start(&second_phone.device_id, now, 2_000)
            .unwrap();
        authority
            .submit(
                &second_phone.device_id,
                &second_enrollment.enrollment_id,
                &second_enrollment.matching_code,
                key,
                now,
            )
            .unwrap();
        authority
            .approve(
                &second_phone.device_id,
                &second_enrollment.enrollment_id,
                key,
                grant.scope.clone(),
                &grant.project_root_ids,
                now,
            )
            .unwrap();
        std::fs::remove_dir(&original_project).unwrap();
        drop(authority);
        let mut persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        for device in [&phone.device_id, &second_phone.device_id] {
            let saved = persisted["grants"][device].as_object_mut().unwrap();
            saved.remove("project_root_ids");
            saved.remove("project_roots_sha256");
            saved.remove("grant_scope_sha256");
        }
        std::fs::write(&path, serde_json::to_vec(&persisted).unwrap()).unwrap();

        let authority = Authority::open(&path, &receiver).unwrap();
        let review = authority.inspect_grant(&pairing, &phone.device_id).unwrap();
        assert_eq!(review.grant_id, grant.grant_id);
        assert!(!review.scope_approved);
        assert_eq!(
            authority.confirm_legacy_project(
                &pairing,
                &phone.device_id,
                &grant.grant_id,
                grant.grant_revision,
                &review.scope_digest,
                &project_id,
                false,
            ),
            Err(Refusal::Unavailable)
        );
        assert_eq!(
            authority.confirm_legacy_project(
                &pairing,
                &phone.device_id,
                &grant.grant_id,
                grant.grant_revision,
                &review.scope_digest,
                &project_id,
                true,
            ),
            Err(Refusal::Unavailable)
        );
        authority
            .approve_legacy_scope(
                &pairing,
                &phone.device_id,
                legacy_approval(&review, grant.scope.clone()),
            )
            .unwrap();
        assert_eq!(
            authority.confirm_legacy_project(
                &pairing,
                &phone.device_id,
                &grant.grant_id,
                grant.grant_revision,
                &"00".repeat(32),
                &project_id,
                false,
            ),
            Err(Refusal::StaleSubject)
        );
        authority
            .confirm_legacy_project(
                &pairing,
                &phone.device_id,
                &grant.grant_id,
                grant.grant_revision,
                &review.scope_digest,
                &project_id,
                false,
            )
            .unwrap();
        // A project confirmation created before scope re-approval is not itself
        // evidence that the whole legacy scope was approved.
        drop(authority);
        let mut persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        persisted["grants"][&phone.device_id]
            .as_object_mut()
            .unwrap()
            .remove("legacy_scope_approval_sha256");
        std::fs::write(&path, serde_json::to_vec(&persisted).unwrap()).unwrap();
        let authority = Authority::open(&path, &receiver).unwrap();
        let review = authority.inspect_grant(&pairing, &phone.device_id).unwrap();
        assert!(!review.scope_approved);
        assert!(review.project_confirmations_valid);
        authority
            .approve_legacy_scope(
                &pairing,
                &phone.device_id,
                legacy_approval(&review, grant.scope.clone()),
            )
            .unwrap();
        assert_eq!(
            authority.lock().unwrap().grants[&phone.device_id]
                .legacy_project_confirmed(&project_id),
            Ok(true)
        );
        std::fs::rename(&approved_root, temporary.path().join("retired-root")).unwrap();
        authority
            .confirm_legacy_project(
                &pairing,
                &phone.device_id,
                &grant.grant_id,
                grant.grant_revision,
                &review.scope_digest,
                &project_id,
                false,
            )
            .unwrap();
        drop(authority);
        let authority = Authority::open(&path, &receiver).unwrap();
        let state = authority.lock().unwrap();
        let restored = &state.grants[&phone.device_id];
        assert_eq!(restored.legacy_project_confirmed(&project_id), Ok(true));
        assert_eq!(
            restored.legacy_project_confirmed(&"04".repeat(16)),
            Ok(false)
        );
        assert_eq!(
            state.grants[&second_phone.device_id].legacy_project_confirmed(&project_id),
            Ok(false)
        );
        drop(state);
        let second_review = authority
            .inspect_grant(&pairing, &second_phone.device_id)
            .unwrap();
        authority.disable(&second_phone.device_id).unwrap();
        assert_eq!(
            authority.confirm_legacy_project(
                &pairing,
                &second_phone.device_id,
                &second_review.grant_id,
                second_review.grant_revision,
                &second_review.scope_digest,
                &project_id,
                false,
            ),
            Err(Refusal::Revoked)
        );
        drop(authority);
        let mut persisted: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        persisted["grants"][&phone.device_id]["confirmed_project_ids"][0] = "04".repeat(16).into();
        std::fs::write(&path, serde_json::to_vec(&persisted).unwrap()).unwrap();
        let authority = Authority::open(&path, &receiver).unwrap();
        let state = authority.lock().unwrap();
        assert_eq!(
            state.grants[&phone.device_id].legacy_project_confirmed(&project_id),
            Err(Refusal::Unavailable)
        );
        drop(state);
        assert_eq!(
            authority.confirm_legacy_project(
                &pairing,
                &phone.device_id,
                &grant.grant_id,
                grant.grant_revision,
                &review.scope_digest,
                &project_id,
                false,
            ),
            Err(Refusal::Unavailable)
        );
        let challenge = authority
            .challenge(
                &phone.device_id,
                &grant.grant_id,
                grant.grant_revision,
                now,
                1_000,
            )
            .unwrap();
        let lease = authority
            .unlock(
                &phone.device_id,
                &signed_proof(&grant, &challenge),
                now,
                1_000,
            )
            .unwrap();
        let request = signed_request(&grant, &lease, 0, 1234);
        let permit = authority
            .begin_read(&phone.device_id, &request, now)
            .unwrap();
        assert!(authority.finish_read(&permit, true, now, 1_000).is_ok());
    }

    #[test]
    fn history_polling_cannot_renew_the_working_period() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let receiver = "01".repeat(32);
        let authority = Authority::open(&temporary.path().join("grants"), &receiver).unwrap();
        let grant = approved(&authority, temporary.path(), now);
        let lease = unlocked(&authority, &grant, now);
        let store = ActionEvidenceStore::open(temporary.path().join("history")).unwrap();
        let original = ActionIdentity {
            operation_id: "11".repeat(16),
            receiver_identity: receiver,
            controller_id: "12".repeat(16),
            subject_id: "13".repeat(16),
            action: 3,
            payload_hash: "14".repeat(32),
            grant_id: grant.grant_id.clone(),
            grant_revision: grant.grant_revision,
            period_id: lease.period_id.clone(),
            expected_revision: 1,
            project: std::fs::canonicalize(temporary.path())
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            agent: "codex".into(),
            recorded_at_ms: 1_000,
        };
        store.reserve_fixture(&original).unwrap();
        let first = signed_history(&grant, &lease, &original, 300);
        assert!(matches!(
            authority.read_history(&store, "phone", &first, now + Duration::from_secs(299)),
            Ok(History::Record(record)) if record.identity == original
        ));
        let second = signed_history(&grant, &lease, &original, 301);
        assert_eq!(
            authority.read_history(&store, "phone", &second, now + Duration::from_secs(300)),
            Err(Refusal::Expired)
        );
    }

    #[test]
    fn history_polling_does_not_renew_the_sleep_aware_idle_deadline() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let (authority, ticks) = clocked_authority(temporary.path());
        let grant = approved(&authority, temporary.path(), now);
        let lease = unlocked(&authority, &grant, now);
        let store = ActionEvidenceStore::open(temporary.path().join("history")).unwrap();
        let original = ActionIdentity {
            operation_id: "11".repeat(16),
            receiver_identity: "01".repeat(32),
            controller_id: "12".repeat(16),
            subject_id: "13".repeat(16),
            action: 3,
            payload_hash: "14".repeat(32),
            grant_id: grant.grant_id.clone(),
            grant_revision: grant.grant_revision,
            period_id: lease.period_id.clone(),
            expected_revision: 1,
            project: std::fs::canonicalize(temporary.path())
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            agent: "codex".into(),
            recorded_at_ms: 1_000,
        };
        store.reserve_fixture(&original).unwrap();
        ticks.store(10_000 + 299 * 100, Ordering::SeqCst);
        assert!(matches!(
            authority.read_history(
                &store,
                "phone",
                &signed_history(&grant, &lease, &original, 1_100),
                now
            ),
            Ok(History::Record(_))
        ));
        ticks.store(10_000 + 300 * 100, Ordering::SeqCst);
        assert_eq!(
            authority.read_history(
                &store,
                "phone",
                &signed_history(&grant, &lease, &original, 1_101),
                now
            ),
            Err(Refusal::Expired)
        );
    }

    #[test]
    fn enrollment_requires_current_offer_and_explicit_approval() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("grants.json");
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
        let now = Instant::now();
        let authority = Authority::open(&path, &"01".repeat(32)).unwrap();
        assert_eq!(
            authority.challenge("phone", "missing", 1, now, 1_000),
            Err(Refusal::Revoked)
        );
        let offer = authority.start("phone", now, 1_000).unwrap();
        assert_eq!(offer.matching_code.len(), 8);
        assert_eq!(
            authority.submit(
                "other",
                &offer.enrollment_id,
                &offer.matching_code,
                key,
                now
            ),
            Err(Refusal::Unauthenticated)
        );
        authority
            .submit(
                "phone",
                &offer.enrollment_id,
                &offer.matching_code,
                key,
                now,
            )
            .unwrap();
        assert_eq!(
            authority.challenge("phone", "missing", 1, now, 1_000),
            Err(Refusal::Revoked)
        );
        let scope = Scope {
            projects: vec![temporary.path().to_str().unwrap().to_owned()],
            agents: vec!["codex".into()],
            groups: vec!["read".into()],
        };
        let grant = authority
            .approve("phone", &offer.enrollment_id, key, scope, &[], now)
            .unwrap();
        assert_ne!(grant.grant_revision, 0);
        assert!(
            authority
                .challenge("phone", &grant.grant_id, grant.grant_revision, now, 1_000)
                .is_ok()
        );
        drop(authority);
        let reopened = Authority::open(&path, &"01".repeat(32)).unwrap();
        assert!(
            reopened
                .challenge(
                    "phone",
                    &grant.grant_id,
                    grant.grant_revision,
                    now + Duration::from_secs(1),
                    2_000
                )
                .is_ok()
        );
    }

    #[test]
    fn offers_expire_are_replaced_and_stop_after_five_failures() {
        let temporary = tempfile::tempdir().unwrap();
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
        let now = Instant::now();
        let authority =
            Authority::open(&temporary.path().join("grants"), &"01".repeat(32)).unwrap();
        let old = authority.start("phone", now, 1_000).unwrap();
        let current = authority.start("phone", now, 1_000).unwrap();
        assert_eq!(
            authority.submit("phone", &old.enrollment_id, &old.matching_code, key, now),
            Err(Refusal::Unauthenticated)
        );
        let incorrect = if current.matching_code == "00000000" {
            "99999999"
        } else {
            "00000000"
        };
        for _ in 0..4 {
            assert_eq!(
                authority.submit("phone", &current.enrollment_id, incorrect, key, now),
                Err(Refusal::Unauthenticated)
            );
        }
        assert_eq!(
            authority.submit(
                "phone",
                &current.enrollment_id,
                &current.matching_code,
                key,
                now
            ),
            Err(Refusal::Unauthenticated)
        );
        let later = authority.start("phone", now, 1_000).unwrap();
        assert_eq!(
            authority.submit(
                "phone",
                &later.enrollment_id,
                &later.matching_code,
                key,
                now + Duration::from_secs(120)
            ),
            Err(Refusal::Expired)
        );
    }

    #[test]
    fn malformed_enrollment_keys_and_codes_have_closed_refusals() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let authority =
            Authority::open(&temporary.path().join("grants"), &"01".repeat(32)).unwrap();
        let offer = authority.start("phone", now, 1_000).unwrap();
        assert_eq!(
            authority.submit(
                "phone",
                &offer.enrollment_id,
                &offer.matching_code,
                "not-a-key",
                now
            ),
            Err(Refusal::Malformed)
        );
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
        assert_eq!(
            authority.submit("phone", &offer.enrollment_id, "not-a-code", key, now),
            Err(Refusal::Malformed)
        );
    }

    #[test]
    fn cancelling_an_offer_cannot_approve_it_and_preserves_existing_grant() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let authority =
            Authority::open(&temporary.path().join("grants"), &"01".repeat(32)).unwrap();
        let grant = approved(&authority, temporary.path(), now);
        let offer = authority.start("phone", now, 1_000).unwrap();
        authority.cancel("phone", &offer.enrollment_id).unwrap();
        assert_eq!(
            authority
                .enrollment("phone", Some(&offer.enrollment_id), now)
                .err(),
            Some(Refusal::Unauthenticated)
        );
        assert!(
            authority
                .challenge("phone", &grant.grant_id, grant.grant_revision, now, 1_000)
                .is_ok()
        );
    }

    #[test]
    fn unlock_consumes_only_the_current_unexpired_signed_challenge() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let authority =
            Authority::open(&temporary.path().join("grants"), &"01".repeat(32)).unwrap();
        let grant = approved(&authority, temporary.path(), now);
        let first = authority
            .challenge("phone", &grant.grant_id, grant.grant_revision, now, 1_000)
            .unwrap();
        let stale = signed_proof(&grant, &first);
        let current = authority
            .challenge("phone", &grant.grant_id, grant.grant_revision, now, 1_000)
            .unwrap();
        assert_eq!(
            authority.unlock("phone", &stale, now, 1_000),
            Err(Refusal::Unauthenticated)
        );
        let proof = signed_proof(&grant, &current);
        let mut forged = signed_proof(&grant, &current);
        let last = forged.signature.pop().unwrap();
        forged.signature.push(if last == '0' { '1' } else { '0' });
        assert_eq!(
            authority.unlock("phone", &forged, now, 1_000),
            Err(Refusal::Unauthenticated)
        );
        let mut malformed = signed_proof(&grant, &current);
        malformed.challenge_id = "invalid".into();
        assert_eq!(
            authority.unlock("phone", &malformed, now, 1_000),
            Err(Refusal::Malformed)
        );
        let lease = authority.unlock("phone", &proof, now, 1_000).unwrap();
        assert_eq!(lease.idle_expires_at_ms, 301_000);
        assert_eq!(lease.absolute_expires_at_ms, 3_601_000);
        assert_eq!(
            authority.unlock("phone", &proof, now, 1_000),
            Err(Refusal::Unauthenticated)
        );
        let timed = authority
            .challenge("phone", &grant.grant_id, grant.grant_revision, now, 1_000)
            .unwrap();
        let timed_proof = signed_proof(&grant, &timed);
        assert_eq!(
            authority.unlock(
                "phone",
                &timed_proof,
                now + Duration::from_secs(120),
                121_000
            ),
            Err(Refusal::Expired)
        );
    }

    #[test]
    fn preflight_checks_pending_lease_without_consuming_or_renewing_it() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let authority =
            Authority::open(&temporary.path().join("grants"), &"01".repeat(32)).unwrap();
        let grant = approved(&authority, temporary.path(), now);
        let lease = unlocked(&authority, &grant, now);
        let request = signed_request(&grant, &lease, 1, 444);
        let permit = authority
            .begin_read("phone", &request, now + Duration::from_secs(299))
            .unwrap();
        let mut state = authority.lock().unwrap();
        let deadline = state.periods["phone"].idle_deadline;
        authority
            .check_pending_read_locked(
                &mut state,
                &permit,
                now + Duration::from_secs(299),
                Instant::now(),
            )
            .unwrap();
        assert_eq!(state.periods["phone"].idle_deadline, deadline);
        assert!(state.pending_reads.contains(&permit.operation_id));
        drop(state);
        assert_eq!(
            authority.begin_read(
                "phone",
                &signed_request(&grant, &lease, 1, 445),
                now + Duration::from_secs(300)
            ),
            Err(Refusal::Expired)
        );
    }

    #[test]
    fn only_successful_interactions_refresh_idle_and_absolute_limit_never_moves() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let authority =
            Authority::open(&temporary.path().join("grants"), &"01".repeat(32)).unwrap();
        let grant = approved(&authority, temporary.path(), now);
        let lease = unlocked(&authority, &grant, now);
        let background = signed_request(&grant, &lease, 0, 1);
        let permit = authority
            .begin_read("phone", &background, now + Duration::from_secs(299))
            .unwrap();
        authority
            .finish_read(&permit, true, now + Duration::from_secs(299), 300_000)
            .unwrap();
        assert_eq!(
            authority.finish_read(&permit, true, now + Duration::from_secs(299), 300_000),
            Err(Refusal::Unauthenticated)
        );
        assert_eq!(
            authority.begin_read(
                "phone",
                &signed_request(&grant, &lease, 1, 2),
                now + Duration::from_secs(300)
            ),
            Err(Refusal::Expired)
        );

        let lease = unlocked(&authority, &grant, now);
        let failed = signed_request(&grant, &lease, 1, 3);
        let permit = authority
            .begin_read("phone", &failed, now + Duration::from_secs(299))
            .unwrap();
        assert_eq!(
            authority.finish_read(&permit, false, now + Duration::from_secs(299), 300_000),
            Err(Refusal::Unavailable)
        );
        assert_eq!(
            authority.begin_read(
                "phone",
                &signed_request(&grant, &lease, 1, 4),
                now + Duration::from_secs(300)
            ),
            Err(Refusal::Expired)
        );

        let lease = unlocked(&authority, &grant, now);
        for i in 1..=12_u64 {
            let elapsed = i * 299;
            let request = signed_request(&grant, &lease, 1, u128::from(100 + i));
            let permit = authority
                .begin_read("phone", &request, now + Duration::from_secs(elapsed))
                .unwrap();
            authority
                .finish_read(
                    &permit,
                    true,
                    now + Duration::from_secs(elapsed),
                    1_000 + elapsed * 1_000,
                )
                .unwrap();
        }
        assert_eq!(
            authority.begin_read(
                "phone",
                &signed_request(&grant, &lease, 1, 200),
                now + Duration::from_secs(3_600)
            ),
            Err(Refusal::Expired)
        );
    }

    #[test]
    fn suspended_time_expires_an_idle_period_before_protected_read() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let (authority, ticks) = clocked_authority(temporary.path());
        let grant = approved(&authority, temporary.path(), now);
        let lease = unlocked(&authority, &grant, now);
        ticks.store(10_000 + 300 * 100, Ordering::SeqCst);
        assert!(matches!(
            authority.begin_read("phone", &signed_request(&grant, &lease, 0, 901), now),
            Err(Refusal::Expired)
        ));
    }

    #[test]
    fn suspended_time_expires_in_flight_delivery() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let (authority, ticks) = clocked_authority(temporary.path());
        let grant = approved(&authority, temporary.path(), now);
        let lease = unlocked(&authority, &grant, now);
        let permit = authority
            .begin_read("phone", &signed_request(&grant, &lease, 1, 902), now)
            .unwrap();
        ticks.store(10_000 + 300 * 100, Ordering::SeqCst);
        assert_eq!(
            authority.finish_read(&permit, true, now, 1_000),
            Err(Refusal::Expired)
        );
    }

    #[test]
    fn delayed_delivery_does_not_extend_idle_past_the_phone_interaction() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let (authority, ticks) = clocked_authority(temporary.path());
        let grant = approved(&authority, temporary.path(), now);
        let lease = unlocked(&authority, &grant, now);
        ticks.store(10_000 + 100 * 100, Ordering::SeqCst);
        let permit = authority
            .begin_read("phone", &signed_request(&grant, &lease, 1, 903), now)
            .unwrap();
        ticks.store(10_000 + 200 * 100, Ordering::SeqCst);
        authority.finish_read(&permit, true, now, 201_000).unwrap();
        let next = authority
            .begin_read("phone", &signed_request(&grant, &lease, 0, 905), now)
            .unwrap();
        let frame: Value = serde_json::from_slice(&next.frame).unwrap();
        assert_eq!(frame["period"]["idle_expires_at_ms"], 400_990);
        ticks.store(10_000 + 400 * 100, Ordering::SeqCst);
        assert!(matches!(
            authority.begin_read("phone", &signed_request(&grant, &lease, 0, 904), now),
            Err(Refusal::Expired)
        ));
    }

    #[test]
    fn older_delayed_interaction_cannot_shorten_a_newer_idle_deadline() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let (authority, ticks) = clocked_authority(temporary.path());
        let grant = approved(&authority, temporary.path(), now);
        let lease = unlocked(&authority, &grant, now);
        ticks.store(10_000 + 100 * 100, Ordering::SeqCst);
        let earlier = authority
            .begin_read("phone", &signed_request(&grant, &lease, 1, 920), now)
            .unwrap();
        ticks.store(10_000 + 150 * 100, Ordering::SeqCst);
        let later = authority
            .begin_read("phone", &signed_request(&grant, &lease, 1, 921), now)
            .unwrap();
        ticks.store(10_000 + 200 * 100, Ordering::SeqCst);
        authority.finish_read(&later, true, now, 201_000).unwrap();
        ticks.store(10_000 + 220 * 100, Ordering::SeqCst);
        authority.finish_read(&earlier, true, now, 221_000).unwrap();
        ticks.store(10_000 + 440 * 100, Ordering::SeqCst);
        let next = authority
            .begin_read("phone", &signed_request(&grant, &lease, 0, 922), now)
            .unwrap();
        let frame: Value = serde_json::from_slice(&next.frame).unwrap();
        assert_eq!(frame["period"]["idle_expires_at_ms"], 450_990);
        ticks.store(10_000 + 450 * 100, Ordering::SeqCst);
        assert!(matches!(
            authority.begin_read("phone", &signed_request(&grant, &lease, 0, 923), now),
            Err(Refusal::Expired)
        ));
    }

    #[test]
    fn unsupported_sleep_clock_cannot_start_a_working_period() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let authority = Authority::open_with_clock(
            &temporary.path().join("grants"),
            &"01".repeat(32),
            Arc::new(|| {
                Err(std::io::Error::new(
                    std::io::ErrorKind::Unsupported,
                    "unsupported",
                ))
            }),
        )
        .unwrap();
        let grant = approved(&authority, temporary.path(), now);
        let challenge = authority
            .challenge("phone", &grant.grant_id, grant.grant_revision, now, 1_000)
            .unwrap();
        assert_eq!(
            authority.unlock("phone", &signed_proof(&grant, &challenge), now, 1_000),
            Err(Refusal::Unsupported)
        );
    }

    #[test]
    fn suspended_time_counts_toward_absolute_cap_despite_interaction() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let (authority, ticks) = clocked_authority(temporary.path());
        let grant = approved(&authority, temporary.path(), now);
        let lease = unlocked(&authority, &grant, now);
        for step in 1..=12_u64 {
            ticks.store(10_000 + step * 299 * 100, Ordering::SeqCst);
            let permit = authority
                .begin_read(
                    "phone",
                    &signed_request(&grant, &lease, 1, u128::from(910 + step)),
                    now,
                )
                .unwrap();
            authority.finish_read(&permit, true, now, 1_000).unwrap();
        }
        ticks.store(10_000 + 3_600 * 100, Ordering::SeqCst);
        assert!(matches!(
            authority.begin_read("phone", &signed_request(&grant, &lease, 1, 999), now),
            Err(Refusal::Expired)
        ));
    }

    #[test]
    fn unavailable_rewinding_or_changed_namespace_clock_cannot_authorize_reads() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let current = Arc::new(Mutex::new(Some(super::sleep_clock::Sample {
            centiseconds: 10_000,
            namespace: "time:[1]".into(),
        })));
        let source = Arc::clone(&current);
        let authority = Authority::open_with_clock(
            &temporary.path().join("grants"),
            &"01".repeat(32),
            Arc::new(move || {
                source
                    .lock()
                    .unwrap()
                    .clone()
                    .ok_or_else(|| std::io::Error::other("unavailable"))
            }),
        )
        .unwrap();
        let grant = approved(&authority, temporary.path(), now);
        let lease = unlocked(&authority, &grant, now);
        current.lock().unwrap().as_mut().unwrap().centiseconds = 11_000;
        authority
            .begin_read("phone", &signed_request(&grant, &lease, 0, 1_001), now)
            .unwrap();
        current.lock().unwrap().as_mut().unwrap().centiseconds = 10_999;
        assert!(matches!(
            authority.begin_read("phone", &signed_request(&grant, &lease, 0, 1_002), now),
            Err(Refusal::Unavailable)
        ));
        *current.lock().unwrap() = Some(super::sleep_clock::Sample {
            centiseconds: 11_001,
            namespace: "time:[2]".into(),
        });
        assert!(matches!(
            authority.begin_read("phone", &signed_request(&grant, &lease, 0, 1_003), now),
            Err(Refusal::Unavailable)
        ));
        *current.lock().unwrap() = None;
        assert!(matches!(
            authority.begin_read("phone", &signed_request(&grant, &lease, 0, 1_004), now),
            Err(Refusal::Unavailable)
        ));
    }

    #[test]
    fn signed_reads_resist_tampering_replay_and_revocation_races() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let authority =
            Authority::open(&temporary.path().join("grants"), &"01".repeat(32)).unwrap();
        let grant = approved(&authority, temporary.path(), now);
        let challenge = authority
            .challenge("phone", &grant.grant_id, grant.grant_revision, now, 1_000)
            .unwrap();
        let old_proof = signed_proof(&grant, &challenge);
        let lease = authority.unlock("phone", &old_proof, now, 1_000).unwrap();
        let request = signed_request(&grant, &lease, 1, 1);
        let mut malformed = signed_request(&grant, &lease, 1, 3);
        malformed.grant_id = "invalid".into();
        assert_eq!(
            authority.begin_read("phone", &malformed, now),
            Err(Refusal::Malformed)
        );
        let mut forged = signed_request(&grant, &lease, 0, 2);
        forged.interaction = 1;
        assert_eq!(
            authority.begin_read("phone", &forged, now),
            Err(Refusal::Unauthenticated)
        );
        let permit = authority.begin_read("phone", &request, now).unwrap();
        assert_eq!(
            super::super::verify_phone_control_frame(&permit.frame, &grant.receiver_identity),
            Ok(())
        );
        assert_eq!(
            authority.begin_read("phone", &request, now),
            Err(Refusal::Unauthenticated)
        );
        authority.disable("phone").unwrap();
        assert_eq!(
            authority.finish_read(&permit, true, now, 1_000),
            Err(Refusal::Revoked)
        );
        assert_eq!(
            authority.begin_read("phone", &request, now),
            Err(Refusal::Revoked)
        );
        drop(authority);
        let authority =
            Authority::open(&temporary.path().join("grants"), &"01".repeat(32)).unwrap();
        let replacement = approved(&authority, temporary.path(), now);
        assert_ne!(grant.grant_id, replacement.grant_id);
        assert!(replacement.grant_revision > grant.grant_revision);
        assert_eq!(
            authority.unlock("phone", &old_proof, now, 1_000),
            Err(Refusal::Unauthenticated)
        );
        assert_eq!(
            authority.begin_read("phone", &request, now),
            Err(Refusal::Revoked)
        );
    }

    #[test]
    fn other_permission_groups_do_not_imply_read_access() {
        let temporary = tempfile::tempdir().unwrap();
        let now = Instant::now();
        let authority =
            Authority::open(&temporary.path().join("grants"), &"01".repeat(32)).unwrap();
        let fixture: Value = serde_json::from_str(FIXTURE).unwrap();
        let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
        let offer = authority.start("phone", now, 1_000).unwrap();
        authority
            .submit(
                "phone",
                &offer.enrollment_id,
                &offer.matching_code,
                key,
                now,
            )
            .unwrap();
        let grant = authority
            .approve(
                "phone",
                &offer.enrollment_id,
                key,
                Scope {
                    projects: vec![temporary.path().to_str().unwrap().into()],
                    agents: vec!["codex".into()],
                    groups: vec![
                        "operate_sessions".into(),
                        "control_runs".into(),
                        "saved_results".into(),
                    ],
                },
                &[],
                now,
            )
            .unwrap();
        let lease = unlocked(&authority, &grant, now);
        let request = signed_request(&grant, &lease, 1, 1);
        let mut forged = signed_request(&grant, &lease, 0, 2);
        forged.interaction = 1;
        assert_eq!(
            authority.begin_read("phone", &forged, now),
            Err(Refusal::Unauthenticated)
        );
        assert_eq!(
            authority.begin_read("phone", &request, now),
            Err(Refusal::OutOfScope)
        );
    }

    #[cfg(unix)]
    #[test]
    fn uncertain_grant_persistence_seals_the_running_authority() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let directory = temporary.path().join("grants");
        let now = Instant::now();
        let authority = Authority::open(&directory.join("state"), &"01".repeat(32)).unwrap();
        let grant = approved(&authority, temporary.path(), now);
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o500)).unwrap();
        assert_eq!(authority.disable("phone"), Err(Refusal::Unavailable));
        assert_eq!(
            authority.challenge("phone", &grant.grant_id, grant.grant_revision, now, 1_000),
            Err(Refusal::Unavailable)
        );
        std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}
