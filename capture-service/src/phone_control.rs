//! Authentication of the frozen phone-control controller frame.

mod authority;
mod http;
mod socket;

use std::{collections::HashSet, io, path::Path, sync::Arc};

use aws_lc_rs::signature::{ECDSA_P256_SHA256_ASN1, ParsedPublicKey};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::PairingRegistry;

pub(crate) struct Service {
    authority: Arc<authority::Authority>,
    pairing: Arc<PairingRegistry>,
    controllers: Arc<socket::ControllerRegistry>,
}

impl Service {
    pub(crate) fn open(
        path: &Path,
        receiver: &str,
        pairing: Arc<PairingRegistry>,
    ) -> Result<Self, io::Error> {
        let authority = authority::Authority::open(path, receiver)
            .map_err(|_| io::Error::other("phone-control authority is unavailable"))?;
        Ok(Self {
            authority: Arc::new(authority),
            pairing,
            controllers: Arc::default(),
        })
    }

    pub(crate) fn router(&self) -> axum::Router {
        http::router(
            self.authority.clone(),
            self.pairing.clone(),
            self.controllers.clone(),
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
        )
        .await?;
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
        Some("read_session") => {}
        Some(_) => return Err(Unsupported),
        None => return Err(Malformed),
    }
    let frame: Frame = serde_json::from_slice(input).map_err(|_| Malformed)?;
    if frame.v != 1 || frame.kind != "read_session" {
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
        validate_request_shape(request)?;
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
        let payload = fixed::<10>(&request.payload)?;
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
        request_bytes.push(1);
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
    if request.grant_revision == 0
        || request.grant_revision > MAX_INTEGER
        || request.expected_revision > MAX_INTEGER
        || request.action > MAX_INTEGER
        || request.interaction > MAX_INTEGER
    {
        return Err(PhoneControlError::Malformed);
    }
    if request.action != 1 || request.interaction > 1 {
        return Err(PhoneControlError::Unsupported);
    }
    fixed::<16>(&request.grant_id)?;
    fixed::<16>(&request.period_id)?;
    fixed::<32>(&request.receiver_identity)?;
    fixed::<16>(&request.controller_id)?;
    fixed::<16>(&request.subject_id)?;
    fixed::<32>(&request.payload_hash)?;
    fixed::<16>(&request.operation_id)?;
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
