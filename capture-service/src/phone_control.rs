//! Authentication of the frozen phone-control controller frame.

mod authority;
mod barrier;
mod barrier_log;
mod discovery;
mod evidence;
mod http;
#[cfg(test)]
mod legacy_recovery_tests;
pub(crate) mod owner_journal;
mod prepared_owner;
#[cfg(test)]
mod prepared_owner_tests;
mod prepared_selection;
mod run_probe;
mod saved_review;
mod socket;
mod source;
mod source_checkout;
#[cfg(test)]
mod source_checkout_tests;
#[cfg(test)]
mod source_tests;

pub use evidence::{
    ActionEvidence, ActionEvidenceError, ActionEvidenceKind, ActionEvidenceStore, ActionIdentity,
    ActionOutcome, ActionReservation,
};

use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    sync::Arc,
};

use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1, ParsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::{PairingRegistry, RunStore};

pub(crate) fn offline_forget(
    authority_path: &Path,
    pairing_path: &Path,
    tls: &Path,
    receiver: &str,
    device_id: &str,
) -> Result<(), ()> {
    let (generation, binding) = evidence::receiver_barrier_state(
        &tls.join("phone-action-evidence.json"),
        receiver,
        ActionEvidenceKind::Archive,
    )
    .map_err(|_| ())?;
    let barrier = barrier::Barrier::existing(tls, receiver, binding).map_err(|_| ())?;
    barrier
        .offline_cutoff(
            &generation,
            barrier_log::CutoffKind::Forget,
            device_id,
            || {
                let pairing = PairingRegistry::open(pairing_path).map_err(|_| ())?;
                let authority =
                    authority::Authority::open(authority_path, receiver).map_err(|_| ())?;
                authority.forget(&pairing, device_id).map_err(|_| ())
            },
        )
        .map_err(|error| match error {
            barrier::CutoffError::Barrier(_error) => (),
            barrier::CutoffError::Operation(()) => (),
        })
}

pub(crate) struct Service {
    authority: Arc<authority::Authority>,
    pairing: Arc<PairingRegistry>,
    controllers: Arc<socket::ControllerRegistry>,
    evidence: Arc<Result<ActionEvidenceStore, ActionEvidenceError>>,
    state_root: PathBuf,
    prepared: Option<prepared_owner::PreparedOwner>,
    authority_available: bool,
}

impl Service {
    pub(crate) fn open(
        path: &Path,
        receiver: &str,
        pairing: Arc<PairingRegistry>,
    ) -> Result<Self, io::Error> {
        let (authority, authority_available) = match authority::Authority::open(path, receiver) {
            Ok(authority) => (authority, true),
            Err(authority::Refusal::Unavailable) => (
                authority::Authority::unavailable(path, receiver)
                    .map_err(|_| io::Error::other("phone-control authority is unavailable"))?,
                false,
            ),
            Err(_) => return Err(io::Error::other("phone-control authority is unavailable")),
        };
        let state_root = path
            .parent()
            .ok_or_else(|| io::Error::other("phone-control state has no parent"))?;
        let evidence_path = state_root.join("action-evidence");
        let guard_path = state_root
            .parent()
            .ok_or_else(|| io::Error::other("receiver identity state has no parent"))?
            .join("tls/phone-action-evidence.json");
        let evidence = ActionEvidenceStore::open_bound(
            evidence_path,
            guard_path,
            receiver,
            ActionEvidenceKind::Archive,
        );
        let registration = evidence
            .as_ref()
            .ok()
            .and_then(|store| {
                Some((
                    store.bound_generation()?.to_owned(),
                    store.bound_barrier()?.clone(),
                ))
            })
            .and_then(|(generation, binding)| {
                barrier::Barrier::existing(
                    state_root.parent().map(|root| root.join("tls"))?.as_path(),
                    receiver,
                    binding,
                )
                .ok()?
                .receiver(&generation)
                .ok()
                .map(|barrier| (generation, barrier))
            });
        let controllers = match registration {
            Some(registration) => socket::ControllerRegistry::new(Some(registration)),
            None => socket::ControllerRegistry::default(),
        };
        Ok(Self {
            authority: Arc::new(authority),
            pairing,
            controllers: Arc::new(controllers),
            evidence: Arc::new(evidence),
            state_root: state_root.to_owned(),
            prepared: None,
            authority_available,
        })
    }

    pub(crate) fn authority_available(&self) -> bool {
        self.authority_available
    }

    pub(crate) fn with_runs(mut self, runs: Option<RunStore>) -> Self {
        self.prepared =
            runs.map(|runs| prepared_owner::PreparedOwner::new(self.state_root.clone(), runs));
        self
    }

    pub(crate) fn router(&self) -> axum::Router {
        http::router(
            self.authority.clone(),
            self.pairing.clone(),
            self.controllers.clone(),
            self.evidence.clone(),
            self.prepared
                .as_ref()
                .map(prepared_owner::PreparedOwner::reader),
        )
    }

    pub(crate) async fn bind(
        &self,
        admin_path: &Path,
        controller_path: &Path,
        capability: &Path,
    ) -> io::Result<Sockets> {
        let admin = socket::AdminSocket::bind(
            admin_path,
            capability,
            self.authority.clone(),
            self.pairing.clone(),
            self.controllers.clone(),
        )
        .await?
        .with_prepared(self.prepared.clone());
        let controller =
            socket::ControllerSocket::bind(controller_path, capability, self.controllers.clone())
                .await?;
        Ok(Sockets { admin, controller })
    }
}

pub(crate) struct Sockets {
    admin: socket::AdminSocket,
    controller: socket::ControllerSocket,
}

impl Sockets {
    pub(crate) async fn serve(self) -> io::Result<()> {
        tokio::select! {
            result = self.admin.serve() => result,
            result = self.controller.serve() => result,
        }
    }
}

pub(crate) const MAX_FRAME: usize = 262_144;
const MAX_INTEGER: u64 = 9_007_199_254_740_991;
const DOMAIN: &[u8] = b"louiselm-phone-control\0";
const P256_ORDER: [u8; 32] = [
    0xff, 0xff, 0xff, 0xff, 0x00, 0x00, 0x00, 0x00, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff,
    0xbc, 0xe6, 0xfa, 0xad, 0xa7, 0x17, 0x9e, 0x84, 0xf3, 0xb9, 0xca, 0xc2, 0xfc, 0x63, 0x25, 0x51,
];

/// Closed, payload-free refusal from phone-control verification.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PhoneControlError {
    /// The signed data, key, or receiver binding did not authenticate.
    #[error("unauthenticated")]
    Unauthenticated,
    /// The frame or a cryptographic encoding is malformed.
    #[error("malformed")]
    Malformed,
    /// The version, message type, action, or enum is unsupported.
    #[error("unsupported")]
    Unsupported,
    /// The local receiver identity is unavailable.
    #[error("unavailable")]
    Unavailable,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Frame {
    v: u64,
    #[serde(rename = "type")]
    kind: String,
    controller_id: String,
    grant: GrantRecord,
    period: PeriodRecord,
    proof: Proof,
    request: Request,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct GrantRecord {
    grant_id: String,
    grant_revision: u64,
    enrolled_key: String,
    projects: Vec<String>,
    agents: Vec<String>,
    groups: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PeriodRecord {
    period_id: String,
    challenge_id: String,
    idle_expires_at_ms: u64,
    absolute_expires_at_ms: u64,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Proof {
    grant_id: String,
    grant_revision: u64,
    enrolled_key_hash: String,
    receiver_identity: String,
    challenge_id: String,
    challenge: String,
    working_key: String,
    signature: String,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    grant_id: String,
    grant_revision: u64,
    period_id: String,
    receiver_identity: String,
    controller_id: String,
    subject_id: String,
    action: u64,
    payload: String,
    payload_hash: String,
    operation_id: String,
    expected_revision: u64,
    interaction: u64,
    signature: String,
}

/// Verify both signatures and their bindings in one controller read frame.
///
/// `receiver_identity` must be the locally obtained SHA-256 identity of the
/// receiver, never an identity supplied by the phone or frame. This verifies
/// authentication only: the caller must independently check live grant and
/// period state, replay, subject, scope, and admission before any delivery.
/// The function performs no I/O and does not consume a challenge or operation.
///
/// # Errors
///
/// Returns a closed refusal for malformed, unsupported, or unauthenticated input.
pub fn verify_phone_control_frame(
    input: &[u8],
    receiver_identity: &str,
) -> Result<(), PhoneControlError> {
    use PhoneControlError::{Malformed, Unsupported};

    if input.len() > MAX_FRAME {
        return Err(Malformed);
    }
    // Preflight the version and type before deserializing the versioned body;
    // the closed struct below still rejects duplicate fields at every level.
    let header: serde_json::Value = serde_json::from_slice(input).map_err(|_| Malformed)?;
    match header.get("v").and_then(serde_json::Value::as_u64) {
        Some(version) if version > MAX_INTEGER => return Err(Malformed),
        Some(1) => {}
        Some(_) => return Err(Unsupported),
        None => return Err(Malformed),
    }
    match header.get("type").and_then(serde_json::Value::as_str) {
        Some(kind) if !valid_string(kind) => return Err(Malformed),
        Some(
            "read_session" | "send_prompt" | "list_controllers" | "list_sessions" | "list_prepared"
            | "read_prepared" | "list_runs" | "read_run" | "read_run_live" | "read_run_session"
            | "read_saved_review",
        ) => {}
        Some(_) => return Err(Unsupported),
        None => return Err(Malformed),
    }
    let frame: Frame = serde_json::from_slice(input).map_err(|_| Malformed)?;
    if frame.request.action > MAX_INTEGER {
        return Err(Malformed);
    }
    if frame.v != 1
        || !matches!(
            (frame.kind.as_str(), frame.request.action),
            ("read_session", 1)
                | ("send_prompt", 3)
                | ("list_controllers", 48)
                | ("list_sessions", 49)
                | ("list_prepared", 52)
                | ("read_prepared", 53)
                | ("list_runs", 54)
                | ("read_run", 55)
                | ("read_run_live", 56)
                | ("read_run_session", 57)
                | ("read_saved_review", 80)
        )
    {
        return Err(Unsupported);
    }
    frame.verify(receiver_identity)
}

impl Frame {
    fn verify(&self, receiver_identity: &str) -> Result<(), PhoneControlError> {
        use PhoneControlError::{Malformed, Unauthenticated, Unsupported};
        let grant = &self.grant;
        let proof = &self.proof;
        let request = &self.request;
        validate_proof_shape(proof)?;
        match request.action {
            1 => validate_request_shape(request)?,
            2 => {
                validate_history_request_shape(request)?;
            }
            3 => validate_send_request_shape(request)?,
            48 | 49 => validate_discovery_request_shape(request)?,
            52 => validate_prepared_list_request_shape(request)?,
            53 => validate_prepared_request_shape(request)?,
            54 => validate_run_list_request_shape(request)?,
            55 | 56 => validate_run_request_shape(request)?,
            57 => {
                run_session_query(request)?;
            }
            80 => {
                saved_review_query(request)?;
            }
            _ => return Err(Unsupported),
        }
        for number in [
            grant.grant_revision,
            self.period.idle_expires_at_ms,
            self.period.absolute_expires_at_ms,
        ] {
            if number > MAX_INTEGER {
                return Err(Malformed);
            }
        }
        if grant.grant_revision == 0 {
            return Err(Malformed);
        }
        validate_list(&grant.projects)?;
        validate_list(&grant.agents)?;
        validate_list(&grant.groups)?;
        if grant.groups.iter().any(|group| {
            !matches!(
                group.as_str(),
                "read" | "operate_sessions" | "control_runs" | "saved_results"
            )
        }) {
            return Err(Unsupported);
        }
        let grant_id = fixed::<16>(&grant.grant_id)?;
        let enrolled_key = key(&grant.enrolled_key)?;
        let expected_receiver = fixed::<32>(receiver_identity)?;
        let proof_receiver = fixed::<32>(&proof.receiver_identity)?;
        let request_receiver = fixed::<32>(&request.receiver_identity)?;
        let challenge_id = fixed::<16>(&proof.challenge_id)?;
        let working_key = key(&proof.working_key)?;
        let period_id = fixed::<16>(&self.period.period_id)?;
        let controller_id = fixed::<16>(&self.controller_id)?;
        let subject_id = fixed::<16>(&request.subject_id)?;
        let payload_hash = fixed::<32>(&request.payload_hash)?;
        let operation_id = fixed::<16>(&request.operation_id)?;
        let payload = decode_hex(&request.payload)?;
        let request_signature = signature(&request.signature)?;
        if grant_id != fixed::<16>(&proof.grant_id)?
            || grant_id != fixed::<16>(&request.grant_id)?
            || grant.grant_revision != proof.grant_revision
            || grant.grant_revision != request.grant_revision
            || expected_receiver != proof_receiver
            || expected_receiver != request_receiver
            || challenge_id != fixed::<16>(&self.period.challenge_id)?
            || period_id != fixed::<16>(&request.period_id)?
            || controller_id != fixed::<16>(&request.controller_id)?
            || payload_hash.as_slice() != Sha256::digest(payload).as_slice()
        {
            return Err(Unauthenticated);
        }
        verify_proof(proof, &enrolled_key)?;

        let mut request_bytes = prefix(2);
        request_bytes.extend(grant_id);
        request_bytes.extend(request.grant_revision.to_be_bytes());
        request_bytes.extend(period_id);
        request_bytes.extend(request_receiver);
        request_bytes.extend(controller_id);
        request_bytes.extend(subject_id);
        request_bytes.push(u8::try_from(request.action).map_err(|_| Malformed)?);
        request_bytes.extend(payload_hash);
        request_bytes.extend(operation_id);
        request_bytes.extend(request.expected_revision.to_be_bytes());
        request_bytes.push(u8::try_from(request.interaction).map_err(|_| Malformed)?);
        check_signature(&working_key, &request_bytes, &request_signature)
    }
}

fn validate_proof_shape(proof: &Proof) -> Result<(), PhoneControlError> {
    if proof.grant_revision == 0 || proof.grant_revision > MAX_INTEGER {
        return Err(PhoneControlError::Malformed);
    }
    fixed::<16>(&proof.grant_id)?;
    fixed::<32>(&proof.enrolled_key_hash)?;
    fixed::<32>(&proof.receiver_identity)?;
    fixed::<16>(&proof.challenge_id)?;
    fixed::<32>(&proof.challenge)?;
    key(&proof.working_key)?;
    signature(&proof.signature)?;
    Ok(())
}

fn validate_request_shape(request: &Request) -> Result<(), PhoneControlError> {
    validate_common_request_shape(request)?;
    if request.action != 1 || request.interaction > 1 {
        return Err(PhoneControlError::Unsupported);
    }
    let payload = fixed::<10>(&request.payload)?;
    let before = u64::from_be_bytes(
        payload[..8]
            .try_into()
            .map_err(|_| PhoneControlError::Malformed)?,
    );
    let limit = u16::from_be_bytes(
        payload[8..]
            .try_into()
            .map_err(|_| PhoneControlError::Malformed)?,
    );
    if before > MAX_INTEGER || !(1..=50).contains(&limit) {
        return Err(PhoneControlError::Malformed);
    }
    Ok(())
}

fn validate_send_request_shape(request: &Request) -> Result<(), PhoneControlError> {
    validate_common_request_shape(request)?;
    if request.action != 3 {
        return Err(PhoneControlError::Unsupported);
    }
    if request.interaction != 1
        || request.expected_revision == 0
        || fixed::<16>(&request.controller_id)? == [0; 16]
        || fixed::<16>(&request.subject_id)? == [0; 16]
    {
        return Err(PhoneControlError::Malformed);
    }
    let payload = decode_hex(&request.payload)?;
    match payload.first() {
        Some(1) => {}
        Some(_) => return Err(PhoneControlError::Unsupported),
        None => return Err(PhoneControlError::Malformed),
    }
    let text = std::str::from_utf8(&payload[1..]).map_err(|_| PhoneControlError::Malformed)?;
    if text.is_empty() || text.contains('\0') {
        return Err(PhoneControlError::Malformed);
    }
    Ok(())
}

fn validate_discovery_request_shape(request: &Request) -> Result<(), PhoneControlError> {
    validate_common_request_shape(request)?;
    if !matches!(request.action, 48 | 49) || request.interaction > 1 {
        return Err(PhoneControlError::Unsupported);
    }
    let payload = fixed::<17>(&request.payload)?;
    let controller = fixed::<16>(&request.controller_id)?;
    if !(1..=50).contains(&payload[16])
        || fixed::<16>(&request.subject_id)? != [0; 16]
        || request.expected_revision != 0
        || (request.action == 48 && controller != [0; 16])
        || (request.action == 49 && controller == [0; 16])
    {
        return Err(PhoneControlError::Malformed);
    }
    Ok(())
}

fn validate_prepared_request_shape(request: &Request) -> Result<(), PhoneControlError> {
    validate_common_request_shape(request)?;
    if request.action != 53 || request.interaction > 1 {
        return Err(PhoneControlError::Unsupported);
    }
    if fixed::<16>(&request.controller_id)? != [0; 16]
        || fixed::<16>(&request.subject_id)? == [0; 16]
        || request.expected_revision == 0
        || !request.payload.is_empty()
    {
        return Err(PhoneControlError::Malformed);
    }
    Ok(())
}

fn validate_prepared_list_request_shape(request: &Request) -> Result<(), PhoneControlError> {
    validate_common_request_shape(request)?;
    if request.action != 52 || request.interaction > 1 {
        return Err(PhoneControlError::Unsupported);
    }
    if fixed::<16>(&request.controller_id)? != [0; 16]
        || fixed::<16>(&request.subject_id)? != [0; 16]
        || request.expected_revision != 0
    {
        return Err(PhoneControlError::Malformed);
    }
    fixed::<16>(&request.payload)?;
    Ok(())
}

fn validate_run_list_request_shape(request: &Request) -> Result<(), PhoneControlError> {
    validate_common_request_shape(request)?;
    if request.action != 54 || request.interaction > 1 {
        return Err(PhoneControlError::Unsupported);
    }
    let cursor = fixed::<17>(&request.payload)?;
    if fixed::<16>(&request.controller_id)? != [0; 16]
        || fixed::<16>(&request.subject_id)? != [0; 16]
        || request.expected_revision != 0
        || cursor[0] > 1
        || (cursor[0] == 0 && cursor[1..] != [0; 16])
    {
        return Err(PhoneControlError::Malformed);
    }
    Ok(())
}

fn validate_run_request_shape(request: &Request) -> Result<(), PhoneControlError> {
    validate_common_request_shape(request)?;
    if !matches!(request.action, 55 | 56) || request.interaction > 1 {
        return Err(PhoneControlError::Unsupported);
    }
    if fixed::<16>(&request.controller_id)? != [0; 16] || !request.payload.is_empty() {
        return Err(PhoneControlError::Malformed);
    }
    Ok(())
}

struct SavedReviewQuery {
    run_id: String,
    result_id: String,
    revision: Option<String>,
    file: u32,
    side: u8,
    offset: u32,
}

fn saved_review_query(request: &Request) -> Result<SavedReviewQuery, PhoneControlError> {
    validate_common_request_shape(request)?;
    if request.action != 80 || request.interaction > 1 {
        return Err(PhoneControlError::Unsupported);
    }
    if fixed::<16>(&request.controller_id)? != [0; 16]
        || fixed::<16>(&request.subject_id)? == [0; 16]
        || request.expected_revision != 0
        || !(88..=342).contains(&request.payload.len())
    {
        return Err(PhoneControlError::Malformed);
    }
    let payload = decode_hex(&request.payload)?;
    let name_len = usize::from(payload[1]);
    if payload[0] != 1 || !(1..=128).contains(&name_len) || payload.len() != 43 + name_len {
        return Err(PhoneControlError::Malformed);
    }
    if !payload[2..2 + name_len]
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err(PhoneControlError::Malformed);
    }
    let cursor = &payload[2 + name_len..];
    let file = u32::from_be_bytes(
        cursor[32..36]
            .try_into()
            .map_err(|_| PhoneControlError::Malformed)?,
    );
    let offset = u32::from_be_bytes(
        cursor[37..41]
            .try_into()
            .map_err(|_| PhoneControlError::Malformed)?,
    );
    if file > 10_000
        || cursor[36] > 1
        || offset > 128 * 1024 * 1024
        || (cursor[..32] == [0; 32] && cursor[32..] != [0; 9])
    {
        return Err(PhoneControlError::Malformed);
    }
    let result_id = std::str::from_utf8(&payload[2..2 + name_len])
        .map_err(|_| PhoneControlError::Malformed)?
        .to_owned();
    Ok(SavedReviewQuery {
        run_id: uuid::Uuid::from_bytes(fixed::<16>(&request.subject_id)?).to_string(),
        result_id,
        revision: (cursor[..32] != [0; 32])
            .then(|| format!("sha256:{}", crate::hex::lower_hex(&cursor[..32]))),
        file,
        side: cursor[36],
        offset,
    })
}

struct RunSessionQuery {
    run_id: String,
    run_revision: u64,
    before: u64,
    limit: u16,
}

fn run_session_query(request: &Request) -> Result<RunSessionQuery, PhoneControlError> {
    validate_common_request_shape(request)?;
    if request.action != 57 || request.interaction > 1 {
        return Err(PhoneControlError::Unsupported);
    }
    let payload = fixed::<51>(&request.payload)?;
    let run: [u8; 16] = payload[1..17]
        .try_into()
        .map_err(|_| PhoneControlError::Malformed)?;
    let owner: [u8; 16] = payload[17..33]
        .try_into()
        .map_err(|_| PhoneControlError::Malformed)?;
    let run_revision = u64::from_be_bytes(
        payload[33..41]
            .try_into()
            .map_err(|_| PhoneControlError::Malformed)?,
    );
    let before = u64::from_be_bytes(
        payload[41..49]
            .try_into()
            .map_err(|_| PhoneControlError::Malformed)?,
    );
    let limit = u16::from_be_bytes(
        payload[49..51]
            .try_into()
            .map_err(|_| PhoneControlError::Malformed)?,
    );
    if payload[0] != 1
        || run == [0; 16]
        || owner == [0; 16]
        || fixed::<16>(&request.controller_id)? == [0; 16]
        || fixed::<16>(&request.subject_id)? == [0; 16]
        || !(1..=MAX_INTEGER).contains(&run_revision)
        || before > MAX_INTEGER
        || !(1..=50).contains(&limit)
    {
        return Err(PhoneControlError::Malformed);
    }
    Ok(RunSessionQuery {
        run_id: uuid::Uuid::from_bytes(run).to_string(),
        run_revision,
        before,
        limit,
    })
}

enum HistoryQuery {
    One(String),
    Page { after: Option<String>, limit: usize },
}

fn validate_history_request_shape(request: &Request) -> Result<HistoryQuery, PhoneControlError> {
    validate_common_request_shape(request)?;
    if request.action != 2 {
        return Err(PhoneControlError::Unsupported);
    }
    // History checks are observation, never a working-period renewal.
    if request.interaction != 0 || request.expected_revision != 0 {
        return Err(PhoneControlError::Malformed);
    }
    let payload = decode_hex(&request.payload)?;
    let query = match payload.first() {
        Some(0) if payload.len() == 17 => HistoryQuery::One(crate::hex::lower_hex(&payload[1..])),
        Some(1) if payload.len() == 2 => HistoryQuery::Page {
            after: None,
            limit: usize::from(payload[1]),
        },
        Some(2) if payload.len() == 18 => HistoryQuery::Page {
            after: Some(crate::hex::lower_hex(&payload[1..17])),
            limit: usize::from(payload[17]),
        },
        Some(0..=2) | None => return Err(PhoneControlError::Malformed),
        Some(_) => return Err(PhoneControlError::Unsupported),
    };
    if let HistoryQuery::Page { limit, .. } = &query
        && (!(1..=50).contains(limit)
            || fixed::<16>(&request.controller_id)? != [0; 16]
            || fixed::<16>(&request.subject_id)? != [0; 16])
    {
        return Err(PhoneControlError::Malformed);
    }
    Ok(query)
}

fn validate_common_request_shape(request: &Request) -> Result<(), PhoneControlError> {
    if request.grant_revision == 0
        || request.grant_revision > MAX_INTEGER
        || request.expected_revision > MAX_INTEGER
        || request.action > MAX_INTEGER
        || request.interaction > MAX_INTEGER
    {
        return Err(PhoneControlError::Malformed);
    }
    fixed::<16>(&request.grant_id)?;
    fixed::<16>(&request.period_id)?;
    fixed::<32>(&request.receiver_identity)?;
    fixed::<16>(&request.controller_id)?;
    fixed::<16>(&request.subject_id)?;
    fixed::<32>(&request.payload_hash)?;
    fixed::<16>(&request.operation_id)?;
    signature(&request.signature)?;
    Ok(())
}

fn verify_proof(proof: &Proof, enrolled_key: &[u8; 65]) -> Result<(), PhoneControlError> {
    let enrolled_hash = fixed::<32>(&proof.enrolled_key_hash)?;
    if enrolled_hash.as_slice() != Sha256::digest(enrolled_key).as_slice() {
        return Err(PhoneControlError::Unauthenticated);
    }
    let mut bytes = prefix(1);
    bytes.extend(fixed::<16>(&proof.grant_id)?);
    bytes.extend(proof.grant_revision.to_be_bytes());
    bytes.extend(enrolled_hash);
    bytes.extend(fixed::<32>(&proof.receiver_identity)?);
    bytes.extend(fixed::<16>(&proof.challenge_id)?);
    bytes.extend(fixed::<32>(&proof.challenge)?);
    bytes.extend(key(&proof.working_key)?);
    check_signature(enrolled_key, &bytes, &signature(&proof.signature)?)
}

fn prefix(kind: u8) -> Vec<u8> {
    let mut bytes = DOMAIN.to_vec();
    bytes.extend([1, kind]);
    bytes
}

fn valid_string(value: &str) -> bool {
    value.len() <= 8192 && !value.contains('\0')
}

fn validate_list(values: &[String]) -> Result<(), PhoneControlError> {
    let mut unique = HashSet::new();
    if values.len() > 50
        || values
            .iter()
            .any(|value| !valid_string(value) || !unique.insert(value))
    {
        return Err(PhoneControlError::Malformed);
    }
    Ok(())
}

fn fixed<const N: usize>(encoded: &str) -> Result<[u8; N], PhoneControlError> {
    let bytes = decode_hex(encoded)?;
    bytes.try_into().map_err(|_| PhoneControlError::Malformed)
}

fn decode_hex(encoded: &str) -> Result<Vec<u8>, PhoneControlError> {
    if encoded.len() > 8192 || !encoded.len().is_multiple_of(2) {
        return Err(PhoneControlError::Malformed);
    }
    encoded
        .as_bytes()
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| {
            let digit = |byte| match byte {
                b'0'..=b'9' => Ok(byte - b'0'),
                b'a'..=b'f' => Ok(byte - b'a' + 10),
                _ => Err(PhoneControlError::Malformed),
            };
            Ok(digit(pair[0])? << 4 | digit(pair[1])?)
        })
        .collect()
}

fn key(encoded: &str) -> Result<[u8; 65], PhoneControlError> {
    let bytes = fixed::<65>(encoded)?;
    if bytes[0] != 4 || ParsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, bytes).is_err() {
        return Err(PhoneControlError::Malformed);
    }
    Ok(bytes)
}

fn signature(encoded: &str) -> Result<Vec<u8>, PhoneControlError> {
    let bytes = decode_hex(encoded)?;
    if bytes.len() > 72
        || bytes.len() < 8
        || bytes[0] != 0x30
        || usize::from(bytes[1]) != bytes.len() - 2
    {
        return Err(PhoneControlError::Malformed);
    }
    let mut rest = &bytes[2..];
    for _ in 0..2 {
        if rest.len() < 3 || rest[0] != 0x02 {
            return Err(PhoneControlError::Malformed);
        }
        let length = usize::from(rest[1]);
        if length == 0 || length > 33 || rest.len() < length + 2 {
            return Err(PhoneControlError::Malformed);
        }
        let mut scalar = &rest[2..length + 2];
        if scalar[0] & 0x80 != 0 || (scalar.len() > 1 && scalar[0] == 0 && scalar[1] & 0x80 == 0) {
            return Err(PhoneControlError::Malformed);
        }
        if scalar[0] == 0 {
            scalar = &scalar[1..];
        }
        if scalar.is_empty()
            || scalar.len() > 32
            || (scalar.len() == 32 && scalar >= P256_ORDER.as_slice())
        {
            return Err(PhoneControlError::Malformed);
        }
        rest = &rest[length + 2..];
    }
    if !rest.is_empty() {
        return Err(PhoneControlError::Malformed);
    }
    Ok(bytes)
}

fn check_signature(
    key: &[u8; 65],
    message: &[u8],
    signature: &[u8],
) -> Result<(), PhoneControlError> {
    ParsedPublicKey::new(&ECDSA_P256_SHA256_ASN1, key)
        .map_err(|_| PhoneControlError::Malformed)?
        .verify_sig(message, signature)
        .map_err(|_| PhoneControlError::Unauthenticated)
}
