use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    Json, Router,
    body::Body,
    extract::{DefaultBodyLimit, Path as RoutePath, State},
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::{get, post, put},
};
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use uuid::Uuid;

use crate::{
    CaptureDraft, CaptureSource, IngestOutcome, MAX_CAPTURE_BYTES, PairingRegistry, Store,
    StoreError,
};

#[derive(Clone)]
struct ReceiverState {
    store: Store,
    pairing: Arc<PairingRegistry>,
    uploads: PathBuf,
}

/// Authenticated HTTP boundary for pairing devices and ingesting recordings.
pub struct Receiver {
    state: ReceiverState,
}

impl Receiver {
    /// Construct a receiver around an existing store and pairing registry.
    ///
    /// # Errors
    ///
    /// Returns an I/O error when the streaming upload directory cannot be created.
    pub fn new(
        store: Store,
        pairing: Arc<PairingRegistry>,
        uploads: impl AsRef<Path>,
    ) -> Result<Self, std::io::Error> {
        fs::create_dir_all(uploads.as_ref())?;
        set_private_permissions(uploads.as_ref(), true)?;
        Ok(Self {
            state: ReceiverState {
                store,
                pairing,
                uploads: uploads.as_ref().to_path_buf(),
            },
        })
    }

    /// Build the receiver routes without binding a socket.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/v1/health", get(health))
            .route("/v1/pair", post(pair))
            .route("/v1/captures/{id}", put(upload))
            // JSON extractors stay small; streamed audio applies its own 20 MiB limit.
            .layer(DefaultBodyLimit::max(8 * 1024))
            .with_state(self.state.clone())
    }
}

#[derive(Serialize)]
struct HealthResponse {
    status: &'static str,
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok" })
}

#[derive(Deserialize)]
struct PairRequest {
    token: String,
    device_name: String,
}

async fn pair(
    State(state): State<ReceiverState>,
    Json(request): Json<PairRequest>,
) -> Result<impl IntoResponse, ApiError> {
    let paired = state
        .pairing
        .consume(&request.token, &request.device_name, now_ms())
        .map_err(|error| match error {
            crate::PairingError::Rejected(message) => ApiError::unauthorized(message),
            _ => ApiError::internal("pairing registry failed"),
        })?;
    Ok(Json(paired))
}

async fn upload(
    State(state): State<ReceiverState>,
    RoutePath(id): RoutePath<String>,
    headers: HeaderMap,
    body: Body,
) -> Result<Response, ApiError> {
    authenticate(&state, &headers)?;
    let source = match required_header(&headers, "x-louiselm-source")? {
        "android" => CaptureSource::Android,
        "neovim" => CaptureSource::Neovim,
        _ => return Err(ApiError::bad_request("unsupported capture source")),
    };
    let recorded_at_ms = parse_positive_header(&headers, "x-louiselm-recorded-at-ms")?;
    let duration_ms = parse_positive_header(&headers, "x-louiselm-duration-ms")?;
    let mime_type = required_header(&headers, header::CONTENT_TYPE.as_str())?.to_owned();
    let expected_sha256 = required_header(&headers, "x-louiselm-sha256")?.to_ascii_lowercase();
    if expected_sha256.len() != 64 || !expected_sha256.bytes().all(|byte| byte.is_ascii_hexdigit())
    {
        return Err(ApiError::bad_request("invalid SHA-256 header"));
    }
    if headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .is_some_and(|length| length > MAX_CAPTURE_BYTES)
    {
        return Err(ApiError::payload_too_large());
    }

    let temporary = state.uploads.join(format!(".upload-{}", Uuid::new_v4()));
    let streamed = stream_body(body, &temporary).await;
    let (bytes, actual_sha256) = match streamed {
        Ok(value) => value,
        Err(error) => {
            let _ = tokio::fs::remove_file(&temporary).await;
            return Err(error);
        }
    };
    if bytes == 0 {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(ApiError::bad_request("audio body is empty"));
    }
    if actual_sha256 != expected_sha256 {
        let _ = tokio::fs::remove_file(&temporary).await;
        return Err(ApiError::bad_request("audio digest does not match header"));
    }

    let store = state.store.clone();
    let draft = CaptureDraft {
        id,
        source,
        recorded_at_ms,
        duration_ms,
        mime_type,
    };
    let ingest_path = temporary.clone();
    let task = tokio::task::spawn_blocking(move || {
        let audio = std::fs::File::open(ingest_path)?;
        store.ingest(draft, audio)
    })
    .await;
    let _ = tokio::fs::remove_file(&temporary).await;
    let outcome = task.map_err(|_| ApiError::internal("capture ingestion task failed"))?;
    match outcome {
        Ok(IngestOutcome::Created) => Ok(StatusCode::CREATED.into_response()),
        Ok(IngestOutcome::Existing) => Ok(StatusCode::OK.into_response()),
        Err(StoreError::TooLarge { .. }) => Err(ApiError::payload_too_large()),
        Err(StoreError::InvalidCapture(message) | StoreError::Conflict(message)) => {
            Err(ApiError::bad_request(message))
        }
        Err(error) => Err(ApiError::internal(error.to_string())),
    }
}

fn authenticate(state: &ReceiverState, headers: &HeaderMap) -> Result<(), ApiError> {
    let authorization = required_header(headers, header::AUTHORIZATION.as_str())?;
    let credential = authorization
        .strip_prefix("Bearer ")
        .ok_or_else(|| ApiError::unauthorized("invalid authorization scheme"))?;
    if !state
        .pairing
        .authenticate(credential)
        .map_err(|error| ApiError::internal(error.to_string()))?
    {
        return Err(ApiError::unauthorized("device credential is invalid"));
    }
    Ok(())
}

fn required_header<'a>(headers: &'a HeaderMap, name: &str) -> Result<&'a str, ApiError> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            if name == header::AUTHORIZATION.as_str() {
                ApiError::unauthorized("authorization is required")
            } else {
                ApiError::bad_request(format!("required header is missing: {name}"))
            }
        })
}

fn parse_positive_header(headers: &HeaderMap, name: &str) -> Result<u64, ApiError> {
    let value = required_header(headers, name)?
        .parse::<u64>()
        .map_err(|_| ApiError::bad_request(format!("header must be a positive integer: {name}")))?;
    if value == 0 {
        return Err(ApiError::bad_request(format!(
            "header must be a positive integer: {name}"
        )));
    }
    Ok(value)
}

async fn stream_body(mut body: Body, path: &Path) -> Result<(u64, String), ApiError> {
    let mut output = tokio::fs::OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(path)
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    set_private_permissions(path, false).map_err(|error| ApiError::internal(error.to_string()))?;
    let mut bytes = 0_u64;
    let mut hasher = Sha256::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| ApiError::bad_request("audio body could not be read"))?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        bytes = bytes.saturating_add(data.len() as u64);
        if bytes > MAX_CAPTURE_BYTES {
            return Err(ApiError::payload_too_large());
        }
        output
            .write_all(&data)
            .await
            .map_err(|error| ApiError::internal(error.to_string()))?;
        hasher.update(&data);
    }
    output
        .flush()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    output
        .sync_all()
        .await
        .map_err(|error| ApiError::internal(error.to_string()))?;
    Ok((bytes, format!("{:x}", hasher.finalize())))
}

#[cfg(unix)]
fn set_private_permissions(path: &Path, directory: bool) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if directory { 0o700 } else { 0o600 }),
    )
}

#[cfg(not(unix))]
fn set_private_permissions(_path: &Path, _directory: bool) -> std::io::Result<()> {
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_millis() as u64)
}

#[derive(Serialize)]
struct ErrorBody {
    error: String,
}

struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn unauthorized(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
        }
    }

    fn payload_too_large() -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message: format!("capture exceeds {MAX_CAPTURE_BYTES} bytes"),
        }
    }

    fn internal(_message: impl Into<String>) -> Self {
        // External responses do not expose filesystem, registry, or runtime details.
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: "capture service failed".to_owned(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}
