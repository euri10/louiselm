//! Stream a signed recording before atomically authorizing its voice job.

use std::{path::PathBuf, sync::Arc, time::Instant};

use axum::{
    Json,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode, header},
};
use serde::Deserialize;
use serde_json::{Value, json};

use super::super::{
    Request,
    authority::{ReadPermit, Refusal},
    socket::ControllerRegistry,
    voice_job::{VoiceJobCommand, VoiceOwnerScope, voice_job_command, voice_source_digest},
};
use super::{HttpState, SignedRequest, decode, outcome};
use crate::{CaptureDraft, CaptureSource, StoreError, VoiceArchive, receiver::stream_body};

enum UploadError {
    Refusal(Refusal),
    TooLarge,
}
impl From<Refusal> for UploadError {
    fn from(value: Refusal) -> Self {
        Self::Refusal(value)
    }
}

pub(super) async fn upload(
    State(state): State<HttpState>,
    headers: HeaderMap,
    body: Body,
) -> (StatusCode, Json<Value>) {
    let result = tokio::spawn(process(state, headers, body))
        .await
        .unwrap_or(Err(UploadError::Refusal(Refusal::Unavailable)));
    match result {
        Ok(value) => outcome(Ok(value)),
        Err(UploadError::Refusal(error)) => outcome(Err(error)),
        Err(UploadError::TooLarge) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            Json(json!({"v":1,"type":"error","code":"too_large"})),
        ),
    }
}

async fn process(state: HttpState, headers: HeaderMap, body: Body) -> Result<Value, UploadError> {
    let authorization =
        one_header(&headers, header::AUTHORIZATION.as_str()).ok_or(Refusal::Unauthenticated)?;
    let credential = authorization
        .strip_prefix("Bearer ")
        .filter(|value| !value.is_empty())
        .ok_or(Refusal::Unauthenticated)?
        .to_owned();
    let pairing = state.pairing.clone();
    let check_credential = credential.clone();
    let device =
        tokio::task::spawn_blocking(move || pairing.authenticate_device(&check_credential))
            .await
            .map_err(|_| Refusal::Unavailable)?
            .map_err(|_| Refusal::Unavailable)?
            .ok_or(Refusal::Unauthenticated)?;
    if !state.voice_transcription_enabled {
        return Err(Refusal::Unsupported.into());
    }
    let root = state.voice_root.clone().ok_or(Refusal::Unsupported)?;
    let encoded = one_header(&headers, "x-louiselm-voice-request").ok_or(Refusal::Malformed)?;
    if encoded.len() > 8192
        || one_header(&headers, header::CONTENT_TYPE.as_str()) != Some("audio/mp4")
    {
        return Err(Refusal::Malformed.into());
    }
    let signed: SignedRequest = decode(encoded.as_bytes(), "request")?;
    let command = voice_job_command(&signed.request).map_err(super::super::authority::refusal)?;
    let VoiceJobCommand::Admit {
        id,
        digest,
        recorded_at_ms,
        duration_ms,
        bytes,
        ..
    } = command
    else {
        return Err(Refusal::Unsupported.into());
    };
    if let Some(length) = one_header(&headers, header::CONTENT_LENGTH.as_str()) {
        let length = length.parse::<u64>().map_err(|_| Refusal::Malformed)?;
        if length != bytes {
            return Err(Refusal::Malformed.into());
        }
    } else if headers.contains_key(header::CONTENT_LENGTH) {
        return Err(Refusal::Malformed.into());
    }
    let authority = state.authority.clone();
    let request = signed.request;
    let initial_request = request.clone();
    let permit = tokio::task::spawn_blocking(move || {
        authority.begin_read(&device, &initial_request, Instant::now())
    })
    .await
    .map_err(|_| Refusal::Unavailable)??;
    let result = upload_permitted(
        &state,
        &credential,
        &request,
        &permit,
        root,
        body,
        CaptureDraft {
            id,
            source: CaptureSource::Android,
            recorded_at_ms,
            duration_ms,
            mime_type: "audio/mp4".into(),
        },
        digest,
        bytes,
    )
    .await;
    let authority = state.authority.clone();
    let discarded = tokio::task::spawn_blocking(move || authority.discard_voice_permit(&permit))
        .await
        .map_err(|_| Refusal::Unavailable)?;
    discarded?;
    result
}

#[expect(
    clippy::too_many_arguments,
    reason = "The signed metadata, permit and owned stream stay explicit at the upload boundary."
)]
async fn upload_permitted(
    state: &HttpState,
    credential: &str,
    request: &Request,
    permit: &ReadPermit,
    root: PathBuf,
    body: Body,
    draft: CaptureDraft,
    digest: String,
    bytes: u64,
) -> Result<Value, UploadError> {
    let first_scope = owner_scope(&state.controllers, request).await?;
    let VoiceJobCommand::Admit { source_digest, .. } =
        voice_job_command(request).map_err(super::super::authority::refusal)?
    else {
        return Err(Refusal::Malformed.into());
    };
    if voice_source_digest(&first_scope.project, &first_scope.agent)
        .map_err(super::super::authority::refusal)?
        != source_digest
    {
        return Err(Refusal::OutOfScope.into());
    }
    let path = root.join(format!(".upload-{}", uuid::Uuid::new_v4()));
    let archive = tokio::task::spawn_blocking(move || VoiceArchive::new(root))
        .await
        .map_err(|_| Refusal::Unavailable)?
        .map_err(|_| Refusal::Unavailable)?;
    let result = async {
        let (received, actual_digest) = stream_body(body, &path).await.map_err(|error| {
            if error.status == StatusCode::PAYLOAD_TOO_LARGE {
                UploadError::TooLarge
            } else if error.status == StatusCode::BAD_REQUEST {
                UploadError::Refusal(Refusal::Malformed)
            } else {
                UploadError::Refusal(Refusal::Unavailable)
            }
        })?;
        if received != bytes || actual_digest != digest {
            return Err(Refusal::Malformed.into());
        }
        let current_scope = owner_scope(&state.controllers, request).await?;
        if current_scope != first_scope {
            return Err(Refusal::StaleSubject.into());
        }
        let staged_archive = archive.clone();
        let staged_draft = draft.clone();
        let staged_path = path.clone();
        tokio::task::spawn_blocking(move || {
            let audio = std::fs::File::open(staged_path)?;
            staged_archive.stage(&staged_draft, audio)
        })
        .await
        .map_err(|_| Refusal::Unavailable)?
        .map_err(|error| match error {
            StoreError::TooLarge { .. } => UploadError::TooLarge,
            StoreError::Conflict(_) | StoreError::InvalidCapture(_) => {
                UploadError::Refusal(Refusal::Malformed)
            }
            _ => UploadError::Refusal(Refusal::Unavailable),
        })?;
        let final_scope = owner_scope(&state.controllers, request).await?;
        if final_scope != first_scope {
            return Err(Refusal::StaleSubject.into());
        }
        let authority = state.authority.clone();
        let pairing = state.pairing.clone();
        let controller = state.controllers.clone();
        let controller_id = request.controller_id.clone();
        let credential = credential.to_owned();
        let permit = permit.clone();
        tokio::task::spawn_blocking(move || {
            authority.finish_voice_admission(
                &permit,
                &pairing,
                &credential,
                &archive,
                final_scope,
                || controller.contains(&controller_id),
            )
        })
        .await
        .map_err(|_| Refusal::Unavailable)?
        .map_err(UploadError::from)
    }
    .await;
    match tokio::fs::remove_file(path).await {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(_) => return Err(Refusal::Unavailable.into()),
    }
    result
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeReply {
    v: u64,
    #[serde(rename = "type")]
    kind: String,
    controller_id: String,
    subject_id: String,
    operation_id: String,
    project: String,
    agent: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScopeError {
    v: u64,
    #[serde(rename = "type")]
    kind: String,
    code: String,
}

async fn owner_scope(
    controllers: &Arc<ControllerRegistry>,
    request: &Request,
) -> Result<VoiceOwnerScope, Refusal> {
    let probe = serde_json::to_vec(
        &json!({"v":1,"type":"action_scope", "controller_id":request.controller_id,
        "subject_id":request.subject_id,"operation_id":request.operation_id}),
    )
    .map_err(|_| Refusal::Unavailable)?;
    let response = controllers.dispatch(&request.controller_id, probe).await?;
    if response.len() >= super::super::MAX_FRAME {
        return Err(Refusal::Unavailable);
    }
    if let Ok(error) = serde_json::from_slice::<ScopeError>(&response) {
        return Err(
            if error.v == 1 && error.kind == "error" && error.code == "stale_subject" {
                Refusal::StaleSubject
            } else {
                Refusal::Unavailable
            },
        );
    }
    let reply: ScopeReply = serde_json::from_slice(&response).map_err(|_| Refusal::Unavailable)?;
    if reply.v != 1
        || reply.kind != "action_scope"
        || reply.controller_id != request.controller_id
        || reply.subject_id != request.subject_id
        || reply.operation_id != request.operation_id
    {
        return Err(Refusal::StaleSubject);
    }
    voice_source_digest(&reply.project, &reply.agent).map_err(super::super::authority::refusal)?;
    Ok(VoiceOwnerScope {
        project: reply.project,
        agent: reply.agent,
    })
}

fn one_header<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    let mut values = headers.get_all(name).iter();
    let value = values.next()?.to_str().ok()?;
    if values.next().is_some() || value.is_empty() {
        return None;
    }
    Some(value)
}
