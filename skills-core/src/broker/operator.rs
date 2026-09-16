//! Bounded local operator inspection and skill decisions, authenticated before lookup.

mod wire;

use crate::launch_protocol::SessionStatus;
use crate::skill_request::{SkillRequestOutcome, SkillRequestStatus};
use serde::{Deserialize, Serialize};
use std::{
    fs, io,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, PermissionsExt},
        net::{UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

/// Fixed installed operator endpoint, distinct from the supervisor rendezvous.
pub const SOCKET: &str = "/run/louiselm-operator/inspect.sock";
/// Total budget for one authenticated inspection exchange.
pub const TIMEOUT: Duration = Duration::from_secs(35);
const READY: &[u8] = b"louiselm.operator/1";

/// Stable, redacted operator failures; no OS paths or external prose are included.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InspectError {
    /// Invalid command or request fields.
    InvalidRequest,
    /// No trusted broker endpoint could be reached.
    BrokerUnavailable,
    /// The client or server kernel UID is not the installed identity.
    AuthenticationRefused,
    /// The authenticated operator named no known Session.
    UnknownSession,
    /// Current status cannot be obtained without guessing from durable history.
    StatusUnavailable,
}

impl InspectError {
    /// Stable CLI exit status; zero is reserved for canonical success.
    #[must_use]
    pub const fn exit_code(self) -> u8 {
        match self {
            Self::InvalidRequest => 2,
            Self::BrokerUnavailable => 3,
            Self::AuthenticationRefused => 4,
            Self::UnknownSession => 5,
            Self::StatusUnavailable => 6,
        }
    }

    /// Canonical typed diagnostic with a fixed safe next action.
    #[must_use]
    pub fn canonical_bytes(self) -> Vec<u8> {
        let (code, action) = match self {
            Self::InvalidRequest => ("invalid_request", "check_request"),
            Self::BrokerUnavailable => ("broker_unavailable", "check_broker_service"),
            Self::AuthenticationRefused => ("authentication_refused", "use_configured_operator"),
            Self::UnknownSession => ("unknown_session", "check_session_id"),
            Self::StatusUnavailable => ("status_unavailable", "retry_inspection"),
        };
        format!("{{\"schema\":\"louiselm.operator-error/1\",\"error\":\"{code}\",\"next_action\":\"{action}\"}}").into_bytes()
    }

    fn parse(bytes: &[u8]) -> Option<Self> {
        [
            Self::InvalidRequest,
            Self::BrokerUnavailable,
            Self::AuthenticationRefused,
            Self::UnknownSession,
            Self::StatusUnavailable,
        ]
        .into_iter()
        .find(|error| error.canonical_bytes() == bytes)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
enum Request {
    #[serde(rename = "louiselm.operator-inspect/1")]
    Inspect { session_id: String },
    #[serde(rename = "louiselm.operator-skill-request/1")]
    Skill {
        operation_id: String,
        outcome: Option<SkillRequestOutcome>,
    },
}

/// Checks the same bounded Session identifier accepted by durable broker records.
///
/// # Errors
/// Returns `InvalidRequest` without connecting or reading installed authority.
pub fn validate_subject(session_id: &str) -> Result<(), InspectError> {
    if super::is_record_identifier(session_id) {
        Ok(())
    } else {
        Err(InspectError::InvalidRequest)
    }
}

/// Performs one blocking, bounded read-only query as the current operator.
/// Authenticates the broker kernel UID and validates exact canonical output.
///
/// # Errors
/// Returns a typed identity, request, availability or broker refusal.
pub fn inspect(
    path: &Path,
    broker_uid: u32,
    session_id: &str,
    timeout: Duration,
) -> Result<SessionStatus, InspectError> {
    validate_subject(session_id)?;
    let bytes = exchange(
        path,
        broker_uid,
        &Request::Inspect {
            session_id: session_id.into(),
        },
        timeout,
    )?;
    let status =
        SessionStatus::parse_canonical(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    if status.session_id != session_id {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(status)
}

/// Inspects, rejects or cancels one durable request through the operator endpoint.
/// Retries name the same operation; no decision grants Admission or Resume.
/// # Errors
/// Returns typed authentication, request, storage or transport refusal.
pub fn skill_request(
    path: &Path,
    broker_uid: u32,
    operation_id: &str,
    outcome: Option<SkillRequestOutcome>,
    timeout: Duration,
) -> Result<SkillRequestStatus, InspectError> {
    if !super::attention::canonical_uuid(operation_id)
        || outcome == Some(SkillRequestOutcome::Pending)
    {
        return Err(InspectError::InvalidRequest);
    }
    let bytes = exchange(
        path,
        broker_uid,
        &Request::Skill {
            operation_id: operation_id.into(),
            outcome,
        },
        timeout,
    )?;
    let status: SkillRequestStatus =
        serde_json::from_slice(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    if !status.valid()
        || status.operation_id != operation_id
        || outcome.is_some_and(|expected| status.outcome != expected)
    {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(status)
}

fn exchange(
    path: &Path,
    broker_uid: u32,
    request: &Request,
    timeout: Duration,
) -> Result<Vec<u8>, InspectError> {
    let deadline = Instant::now()
        .checked_add(timeout)
        .ok_or(InspectError::InvalidRequest)?;
    let mut stream = wire::connect(path).map_err(|error| {
        if error.kind() == io::ErrorKind::PermissionDenied {
            InspectError::AuthenticationRefused
        } else {
            InspectError::BrokerUnavailable
        }
    })?;
    if peer_uid(&stream)? != broker_uid {
        return Err(InspectError::AuthenticationRefused);
    }
    let ready = wire::read(&mut stream, deadline).map_err(|_| InspectError::BrokerUnavailable)?;
    if let Some(error) = InspectError::parse(&ready) {
        return Err(error);
    }
    if ready != READY {
        return Err(InspectError::BrokerUnavailable);
    }
    let bytes = serde_json::to_vec(&request).map_err(|_| InspectError::InvalidRequest)?;
    wire::write(&mut stream, &bytes, deadline).map_err(|_| InspectError::BrokerUnavailable)?;
    let bytes = wire::read(&mut stream, deadline).map_err(|_| InspectError::BrokerUnavailable)?;
    if let Some(error) = InspectError::parse(&bytes) {
        return Err(error);
    }
    Ok(bytes)
}

fn peer_uid(stream: &UnixStream) -> Result<u32, InspectError> {
    rustix::net::sockopt::socket_peercred(stream)
        .map(|peer| peer.uid.as_raw())
        .map_err(|_| InspectError::AuthenticationRefused)
}

/// Owns the daemon-created operator listener. The caller owns its serving thread.
/// The installed caller must hold the broker state lock, validate trusted path
/// ancestors and use the fixed endpoint. No Session is read during construction.
pub struct OperatorServer {
    listener: UnixListener,
    operator_uid: u32,
    path: PathBuf,
    inode: u64,
}

impl OperatorServer {
    /// Binds below an existing directory owned by this broker and not writable
    /// by other identities. Under the caller's singleton state lock, a refused
    /// stale socket may be removed; live listeners and foreign paths are preserved.
    ///
    /// # Errors
    /// Refuses foreign/linked paths, insecure directories or unavailable sockets.
    pub fn bind(path: &Path, operator_uid: u32) -> io::Result<Self> {
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("missing endpoint parent"))?;
        let uid = rustix::process::geteuid().as_raw();
        let metadata = fs::symlink_metadata(parent)?;
        if !metadata.is_dir() || metadata.uid() != uid || metadata.mode() & 0o022 != 0 {
            return Err(io::Error::other("untrusted endpoint directory"));
        }
        match fs::symlink_metadata(path) {
            Ok(metadata) => {
                if !metadata.file_type().is_socket() || metadata.uid() != uid {
                    return Err(io::Error::other("untrusted endpoint path"));
                }
                match wire::connect(path) {
                    Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => {
                        fs::remove_file(path)?;
                    }
                    _ => return Err(io::Error::other("endpoint already in use")),
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        let listener = UnixListener::bind(path)?;
        // Anyone may connect to receive a typed refusal; only the kernel-pinned
        // operator reaches a request read or Session lookup.
        fs::set_permissions(path, fs::Permissions::from_mode(0o666))?;
        let inode = fs::symlink_metadata(path)?.ino();
        Ok(Self {
            listener,
            operator_uid,
            path: path.to_owned(),
            inode,
        })
    }

    /// Serves one peer synchronously, authenticating before reading its request.
    /// Client failures do not terminate the listener. The handler must finish
    /// within the supplied absolute deadline and serialize with the Session owner.
    ///
    /// # Errors
    /// Returns listener failure; refused or disconnected clients are isolated.
    pub fn serve_once(
        &self,
        lookup: impl FnOnce(&str, Instant) -> Result<SessionStatus, InspectError>,
        skill: impl FnOnce(
            &str,
            Option<SkillRequestOutcome>,
        ) -> Result<SkillRequestStatus, InspectError>,
    ) -> io::Result<()> {
        let (mut stream, _) = self.listener.accept()?;
        let deadline = Instant::now() + TIMEOUT;
        let result = (|| {
            if peer_uid(&stream)? != self.operator_uid {
                return Err(InspectError::AuthenticationRefused);
            }
            wire::write(&mut stream, READY, deadline).map_err(|_| InspectError::InvalidRequest)?;
            let bytes =
                wire::read(&mut stream, deadline).map_err(|_| InspectError::InvalidRequest)?;
            let request: Request =
                serde_json::from_slice(&bytes).map_err(|_| InspectError::InvalidRequest)?;
            if Instant::now() >= deadline {
                return Err(InspectError::StatusUnavailable);
            }
            match request {
                Request::Inspect { session_id } => {
                    validate_subject(&session_id)?;
                    Ok(lookup(&session_id, deadline)?.canonical_bytes())
                }
                Request::Skill {
                    operation_id,
                    outcome,
                } => {
                    if !super::attention::canonical_uuid(&operation_id)
                        || outcome == Some(SkillRequestOutcome::Pending)
                    {
                        return Err(InspectError::InvalidRequest);
                    }
                    serde_json::to_vec(&skill(&operation_id, outcome)?)
                        .map_err(|_| InspectError::StatusUnavailable)
                }
            }
        })();
        let bytes = result.unwrap_or_else(InspectError::canonical_bytes);
        // A vanished reader never rolls back a durable decision. Retrying names
        // the same operation and cannot reopen a terminal request.
        let _ = wire::write(&mut stream, &bytes, deadline);
        Ok(())
    }
}

impl Drop for OperatorServer {
    fn drop(&mut self) {
        if let Ok(metadata) = fs::symlink_metadata(&self.path)
            && metadata.file_type().is_socket()
            && metadata.ino() == self.inode
        {
            // Best-effort name cleanup only. Startup validates stale sockets;
            // failure cannot authorize a peer or permit concurrent state ownership.
            let _ = fs::remove_file(&self.path);
        }
    }
}
