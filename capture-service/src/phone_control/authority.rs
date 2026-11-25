//! Durable grants and ephemeral phone-control authority.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, File, OpenOptions},
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard},
    time::{Duration, Instant},
};

use aws_lc_rs::rand::{SecureRandom, SystemRandom};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use super::{
    ActionEvidence, ActionEvidenceStore, Frame, GrantRecord, HistoryQuery, MAX_FRAME, MAX_INTEGER,
    PeriodRecord, PhoneControlError, Proof, Request, fixed, key, validate_history_request_shape,
    validate_list, validate_proof_shape, validate_request_shape, verify_phone_control_frame,
    verify_proof,
};
use crate::{PairingRegistry, hex::lower_hex, permissions::set_private_permissions};

const OFFER_LIFETIME: Duration = Duration::from_secs(120);
const IDLE_LIFETIME: Duration = Duration::from_secs(300);
const ABSOLUTE_LIFETIME: Duration = Duration::from_secs(3_600);
const MAX_STORED_BYTES: u64 = 1_048_576;

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
    Granted(StoredGrant),
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
}

#[derive(Debug, Eq, PartialEq)]
pub(super) struct ReadPermit {
    device: String,
    grant_id: String,
    period_id: String,
    operation_id: String,
    interaction: bool,
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
    state: Mutex<State>,
}

impl Authority {
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
            let stored: StoredState =
                serde_json::from_slice(&bytes).map_err(|_| Refusal::Unavailable)?;
            if stored
                .revisions
                .values()
                .any(|revision| *revision == 0 || *revision > MAX_INTEGER)
            {
                return Err(Refusal::Unavailable);
            }
            for (device, grant) in &stored.grants {
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
            }
            stored
        } else {
            StoredState::default()
        };
        Ok(Self {
            path: path.to_owned(),
            receiver_identity: receiver_identity.to_owned(),
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
        now: Instant,
    ) -> Result<StoredGrant, Refusal> {
        let started = Instant::now();
        validate_scope(&scope)?;
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
        let grant = StoredGrant {
            device_id: device.to_owned(),
            enrollment_id: enrollment_id.to_owned(),
            grant_id: random_hex(16)?,
            grant_revision: revision,
            enrolled_key: enrolled_key.to_owned(),
            receiver_identity: self.receiver_identity.clone(),
            scope,
        };
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
        validate_proof_shape(proof).map_err(refusal)?;
        let mut state = self.lock()?;
        let grant = state.grants.get(device).ok_or(Refusal::Revoked)?;
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
        state.challenges.remove(device);
        state.periods.insert(
            device.to_owned(),
            WorkingLease {
                public: public.clone(),
                proof: proof.clone(),
                idle_deadline,
                absolute_deadline,
            },
        );
        Ok(public)
    }

    pub(super) fn begin_read(
        &self,
        device: &str,
        request: &Request,
        now: Instant,
    ) -> Result<ReadPermit, Refusal> {
        let started = Instant::now();
        if request.action == 1 {
            validate_request_shape(request).map_err(refusal)?;
        } else {
            super::validate_discovery_request_shape(request).map_err(refusal)?;
        }
        let mut state = self.lock()?;
        let grant = state.grants.get(device).ok_or(Refusal::Revoked)?;
        if grant.grant_id != request.grant_id
            || grant.grant_revision != request.grant_revision
            || grant.receiver_identity != self.receiver_identity
        {
            return Err(Refusal::Revoked);
        }
        let lease = state.periods.get(device).ok_or(Refusal::Unauthenticated)?;
        if lease.public.period_id != request.period_id {
            return Err(Refusal::Unauthenticated);
        }
        let checked_now = elapsed_now(now, started)?;
        if checked_now >= lease.idle_deadline || checked_now >= lease.absolute_deadline {
            return Err(Refusal::Expired);
        }
        let frame = Frame {
            v: 1,
            kind: match request.action {
                1 => "read_session",
                48 => "list_controllers",
                49 => "list_sessions",
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
        if !grant.scope.groups.iter().any(|group| group == "read") {
            return Err(Refusal::OutOfScope);
        }
        if !state.operations.insert(request.operation_id.clone()) {
            return Err(Refusal::Unauthenticated);
        }
        state.pending_reads.insert(request.operation_id.clone());
        Ok(ReadPermit {
            device: device.to_owned(),
            grant_id: request.grant_id.clone(),
            period_id: request.period_id.clone(),
            operation_id: request.operation_id.clone(),
            interaction: request.interaction == 1,
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
        if !success {
            return Err(Refusal::Unavailable);
        }
        if permit.interaction {
            lease.idle_deadline = now
                .checked_add(IDLE_LIFETIME)
                .ok_or(Refusal::Unavailable)?
                .min(lease.absolute_deadline);
            lease.public.idle_expires_at_ms =
                expiry_ms(wall_ms, IDLE_LIFETIME)?.min(lease.public.absolute_expires_at_ms);
        }
        Ok(())
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
        let grant = state.grants.get(device).ok_or(Refusal::Revoked)?;
        if grant.grant_id != request.grant_id
            || grant.grant_revision != request.grant_revision
            || grant.receiver_identity != self.receiver_identity
        {
            return Err(Refusal::Revoked);
        }
        let lease = state.periods.get(device).ok_or(Refusal::Unauthenticated)?;
        if lease.public.period_id != request.period_id {
            return Err(Refusal::Unauthenticated);
        }
        let checked_now = elapsed_now(now, started)?;
        if checked_now >= lease.idle_deadline || checked_now >= lease.absolute_deadline {
            return Err(Refusal::Expired);
        }
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
        let lease = state.periods.get(device).ok_or(Refusal::Revoked)?;
        let checked_now = elapsed_now(now, started)?;
        if checked_now >= lease.idle_deadline || checked_now >= lease.absolute_deadline {
            return Err(Refusal::Expired);
        }
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
            return Ok(EnrollmentStatus::Granted(grant.clone()));
        }
        Err(Refusal::Unauthenticated)
    }

    pub(super) fn receiver_identity(&self) -> &str {
        &self.receiver_identity
    }

    pub(super) fn forget(&self, pairing: &PairingRegistry, device: &str) -> Result<(), Refusal> {
        let mut state = self.lock()?;
        if state.grants.contains_key(device) {
            let mut grants = state.grants.clone();
            grants.remove(device);
            let revisions = state.revisions.clone();
            self.persist_or_seal(&mut state, &grants, &revisions)?;
            state.grants = grants;
        }
        state.enrollments.remove(device);
        state.challenges.remove(device);
        state.periods.remove(device);
        pairing.revoke(device).map_err(|_| Refusal::Unavailable)
    }
}

fn elapsed_now(observed: Instant, started: Instant) -> Result<Instant, Refusal> {
    observed
        .checked_add(started.elapsed())
        .ok_or(Refusal::Unavailable)
}

fn refusal(error: PhoneControlError) -> Refusal {
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
        time::{Duration, Instant},
    };

    use aws_lc_rs::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
    };
    use serde_json::Value;
    use sha2::{Digest, Sha256};

    use super::super::{ActionEvidenceStore, ActionIdentity, Proof, Request, fixed, prefix};
    use super::{Authority, History, IssuedChallenge, Lease, Refusal, Scope, StoredGrant};
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
                now,
            )
            .unwrap()
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
        store.reserve(&original).unwrap();
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
            .approve("phone", &offer.enrollment_id, key, scope, now)
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
