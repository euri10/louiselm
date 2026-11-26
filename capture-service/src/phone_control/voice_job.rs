//! Closed signed payloads for capture-owned voice jobs.

use super::{MAX_INTEGER, PhoneControlError, Request, fixed, validate_common_request_shape};
use crate::{MAX_CAPTURE_BYTES, hex::lower_hex};
use sha2::{Digest, Sha256};

#[derive(Debug, Eq, PartialEq)]
pub(super) struct VoiceOwnerScope {
    pub(super) project: String,
    pub(super) agent: String,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) enum VoiceJobCommand {
    Admit {
        id: String,
        digest: String,
        recorded_at_ms: u64,
        duration_ms: u64,
        bytes: u64,
        draft: String,
        revision: String,
        source_digest: String,
    },
    Cancel {
        id: String,
    },
    Retry {
        id: String,
        retry_id: String,
    },
}

pub(super) fn voice_job_command(request: &Request) -> Result<VoiceJobCommand, PhoneControlError> {
    use PhoneControlError::{Malformed, Unsupported};
    validate_common_request_shape(request)?;
    if request.expected_revision != 0
        || request.interaction > 1
        || fixed::<16>(&request.controller_id)? == [0; 16]
        || fixed::<16>(&request.subject_id)? == [0; 16]
        || fixed::<16>(&request.operation_id)? == [0; 16]
    {
        return Err(Malformed);
    }
    match request.action {
        83 => {
            if request.interaction != 0 {
                return Err(Malformed);
            }
            let payload = fixed::<169>(&request.payload)?;
            let id = uuid::Uuid::from_bytes(payload[1..17].try_into().map_err(|_| Malformed)?)
                .to_string();
            let recorded_at_ms =
                u64::from_be_bytes(payload[49..57].try_into().map_err(|_| Malformed)?);
            let duration_ms =
                u64::from_be_bytes(payload[57..65].try_into().map_err(|_| Malformed)?);
            let bytes = u64::from_be_bytes(payload[65..73].try_into().map_err(|_| Malformed)?);
            if payload[0] != 1
                || !(1..=MAX_INTEGER).contains(&recorded_at_ms)
                || !(1..=MAX_INTEGER).contains(&duration_ms)
                || !(1..=MAX_CAPTURE_BYTES).contains(&bytes)
            {
                return Err(Malformed);
            }
            Ok(VoiceJobCommand::Admit {
                id,
                digest: lower_hex(&payload[17..49]),
                recorded_at_ms,
                duration_ms,
                bytes,
                draft: lower_hex(&payload[73..105]),
                revision: lower_hex(&payload[105..137]),
                source_digest: lower_hex(&payload[137..169]),
            })
        }
        84 => {
            let id = uuid::Uuid::from_bytes(fixed::<16>(&request.payload)?).to_string();
            Ok(VoiceJobCommand::Cancel { id })
        }
        85 => {
            let payload = fixed::<32>(&request.payload)?;
            let id = uuid::Uuid::from_bytes(payload[..16].try_into().map_err(|_| Malformed)?)
                .to_string();
            if payload[16..] == [0; 16] {
                return Err(Malformed);
            }
            Ok(VoiceJobCommand::Retry {
                id,
                retry_id: lower_hex(&payload[16..]),
            })
        }
        _ => Err(Unsupported),
    }
}

pub(super) fn voice_source_digest(project: &str, agent: &str) -> Result<String, PhoneControlError> {
    use PhoneControlError::Malformed;
    if project.is_empty()
        || agent.is_empty()
        || project.len() > 512
        || agent.len() > 512
        || project.contains('\0')
        || agent.contains('\0')
    {
        return Err(Malformed);
    }
    let project_len = u16::try_from(project.len()).map_err(|_| Malformed)?;
    let agent_len = u16::try_from(agent.len()).map_err(|_| Malformed)?;
    let mut hash = Sha256::new();
    hash.update(b"louiselm-voice-source\0");
    hash.update(project_len.to_be_bytes());
    hash.update(project.as_bytes());
    hash.update(agent_len.to_be_bytes());
    hash.update(agent.as_bytes());
    Ok(lower_hex(&hash.finalize()))
}
