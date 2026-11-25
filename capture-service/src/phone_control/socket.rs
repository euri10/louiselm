//! Authenticated local phone-control administration and controller sockets.

use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use fs2::FileExt;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    sync::{mpsc, oneshot},
    task::JoinSet,
};

use super::{
    MAX_FRAME, MAX_INTEGER, Request,
    authority::{Authority, EnrollmentStatus, Refusal, Scope, random_hex},
    fixed,
    prepared_owner::{OwnerPreparedError, PreparedAdmission, PreparedOwner, Publication},
    source::PhoneSourceError,
    valid_string,
};
use crate::{
    PairingRegistry,
    operator_socket::{
        load_or_create_capability, remove_stale_socket, set_owner_only, token_sha256,
        verify_capability,
    },
    permissions::set_private_permissions,
    time::now_ms,
};

struct LockedListener {
    listener: UnixListener,
    _lock: SocketLock,
    operator_hash: String,
}

struct SocketLock(File);

impl Drop for SocketLock {
    fn drop(&mut self) {
        if let Err(error) = FileExt::unlock(&self.0) {
            eprintln!("phone-control socket lock release failed: {error}");
        }
    }
}

fn prepare_listener(path: &Path, capability: &Path) -> io::Result<(SocketLock, String, bool)> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::other("socket has no parent"))?;
    fs::create_dir_all(parent)?;
    set_private_permissions(parent, true)?;
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)?;
    set_private_permissions(&lock_path, false)?;
    lock.try_lock_exclusive()
        .map_err(|_| io::Error::new(io::ErrorKind::AddrInUse, "phone-control socket is owned"))?;
    let lock = SocketLock(lock);
    let token = load_or_create_capability(capability)?;
    let occupied = match fs::symlink_metadata(path) {
        Ok(_) => true,
        Err(error) if error.kind() == io::ErrorKind::NotFound => false,
        Err(error) => return Err(error),
    };
    Ok((lock, token_sha256(&token), occupied))
}

impl LockedListener {
    async fn bind(path: &Path, capability: &Path) -> io::Result<Self> {
        let owned_path = path.to_owned();
        let owned_capability = capability.to_owned();
        let (lock, operator_hash, occupied) =
            tokio::task::spawn_blocking(move || prepare_listener(&owned_path, &owned_capability))
                .await
                .map_err(io::Error::other)??;
        if occupied {
            if UnixStream::connect(path).await.is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    "phone-control socket is live",
                ));
            }
            let owned_path = path.to_owned();
            tokio::task::spawn_blocking(move || remove_stale_socket(&owned_path))
                .await
                .map_err(io::Error::other)??;
        }
        let listener = UnixListener::bind(path)?;
        let owned_path = path.to_owned();
        tokio::task::spawn_blocking(move || set_owner_only(&owned_path))
            .await
            .map_err(io::Error::other)??;
        Ok(Self {
            listener,
            _lock: lock,
            operator_hash,
        })
    }
}

#[derive(Default)]
pub(super) struct ControllerRegistry {
    attached: Mutex<BTreeMap<String, Option<mpsc::Sender<Dispatch>>>>,
}

struct Dispatch {
    frame: Vec<u8>,
    response: oneshot::Sender<Vec<u8>>,
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ReadResponse {
    Error {
        v: u64,
        code: String,
    },
    SessionPage {
        v: u64,
        controller_id: String,
        subject_id: String,
        operation_id: String,
        revision: u64,
        status: String,
        entries: Vec<ReadEntry>,
        next_before: u64,
        complete: bool,
        source: HistorySource,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct HistorySource {
    replay: ReplaySource,
    #[serde(rename = "archive")]
    _archive: ArchiveSource,
    redaction: RedactionSource,
    missing: bool,
    truncated: bool,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum ReplaySource {
    Unverified,
    NotApplicable,
}

#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum ArchiveSource {
    Unavailable,
}

#[derive(Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum RedactionSource {
    Present,
    Unknown,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum ReadEntry {
    Message {
        seq: u64,
        role: String,
        text: String,
        #[serde(rename = "truncated")]
        _truncated: bool,
    },
    Tool {
        seq: u64,
        id: String,
        title: String,
        status: String,
        content: String,
        #[serde(rename = "truncated")]
        _truncated: bool,
    },
    Question {
        seq: u64,
        request_id: String,
        question: String,
        options: Vec<ReadOption>,
        #[serde(rename = "multi_select")]
        _multi_select: bool,
        #[serde(rename = "truncated")]
        _truncated: bool,
    },
    Plan {
        seq: u64,
        request_id: String,
        text: String,
        #[serde(rename = "truncated")]
        _truncated: bool,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadOption {
    label: String,
    description: String,
    preview: String,
}

impl ReadEntry {
    fn valid_sequence(&self) -> Option<u64> {
        let (seq, valid) = match self {
            Self::Message {
                seq, role, text, ..
            } => (
                *seq,
                matches!(role.as_str(), "user" | "assistant") && valid_string(text),
            ),
            Self::Tool {
                seq,
                id,
                title,
                status,
                content,
                ..
            } => (
                *seq,
                valid_string(id)
                    && valid_string(title)
                    && valid_string(content)
                    && matches!(
                        status.as_str(),
                        "pending" | "in_progress" | "completed" | "failed" | "unknown"
                    ),
            ),
            Self::Question {
                seq,
                request_id,
                question,
                options,
                ..
            } => (
                *seq,
                valid_string(request_id)
                    && valid_string(question)
                    && options.len() <= 50
                    && options.iter().all(|option| {
                        valid_string(&option.label)
                            && valid_string(&option.description)
                            && valid_string(&option.preview)
                    }),
            ),
            Self::Plan {
                seq,
                request_id,
                text,
                ..
            } => (*seq, valid_string(request_id) && valid_string(text)),
        };
        (valid && seq > 0 && seq <= MAX_INTEGER).then_some(seq)
    }
}

pub(super) fn validate_read_response(bytes: &[u8], request: &Request) -> Result<Value, Refusal> {
    if bytes.len() >= MAX_FRAME {
        return Err(Refusal::Unavailable);
    }
    let response: ReadResponse = serde_json::from_slice(bytes).map_err(|_| Refusal::Unavailable)?;
    match response {
        ReadResponse::Error { v: 1, code } => Err(match code.as_str() {
            "unauthenticated" => Refusal::Unauthenticated,
            "expired" => Refusal::Expired,
            "revoked" => Refusal::Revoked,
            "out_of_scope" => Refusal::OutOfScope,
            "stale_subject" => Refusal::StaleSubject,
            "malformed" => Refusal::Malformed,
            "unsupported" => Refusal::Unsupported,
            _ => Refusal::Unavailable,
        }),
        ReadResponse::SessionPage {
            v: 2,
            controller_id,
            subject_id,
            operation_id,
            revision,
            status,
            entries,
            next_before,
            complete,
            source,
        } => {
            let payload = fixed::<10>(&request.payload).map_err(|_| Refusal::Unavailable)?;
            let mut before_bytes = [0_u8; 8];
            before_bytes.copy_from_slice(&payload[..8]);
            let before = u64::from_be_bytes(before_bytes);
            let limit = usize::from(u16::from_be_bytes([payload[8], payload[9]]));
            let mut previous = 0;
            for entry in &entries {
                let seq = entry.valid_sequence().ok_or(Refusal::Unavailable)?;
                if seq <= previous || (before != 0 && seq >= before) {
                    return Err(Refusal::Unavailable);
                }
                previous = seq;
            }
            if controller_id != request.controller_id
                || subject_id != request.subject_id
                || operation_id != request.operation_id
                || revision == 0
                || revision > MAX_INTEGER
                || (request.expected_revision != 0 && revision != request.expected_revision)
                || entries.len() > limit
                || (complete
                    && (source.replay != ReplaySource::NotApplicable
                        || source.redaction != RedactionSource::Unknown
                        || source.missing
                        || source.truncated))
                || next_before > MAX_INTEGER
                || (next_before != 0 && before != 0 && next_before >= before)
                || (next_before != 0
                    && entries
                        .first()
                        .and_then(ReadEntry::valid_sequence)
                        .is_some_and(|seq| next_before > seq))
                || (next_before != 0
                    && !source.missing
                    && entries
                        .first()
                        .and_then(ReadEntry::valid_sequence)
                        .is_none_or(|seq| next_before < seq))
                || !matches!(
                    status.as_str(),
                    "starting"
                        | "ready"
                        | "admitting"
                        | "configuring"
                        | "preparing"
                        | "prompting"
                        | "running"
                        | "waiting_permission"
                        | "cancelling"
                        | "error"
                )
            {
                return Err(Refusal::Unavailable);
            }
            serde_json::from_slice(bytes).map_err(|_| Refusal::Unavailable)
        }
        _ => Err(Refusal::Unavailable),
    }
}

impl ControllerRegistry {
    pub(super) fn list_after(&self, after: &str, limit: usize) -> Result<Vec<String>, Refusal> {
        let entries = self.attached.lock().map_err(|_| Refusal::Unavailable)?;
        Ok(entries
            .iter()
            .filter(|(id, sender)| id.as_str() > after && sender.is_some())
            .take(limit)
            .map(|(id, _)| id.clone())
            .collect())
    }

    pub(super) fn contains(&self, id: &str) -> bool {
        self.attached
            .lock()
            .is_ok_and(|entries| entries.get(id).is_some_and(Option::is_some))
    }

    pub(super) async fn dispatch(&self, id: &str, frame: Vec<u8>) -> Result<Vec<u8>, Refusal> {
        let sender = self
            .attached
            .lock()
            .map_err(|_| Refusal::Unavailable)?
            .get(id)
            .and_then(Clone::clone)
            .ok_or(Refusal::StaleSubject)?;
        let (response, received) = oneshot::channel();
        sender
            .try_send(Dispatch { frame, response })
            .map_err(|_| Refusal::Unavailable)?;
        tokio::time::timeout(Duration::from_secs(10), received)
            .await
            .map_err(|_| Refusal::Unavailable)?
            .map_err(|_| Refusal::Unavailable)
    }
}

struct Registration {
    registry: Arc<ControllerRegistry>,
    id: String,
}

impl Drop for Registration {
    fn drop(&mut self) {
        if let Ok(mut attached) = self.registry.attached.lock()
            && let Some(active) = attached.get_mut(&self.id)
        {
            *active = None;
        }
    }
}

pub(super) struct ControllerSocket {
    socket: LockedListener,
    registry: Arc<ControllerRegistry>,
}

impl ControllerSocket {
    pub(super) async fn bind(
        path: &Path,
        capability: &Path,
        registry: Arc<ControllerRegistry>,
    ) -> io::Result<Self> {
        Ok(Self {
            socket: LockedListener::bind(path, capability).await?,
            registry,
        })
    }

    pub(super) async fn serve(self) -> io::Result<()> {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                accepted = self.socket.listener.accept() => {
                    let (stream, _) = accepted?;
                    if clients.len() >= 64 { continue; }
                    let hash = self.socket.operator_hash.clone();
                    let registry = self.registry.clone();
                    clients.spawn(async move { serve_controller(stream, &hash, registry).await });
                }
                Some(_) = clients.join_next(), if !clients.is_empty() => {}
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Attach {
    v: u64,
    #[serde(rename = "type")]
    kind: String,
    capability: String,
}

async fn serve_controller(
    stream: UnixStream,
    hash: &str,
    registry: Arc<ControllerRegistry>,
) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let bytes = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut reader))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "attach timed out"))??;
    let (sender, mut requests) = mpsc::channel::<Dispatch>(1);
    let result = match serde_json::from_slice::<Attach>(&bytes) {
        Err(_) => Err(Refusal::Malformed),
        Ok(request)
            if request.v > MAX_INTEGER
                || !valid_string(&request.kind)
                || !valid_string(&request.capability) =>
        {
            Err(Refusal::Malformed)
        }
        Ok(request) if request.v != 1 || request.kind != "attach" => Err(Refusal::Unsupported),
        Ok(request) if !verify_capability(hash, &request.capability) => {
            Err(Refusal::Unauthenticated)
        }
        Ok(_) => {
            let mut attached = registry
                .attached
                .lock()
                .map_err(|_| io::Error::other("controller registry unavailable"))?;
            let mut created = Err(Refusal::Unavailable);
            for _ in 0..8 {
                let candidate = random_hex(16);
                let Ok(id) = candidate else {
                    break;
                };
                if id != "00".repeat(16) && !attached.contains_key(&id) {
                    attached.insert(id.clone(), Some(sender));
                    created = Ok(id);
                    break;
                }
            }
            created
        }
    };
    let id = match result {
        Ok(id) => id,
        Err(error) => {
            let response = json!({"v":1,"type":"error","code":error.code()});
            writer
                .write_all(&serde_json::to_vec(&response).map_err(io::Error::other)?)
                .await?;
            writer.write_all(b"\n").await?;
            return Ok(());
        }
    };
    let _registration = Registration {
        registry,
        id: id.clone(),
    };
    let response = json!({"v":1,"type":"attached","controller_id":id});
    writer
        .write_all(&serde_json::to_vec(&response).map_err(io::Error::other)?)
        .await?;
    writer.write_all(b"\n").await?;
    loop {
        let mut byte = [0; 1];
        tokio::select! {
            unsolicited = reader.read(&mut byte) => {
                unsolicited?;
                return Ok(());
            }
            Some(dispatch) = requests.recv() => {
                if dispatch.response.is_closed() {
                    continue;
                }
                let delivery = async {
                    writer.write_all(&dispatch.frame).await?;
                    writer.write_all(b"\n").await?;
                    read_frame(&mut reader).await
                };
                let bytes = tokio::time::timeout(Duration::from_secs(8), delivery).await
                    .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "controller response timed out"))??;
                if dispatch.response.send(bytes).is_err() {
                    return Ok(());
                }
            }
            else => return Ok(()),
        }
    }
}

pub(super) struct AdminSocket {
    socket: LockedListener,
    authority: Arc<Authority>,
    pairing: Arc<PairingRegistry>,
    prepared: Option<PreparedOwner>,
}

impl AdminSocket {
    pub(super) async fn bind(
        path: &Path,
        capability: &Path,
        authority: Arc<Authority>,
        pairing: Arc<PairingRegistry>,
    ) -> io::Result<Self> {
        Ok(Self {
            socket: LockedListener::bind(path, capability).await?,
            authority,
            pairing,
            prepared: None,
        })
    }

    pub(super) fn with_prepared(mut self, prepared: Option<PreparedOwner>) -> Self {
        self.prepared = prepared;
        self
    }

    pub(super) async fn serve(self) -> io::Result<()> {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                accepted = self.socket.listener.accept() => {
                    let (stream, _) = accepted?;
                    if clients.len() >= 64 { continue; }
                    let authority = self.authority.clone();
                    let pairing = self.pairing.clone();
                    let prepared = self.prepared.clone();
                    let hash = self.socket.operator_hash.clone();
                    clients.spawn(async move { serve_admin(stream, authority, pairing, prepared, &hash).await });
                }
                Some(_) = clients.join_next(), if !clients.is_empty() => {}
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum AdminRequest {
    PublishPrepared {
        v: u64,
        capability: String,
        run_id: String,
        source_id: String,
        checkout_id: String,
        selection: String,
        registration_id: String,
        expected_revision: u64,
    },
    ListPrepared {
        v: u64,
        capability: String,
    },
    InspectPrepared {
        v: u64,
        capability: String,
        registration_id: String,
        revision: u64,
    },
    OpenPrepared {
        v: u64,
        capability: String,
        registration_id: String,
        revision: u64,
    },
    AdmitPrepared {
        v: u64,
        capability: String,
        registration_id: String,
        revision: u64,
        selection_sha256: String,
        generated_work_max: u64,
        park_ttl_ms: u64,
    },
    UnpublishPrepared {
        v: u64,
        capability: String,
        registration_id: String,
        expected_revision: u64,
    },
    PrepareRunCheckout {
        v: u64,
        capability: String,
        run_id: String,
        source_id: String,
        checkout: String,
    },
    InspectRunCheckout {
        v: u64,
        capability: String,
        run_id: String,
    },
    FreezeRunSource {
        v: u64,
        capability: String,
        project_id: String,
        run_id: String,
        snapshot: String,
        includes: Vec<String>,
    },
    InspectRunSource {
        v: u64,
        capability: String,
        run_id: String,
    },
    Projects {
        v: u64,
        capability: String,
        after: String,
    },
    ProjectRoots {
        v: u64,
        capability: String,
        after: String,
    },
    RegisterProject {
        v: u64,
        capability: String,
        path: String,
        root_ids: Vec<String>,
    },
    LinkProjectRoots {
        v: u64,
        capability: String,
        project_id: String,
        root_ids: Vec<String>,
    },
    RegisterRoot {
        v: u64,
        capability: String,
        path: String,
    },
    InspectGrant {
        v: u64,
        capability: String,
        device_id: String,
    },
    ConfirmLegacyProject {
        v: u64,
        capability: String,
        device_id: String,
        grant_id: String,
        grant_revision: u64,
        scope_digest: String,
        project_id: String,
    },
    Start {
        v: u64,
        capability: String,
        device_id: String,
    },
    Inspect {
        v: u64,
        capability: String,
        device_id: String,
    },
    Approve {
        v: u64,
        capability: String,
        device_id: String,
        enrollment_id: String,
        enrolled_key: String,
        scope: Scope,
        project_root_ids: Vec<String>,
    },
    Cancel {
        v: u64,
        capability: String,
        device_id: String,
        enrollment_id: String,
    },
    Disable {
        v: u64,
        capability: String,
        device_id: String,
    },
    Forget {
        v: u64,
        capability: String,
        device_id: String,
    },
}

impl AdminRequest {
    fn version(&self) -> u64 {
        if matches!(self, Self::Approve { .. }) {
            2
        } else {
            1
        }
    }

    fn common(&self) -> (u64, &str, Option<&str>) {
        match self {
            Self::PublishPrepared { v, capability, .. }
            | Self::ListPrepared { v, capability, .. }
            | Self::InspectPrepared { v, capability, .. }
            | Self::OpenPrepared { v, capability, .. }
            | Self::AdmitPrepared { v, capability, .. }
            | Self::UnpublishPrepared { v, capability, .. }
            | Self::PrepareRunCheckout { v, capability, .. }
            | Self::InspectRunCheckout { v, capability, .. }
            | Self::FreezeRunSource { v, capability, .. }
            | Self::InspectRunSource { v, capability, .. }
            | Self::Projects { v, capability, .. }
            | Self::ProjectRoots { v, capability, .. }
            | Self::RegisterProject { v, capability, .. }
            | Self::LinkProjectRoots { v, capability, .. }
            | Self::RegisterRoot { v, capability, .. } => (*v, capability, None),
            Self::Start {
                v,
                capability,
                device_id,
            }
            | Self::Inspect {
                v,
                capability,
                device_id,
            }
            | Self::InspectGrant {
                v,
                capability,
                device_id,
            }
            | Self::ConfirmLegacyProject {
                v,
                capability,
                device_id,
                ..
            }
            | Self::Approve {
                v,
                capability,
                device_id,
                ..
            }
            | Self::Cancel {
                v,
                capability,
                device_id,
                ..
            }
            | Self::Disable {
                v,
                capability,
                device_id,
            }
            | Self::Forget {
                v,
                capability,
                device_id,
            } => (*v, capability, Some(device_id)),
        }
    }
}

async fn read_frame(reader: &mut (impl AsyncBufRead + Unpin)) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let available = reader.fill_buf().await?;
        if available.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "partial phone-control frame",
            ));
        }
        let count = available
            .iter()
            .position(|byte| *byte == b'\n')
            .map_or(available.len(), |index| index + 1);
        if bytes.len() + count > MAX_FRAME {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "phone-control frame is too large",
            ));
        }
        bytes.extend_from_slice(&available[..count]);
        reader.consume(count);
        if bytes.last() == Some(&b'\n') {
            bytes.pop();
            return Ok(bytes);
        }
    }
}

async fn serve_admin(
    stream: UnixStream,
    authority: Arc<Authority>,
    pairing: Arc<PairingRegistry>,
    prepared: Option<PreparedOwner>,
    hash: &str,
) -> io::Result<()> {
    let (reader, mut writer) = stream.into_split();
    let mut reader = BufReader::new(reader);
    let bytes = tokio::time::timeout(Duration::from_secs(5), read_frame(&mut reader))
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "administration timed out"))??;
    let request = serde_json::from_slice::<AdminRequest>(&bytes);
    let result = match request {
        Ok(request) => {
            let (version, token, device) = request.common();
            if version > MAX_INTEGER || !valid_string(token) {
                Err(Refusal::Malformed)
            } else if version != request.version() {
                Err(Refusal::Unsupported)
            } else if !verify_capability(hash, token) {
                Err(Refusal::Unauthenticated)
            } else if device.is_some_and(|device| {
                !uuid::Uuid::parse_str(device).is_ok_and(|id| id.to_string() == device)
            }) {
                Err(Refusal::Malformed)
            } else {
                tokio::task::spawn_blocking(move || {
                    process_admin(&authority, &pairing, prepared.as_ref(), request)
                })
                .await
                .unwrap_or(Err(Refusal::Unavailable))
            }
        }
        Err(_) => Err(Refusal::Malformed),
    };
    let response = result.unwrap_or_else(|error| json!({"v":1,"type":"error","code":error.code()}));
    let bytes = serde_json::to_vec(&response).map_err(io::Error::other)?;
    writer.write_all(&bytes).await?;
    writer.write_all(b"\n").await
}

fn process_admin(
    authority: &Authority,
    pairing: &PairingRegistry,
    prepared: Option<&PreparedOwner>,
    request: AdminRequest,
) -> Result<Value, Refusal> {
    let (_, _, device) = request.common();
    if let Some(device) = device
        && !pairing
            .status()
            .map_err(|_| Refusal::Unavailable)?
            .devices
            .iter()
            .any(|status| status.device_id == device)
    {
        return Err(Refusal::Unauthenticated);
    }
    match request {
        request @ (AdminRequest::PublishPrepared { .. }
        | AdminRequest::ListPrepared { .. }
        | AdminRequest::InspectPrepared { .. }
        | AdminRequest::OpenPrepared { .. }
        | AdminRequest::AdmitPrepared { .. }
        | AdminRequest::UnpublishPrepared { .. }) => {
            process_prepared_admin(prepared.ok_or(Refusal::Unsupported)?, request)
        }
        request @ (AdminRequest::PrepareRunCheckout { .. }
        | AdminRequest::InspectRunCheckout { .. }
        | AdminRequest::FreezeRunSource { .. }
        | AdminRequest::InspectRunSource { .. }
        | AdminRequest::Projects { .. }
        | AdminRequest::ProjectRoots { .. }
        | AdminRequest::RegisterProject { .. }
        | AdminRequest::LinkProjectRoots { .. }
        | AdminRequest::RegisterRoot { .. }
        | AdminRequest::InspectGrant { .. }
        | AdminRequest::ConfirmLegacyProject { .. }) => {
            process_project_admin(authority, pairing, request)
        }
        AdminRequest::Start { device_id, .. } => {
            let offer = authority.start(&device_id, Instant::now(), now_ms())?;
            Ok(
                json!({"v":1,"type":"enrollment_started","device_id":device_id,
                "enrollment_id":offer.enrollment_id,"matching_code":offer.matching_code,
                "expires_at_ms":offer.expires_at_ms}),
            )
        }
        AdminRequest::Inspect { device_id, .. } => {
            match authority.enrollment(&device_id, None, Instant::now())? {
                EnrollmentStatus::Pending {
                    offer,
                    enrolled_key,
                } => Ok(json!({"v":1,"type":"enrollment_inspection",
                "device_id":device_id,"enrollment_id":offer.enrollment_id,"matching_code":offer.matching_code,
                "enrolled_key":enrolled_key.unwrap_or_default(),"expires_at_ms":offer.expires_at_ms})),
                EnrollmentStatus::Granted(_) => Err(Refusal::Unauthenticated),
            }
        }
        AdminRequest::Approve {
            device_id,
            enrollment_id,
            enrolled_key,
            scope,
            project_root_ids,
            ..
        } => {
            let grant = authority.approve(
                &device_id,
                &enrollment_id,
                &enrolled_key,
                scope,
                &project_root_ids,
                Instant::now(),
            )?;
            let project_root_ids = grant.project_roots()?.ok_or(Refusal::Unavailable)?;
            Ok(
                json!({"v":2,"type":"enrollment_approved","device_id":device_id,
                "grant_id":grant.grant_id,"grant_revision":grant.grant_revision,
                "project_root_ids":project_root_ids}),
            )
        }
        AdminRequest::Disable { device_id, .. } => {
            authority.disable(&device_id)?;
            Ok(json!({"v":1,"type":"disabled","device_id":device_id}))
        }
        AdminRequest::Cancel {
            device_id,
            enrollment_id,
            ..
        } => {
            authority.cancel(&device_id, &enrollment_id)?;
            Ok(
                json!({"v":1,"type":"enrollment_cancelled","device_id":device_id,"enrollment_id":enrollment_id}),
            )
        }
        AdminRequest::Forget { device_id, .. } => {
            authority.forget(pairing, &device_id)?;
            Ok(json!({"v":1,"type":"forgotten","device_id":device_id}))
        }
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "one closed owner protocol dispatch keeps exact reply handling together"
)]
fn process_prepared_admin(owner: &PreparedOwner, request: AdminRequest) -> Result<Value, Refusal> {
    match request {
        AdminRequest::PublishPrepared {
            run_id,
            source_id,
            checkout_id,
            selection,
            registration_id,
            expected_revision,
            ..
        } => {
            let previous = match (registration_id.is_empty(), expected_revision) {
                (true, 0) => None,
                (false, 1..=MAX_INTEGER) => Some((registration_id.as_str(), expected_revision)),
                _ => return Err(Refusal::Malformed),
            };
            let request = Publication {
                run_id: &run_id,
                source_id: &source_id,
                checkout_id: &checkout_id,
                selection: Path::new(&selection),
                previous,
            };
            let summary = owner
                .publish(&request)
                .map_err(|error| prepared_refusal(&error))?;
            Ok(
                json!({"v":1,"type":"prepared_registered","registration_id":summary.id,"run_id":summary.run_id,"revision":summary.revision}),
            )
        }
        AdminRequest::ListPrepared { .. } => {
            let items = owner.list().map_err(|error| prepared_refusal(&error))?;
            Ok(json!({"v":1,"type":"prepared_list","items":items}))
        }
        AdminRequest::InspectPrepared {
            registration_id,
            revision,
            ..
        } => {
            if revision == 0 || revision > MAX_INTEGER {
                return Err(Refusal::Malformed);
            }
            let registration = owner
                .inspect(&registration_id)
                .map_err(|error| prepared_refusal(&error))?;
            let saved = registration
                .revisions
                .iter()
                .find(|saved| saved.revision == revision)
                .ok_or(Refusal::StaleSubject)?;
            Ok(
                json!({"v":1,"type":"prepared_inspection","registration_id":registration.id,"published":registration.published,"revision":saved.revision,"binding":saved.binding,"selection":saved.selection}),
            )
        }
        AdminRequest::OpenPrepared {
            registration_id,
            revision,
            ..
        } => {
            if revision == 0 || revision > MAX_INTEGER {
                return Err(Refusal::Malformed);
            }
            owner
                .with_published(&registration_id, revision, |saved, uid| {
                    super::prepared_selection::project_detail(saved, uid)
                        .map_err(|_| Refusal::Malformed)?;
                    Ok(
                        json!({"v":1,"type":"prepared_waiting","registration_id":registration_id,
                    "revision":saved.revision,"binding":saved.binding,"selection":saved.selection}),
                    )
                })
                .map_err(|error| prepared_refusal(&error))?
        }
        AdminRequest::AdmitPrepared {
            registration_id,
            revision,
            selection_sha256,
            generated_work_max,
            park_ttl_ms,
            ..
        } => {
            if revision == 0
                || revision > MAX_INTEGER
                || generated_work_max == 0
                || generated_work_max > MAX_INTEGER
                || park_ttl_ms == 0
                || park_ttl_ms > MAX_INTEGER
            {
                return Err(Refusal::Malformed);
            }
            let token = uuid::Uuid::new_v4().to_string();
            let request = PreparedAdmission {
                registration_id: &registration_id,
                revision,
                selection_sha256: &selection_sha256,
                generated_work_max,
                park_ttl_ms,
                token: &token,
            };
            let run_id = owner
                .admit(&request)
                .map_err(|error| prepared_refusal(&error))?;
            Ok(
                json!({"v":1,"type":"prepared_admitted","registration_id":registration_id,
                "revision":revision,"run_id":run_id,"token":token}),
            )
        }
        AdminRequest::UnpublishPrepared {
            registration_id,
            expected_revision,
            ..
        } => {
            if expected_revision == 0 || expected_revision > MAX_INTEGER {
                return Err(Refusal::Malformed);
            }
            owner
                .unpublish(&registration_id, expected_revision)
                .map_err(|error| prepared_refusal(&error))?;
            Ok(
                json!({"v":1,"type":"prepared_unpublished","registration_id":registration_id,"revision":expected_revision}),
            )
        }
        _ => Err(Refusal::Malformed),
    }
}

fn prepared_refusal(error: &OwnerPreparedError) -> Refusal {
    match error {
        OwnerPreparedError::Source(PhoneSourceError::Unsupported) => Refusal::Unsupported,
        OwnerPreparedError::Source(
            PhoneSourceError::Conflict
            | PhoneSourceError::Invalid("source or checkout identity is stale"),
        )
        | OwnerPreparedError::Run(crate::RunStoreError::Invalid(_))
        | OwnerPreparedError::Prepared(crate::PreparedRunError::Invalid(_)) => {
            Refusal::StaleSubject
        }
        OwnerPreparedError::Invalid(_)
        | OwnerPreparedError::Selection(
            super::prepared_selection::SelectionError::Invalid(_)
            | super::prepared_selection::SelectionError::Json(_),
        ) => Refusal::Malformed,
        _ => Refusal::Unavailable,
    }
}

fn process_project_admin(
    authority: &Authority,
    pairing: &PairingRegistry,
    request: AdminRequest,
) -> Result<Value, Refusal> {
    match request {
        AdminRequest::PrepareRunCheckout {
            run_id,
            source_id,
            checkout,
            ..
        } => {
            let proof = authority.prepare_run_checkout(&run_id, &source_id, &checkout)?;
            Ok(checkout_result(&proof))
        }
        AdminRequest::InspectRunCheckout { run_id, .. } => {
            let proof = authority.inspect_run_checkout(&run_id)?;
            Ok(checkout_result(&proof))
        }
        AdminRequest::FreezeRunSource {
            project_id,
            run_id,
            snapshot,
            includes,
            ..
        } => {
            let proof = authority.freeze_run_source(&project_id, &run_id, &snapshot, &includes)?;
            Ok(source_result(&proof))
        }
        AdminRequest::InspectRunSource { run_id, .. } => {
            let proof = authority.inspect_run_source(&run_id)?;
            Ok(source_result(&proof))
        }
        AdminRequest::Projects { after, .. } => {
            let page = authority.projects(&after)?;
            Ok(json!({"v":1,"type":"projects","projects":page.projects,
                "next_after":page.next_after.unwrap_or_default()}))
        }
        AdminRequest::ProjectRoots { after, .. } => {
            let page = authority.project_roots(&after)?;
            Ok(
                json!({"v":1,"type":"project_roots","roots":page.roots,"next_after":page.next_after.unwrap_or_default()}),
            )
        }
        AdminRequest::RegisterProject { path, root_ids, .. } => {
            let id = authority.register_project(&path, &root_ids)?;
            Ok(json!({"v":1,"type":"project_registered","project_id":id}))
        }
        AdminRequest::LinkProjectRoots {
            project_id,
            root_ids,
            ..
        } => {
            authority.link_project_roots(&project_id, &root_ids)?;
            Ok(json!({"v":1,"type":"project_roots_linked","project_id":project_id}))
        }
        AdminRequest::RegisterRoot { path, .. } => {
            let id = authority.register_root(&path)?;
            Ok(json!({"v":1,"type":"root_registered","root_id":id}))
        }
        AdminRequest::InspectGrant { device_id, .. } => {
            let review = authority.inspect_grant(pairing, &device_id)?;
            Ok(
                json!({"v":1,"type":"grant_inspection","device_id":device_id,
                "grant_id":review.grant_id,"grant_revision":review.grant_revision,
                "scope":review.scope,"scope_digest":review.scope_digest}),
            )
        }
        AdminRequest::ConfirmLegacyProject {
            device_id,
            grant_id,
            grant_revision,
            scope_digest,
            project_id,
            ..
        } => {
            authority.confirm_legacy_project(
                pairing,
                &device_id,
                &grant_id,
                grant_revision,
                &scope_digest,
                &project_id,
            )?;
            Ok(
                json!({"v":1,"type":"legacy_project_confirmed","device_id":device_id,"project_id":project_id}),
            )
        }
        _ => Err(Refusal::Malformed),
    }
}

fn source_result(proof: &super::source::SourceProof) -> Value {
    json!({"v":1,"type":"run_source","run_id":proof.run_id,"project_id":proof.project_id,
        "source_id":proof.source_id,"snapshot_digest":proof.snapshot_digest,
        "base_digest":proof.base_digest,"base_commit":proof.base_commit})
}

fn checkout_result(proof: &super::source_checkout::CheckoutProof) -> Value {
    json!({"v":1,"type":"run_checkout","run_id":proof.run_id,"project_id":proof.project_id,
        "source_id":proof.source_id,"checkout_id":proof.checkout_id,
        "snapshot_digest":proof.snapshot_digest,"head":proof.head,"reviewed":proof.reviewed})
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]
mod tests {
    use std::{sync::Arc, time::Instant};

    use serde_json::{Value, json};
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixStream,
    };

    use super::super::authority::Authority;
    use crate::PairingRegistry;

    async fn exchange(path: &std::path::Path, message: Value) -> Value {
        let mut stream = UnixStream::connect(path).await.unwrap();
        stream
            .write_all(message.to_string().as_bytes())
            .await
            .unwrap();
        stream.write_all(b"\n").await.unwrap();
        let mut line = String::new();
        BufReader::new(stream).read_line(&mut line).await.unwrap();
        serde_json::from_str(&line).unwrap()
    }

    async fn assert_project_admin(
        path: &std::path::Path,
        project: &std::path::Path,
        token: &str,
        device: &str,
        approved: &Value,
    ) {
        let denied = exchange(
            path,
            json!({"v":1,"type":"project_roots","capability":"wrong","after":""}),
        )
        .await;
        assert_eq!(denied["code"], "unauthenticated");
        let roots = exchange(
            path,
            json!({"v":1,"type":"project_roots","capability":token,"after":""}),
        )
        .await;
        assert_eq!(roots["type"], "project_roots");
        assert_eq!(roots["roots"][0]["id"], approved["project_root_ids"][0]);
        let malformed = exchange(
            path,
            json!({"v":1,"type":"project_roots","capability":token,"after":"bad"}),
        )
        .await;
        assert_eq!(malformed["code"], "malformed");
        let foreign = tempfile::tempdir().unwrap();
        let denied = exchange(
            path,
            json!({"v":1,"type":"register_project","capability":token,
            "path":foreign.path().to_str().unwrap(),"root_ids":[roots["roots"][0]["id"]]}),
        )
        .await;
        assert_eq!(denied["code"], "out_of_scope");
        let added_root = exchange(
            path,
            json!({"v":1,"type":"register_root","capability":token,
            "path":foreign.path().to_str().unwrap()}),
        )
        .await;
        assert_eq!(added_root["type"], "root_registered");
        assert_eq!(added_root["root_id"].as_str().unwrap().len(), 32);
        let registered = exchange(
            path,
            json!({"v":1,"type":"register_project","capability":token,
            "path":project.to_str().unwrap(),"root_ids":[roots["roots"][0]["id"]]}),
        )
        .await;
        assert_eq!(registered["type"], "project_registered");
        assert_eq!(registered["project_id"].as_str().unwrap().len(), 32);
        let denied = exchange(
            path,
            json!({"v":1,"type":"projects","capability":"wrong","after":""}),
        )
        .await;
        assert_eq!(denied["code"], "unauthenticated");
        let projects = exchange(
            path,
            json!({"v":1,"type":"projects","capability":token,"after":""}),
        )
        .await;
        assert_eq!(projects["projects"][0]["id"], registered["project_id"]);
        let malformed = exchange(
            path,
            json!({"v":1,"type":"projects","capability":token,"after":"bad"}),
        )
        .await;
        assert_eq!(malformed["code"], "malformed");
        let new_root = exchange(
            path,
            json!({"v":1,"type":"register_root","capability":token,
                "path":project.to_str().unwrap()}),
        )
        .await;
        let denied = exchange(
            path,
            json!({"v":1,"type":"link_project_roots","capability":"wrong",
                "project_id":registered["project_id"],"root_ids":[new_root["root_id"]]}),
        )
        .await;
        assert_eq!(denied["code"], "unauthenticated");
        let linked = exchange(
            path,
            json!({"v":1,"type":"link_project_roots","capability":token,
                "project_id":registered["project_id"],"root_ids":[new_root["root_id"]]}),
        )
        .await;
        assert_eq!(linked["type"], "project_roots_linked");
        assert_source_admin(path, project, token, &registered["project_id"]).await;
        let review = exchange(
            path,
            json!({"v":1,"type":"inspect_grant","capability":token,"device_id":device}),
        )
        .await;
        assert_eq!(review["type"], "grant_inspection");
        assert_eq!(review["grant_id"], approved["grant_id"]);
        let confirm = exchange(path, json!({"v":1,"type":"confirm_legacy_project","capability":token,
            "device_id":device,"grant_id":review["grant_id"],"grant_revision":review["grant_revision"],
            "scope_digest":review["scope_digest"],"project_id":registered["project_id"]})).await;
        assert_eq!(confirm["code"], "unsupported");
    }

    async fn assert_source_admin(
        path: &std::path::Path,
        project: &std::path::Path,
        token: &str,
        project_id: &Value,
    ) {
        let denied = exchange(
            path,
            json!({"v":1,"type":"freeze_run_source","capability":"wrong",
                "project_id":project_id,"run_id":"11111111-1111-4111-8111-111111111111",
                "snapshot":project.join("snapshot").to_str().unwrap(),"includes":[]}),
        )
        .await;
        assert_eq!(denied["code"], "unauthenticated");
        let absent = exchange(
            path,
            json!({"v":1,"type":"inspect_run_source","capability":token,
                "run_id":"11111111-1111-4111-8111-111111111111"}),
        )
        .await;
        assert_eq!(absent["code"], "unavailable");
    }

    #[tokio::test]
    async fn prepared_owner_requires_authentication_and_explicit_run_support() {
        let root = tempfile::tempdir().unwrap();
        let authority =
            Arc::new(Authority::open(&root.path().join("grants/state"), &"01".repeat(32)).unwrap());
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let path = root.path().join("operator.sock");
        let capability = root.path().join("operator-capability");
        let server = super::AdminSocket::bind(&path, &capability, authority, pairing)
            .await
            .unwrap();
        let token = std::fs::read_to_string(&capability).unwrap();
        let task = tokio::spawn(server.serve());
        let denied = exchange(
            &path,
            json!({"v":1,"type":"list_prepared","capability":"wrong"}),
        )
        .await;
        assert_eq!(denied["code"], "unauthenticated");
        let absent = exchange(
            &path,
            json!({"v":1,"type":"list_prepared","capability":token}),
        )
        .await;
        assert_eq!(absent["code"], "unsupported");
        let waiting = exchange(
            &path,
            json!({"v":1,"type":"open_prepared","capability":token,
            "registration_id":"22".repeat(16),"revision":1}),
        )
        .await;
        assert_eq!(waiting["code"], "unsupported");
        let admission = exchange(
            &path,
            json!({"v":1,"type":"admit_prepared","capability":token,
            "registration_id":"22".repeat(16),"revision":1,"selection_sha256":"33".repeat(32),
            "generated_work_max":3,"park_ttl_ms":3_600_000}),
        )
        .await;
        assert_eq!(admission["code"], "unsupported");
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    async fn assert_checkout_admin(
        path: &std::path::Path,
        root: &std::path::Path,
        token: &str,
        source: &Value,
    ) -> Value {
        let request = |capability: &str, source_id: &Value, checkout: &std::path::Path| {
            json!({
                "v":1,"type":"prepare_run_checkout","capability":capability,
                "run_id":source["run_id"],"source_id":source_id,"checkout":checkout.to_str().unwrap()
            })
        };
        let denied = exchange(
            path,
            request("wrong", &source["source_id"], &root.join("checkout")),
        )
        .await;
        assert_eq!(denied["code"], "unauthenticated");
        let stale = exchange(
            path,
            request(token, &json!("ff".repeat(16)), &root.join("checkout")),
        )
        .await;
        assert_eq!(stale["code"], "stale_subject");
        let checkout = exchange(
            path,
            request(token, &source["source_id"], &root.join("checkout")),
        )
        .await;
        assert_eq!(checkout["type"], "run_checkout");
        assert_eq!(checkout["source_id"], source["source_id"]);
        assert_eq!(checkout["reviewed"], false);
        assert_eq!(
            std::fs::read(root.join("checkout/source")).unwrap(),
            b"original"
        );
        let duplicate = exchange(
            path,
            request(token, &source["source_id"], &root.join("other-checkout")),
        )
        .await;
        assert_eq!(duplicate["code"], "stale_subject");
        checkout
    }

    #[tokio::test]
    #[ignore = "requires the optional root-owned installed skills integration"]
    #[expect(
        clippy::too_many_lines,
        reason = "one end-to-end owner lifecycle verifies source and checkout across restarts"
    )]
    async fn operator_source_freeze_survives_project_removal_and_restart() {
        use sha2::{Digest, Sha256};
        use std::{os::unix::fs::PermissionsExt, process::Command};

        let root = tempfile::tempdir().unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let project = root.path().join("project");
        std::fs::create_dir(&project).unwrap();
        let git = |args: &[&str]| {
            assert!(
                Command::new("/usr/bin/git")
                    .current_dir(&project)
                    .env("GIT_CONFIG_NOSYSTEM", "1")
                    .env("GIT_CONFIG_GLOBAL", "/dev/null")
                    .args(args)
                    .status()
                    .unwrap()
                    .success()
            );
        };
        git(&["init", "--quiet"]);
        std::fs::write(project.join("source"), "original").unwrap();
        git(&["add", "--", "source"]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "commit",
            "--quiet",
            "-m",
            "initial",
        ]);
        let grants = root.path().join("grants/state");
        let authority = Arc::new(Authority::open(&grants, &"01".repeat(32)).unwrap());
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let path = root
            .path()
            .join("louiselm/workflow/phone-control-admin.sock");
        let capability = root.path().join("louiselm/workflow/operator-capability");
        let server = super::AdminSocket::bind(&path, &capability, authority, pairing.clone())
            .await
            .unwrap();
        let token = std::fs::read_to_string(&capability).unwrap();
        let task = tokio::spawn(server.serve());
        let selected = exchange(
            &path,
            json!({"v":1,"type":"register_root","capability":token,
            "path":project.to_str().unwrap()}),
        )
        .await;
        let registered = exchange(
            &path,
            json!({"v":1,"type":"register_project","capability":token,
            "path":project.to_str().unwrap(),"root_ids":[selected["root_id"]]}),
        )
        .await;
        let run_id = "11111111-1111-4111-8111-111111111111";
        let frozen = exchange(
            &path,
            json!({"v":1,"type":"freeze_run_source","capability":token,
            "project_id":registered["project_id"],"run_id":run_id,
            "snapshot":root.path().join("snapshot").to_str().unwrap(),"includes":[]}),
        )
        .await;
        assert_eq!(frozen["type"], "run_source");
        let duplicate = exchange(
            &path,
            json!({"v":1,"type":"freeze_run_source","capability":token,
            "project_id":registered["project_id"],"run_id":run_id,
            "snapshot":root.path().join("other").to_str().unwrap(),"includes":[]}),
        )
        .await;
        assert_eq!(duplicate["code"], "stale_subject");
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        std::fs::rename(&project, root.path().join("moved-project")).unwrap();
        let authority = Arc::new(Authority::open(&grants, &"01".repeat(32)).unwrap());
        let runs = crate::RunStore::new(root.path().join("runs")).unwrap();
        let prepared = super::PreparedOwner::new(root.path().join("grants"), runs.clone());
        let server = super::AdminSocket::bind(&path, &capability, authority, pairing)
            .await
            .unwrap();
        let task = tokio::spawn(server.serve());
        let restored = exchange(
            &path,
            json!({"v":1,"type":"inspect_run_source","capability":token,
            "run_id":run_id}),
        )
        .await;
        assert_eq!(restored, frozen);
        let checkout = assert_checkout_admin(&path, root.path(), &token, &restored).await;
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        let authority = Arc::new(Authority::open(&grants, &"01".repeat(32)).unwrap());
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let server = super::AdminSocket::bind(&path, &capability, authority, pairing)
            .await
            .unwrap()
            .with_prepared(Some(prepared));
        let task = tokio::spawn(server.serve());
        let restored_checkout = exchange(
            &path,
            json!({"v":1,"type":"inspect_run_checkout",
            "capability":token,"run_id":run_id}),
        )
        .await;
        assert_eq!(restored_checkout, checkout);
        let selection_path = root.path().join("selection.json");
        let selection = super::super::prepared_owner_tests::selection(
            root.path(),
            checkout["head"].as_str().unwrap(),
            "private instructions",
        );
        std::fs::write(&selection_path, &selection).unwrap();
        std::fs::set_permissions(&selection_path, std::fs::Permissions::from_mode(0o600)).unwrap();
        let request = |capability: &str| {
            json!({"v":1,"type":"publish_prepared","capability":capability,
            "run_id":run_id,"source_id":restored["source_id"],"checkout_id":checkout["checkout_id"],
            "selection":selection_path.to_str().unwrap(),"registration_id":"","expected_revision":0})
        };
        assert_eq!(
            exchange(&path, request("wrong")).await["code"],
            "unauthenticated"
        );
        std::fs::write(root.path().join("checkout/source"), "dirty").unwrap();
        assert_eq!(
            exchange(&path, request(&token)).await["code"],
            "unavailable"
        );
        std::fs::write(root.path().join("checkout/source"), "original").unwrap();
        let registration = exchange(&path, request(&token)).await;
        assert_eq!(registration["type"], "prepared_registered");
        assert!(runs.run(run_id).is_err());
        let list = exchange(
            &path,
            json!({"v":1,"type":"list_prepared","capability":token}),
        )
        .await;
        assert_eq!(list["items"].as_array().unwrap().len(), 1);
        assert!(!list.to_string().contains("private instructions"));
        assert!(!list.to_string().contains(selection_path.to_str().unwrap()));
        std::fs::remove_file(&selection_path).unwrap();
        let inspect = || {
            json!({"v":1,"type":"inspect_prepared","capability":token,
            "registration_id":registration["registration_id"],"revision":registration["revision"]})
        };
        let saved = exchange(&path, inspect()).await;
        assert_eq!(saved["selection"], selection);
        let waiting_request = |capability: &str, revision: u64| {
            json!({"v":1,"type":"open_prepared",
            "capability":capability,"registration_id":registration["registration_id"],"revision":revision})
        };
        assert_eq!(
            exchange(&path, waiting_request("wrong", 1)).await["code"],
            "unauthenticated"
        );
        assert_eq!(
            exchange(&path, waiting_request(&token, 2)).await["code"],
            "stale_subject"
        );
        let waiting = exchange(&path, waiting_request(&token, 1)).await;
        assert_eq!(waiting["type"], "prepared_waiting");
        assert_eq!(waiting["selection"], selection);
        assert_eq!(waiting["binding"]["run_id"], run_id);
        assert!(runs.run(run_id).is_err());
        let selection_sha256 = crate::hex::lower_hex(&Sha256::digest(selection.as_bytes()));
        let admission_request = |capability: &str, digest: &str| {
            json!({"v":1,"type":"admit_prepared",
            "capability":capability,"registration_id":registration["registration_id"],
                "revision":registration["revision"],"selection_sha256":digest,
                "generated_work_max":3,"park_ttl_ms":3_600_000})
        };
        assert_eq!(
            exchange(&path, admission_request("wrong", &selection_sha256)).await["code"],
            "unauthenticated"
        );
        assert_eq!(
            exchange(&path, admission_request(&token, &"00".repeat(32))).await["code"],
            "malformed"
        );
        let mut wrong_limit = admission_request(&token, &selection_sha256);
        wrong_limit["generated_work_max"] = 4.into();
        assert_eq!(exchange(&path, wrong_limit).await["code"], "malformed");
        assert!(runs.run(run_id).is_err());
        let admitted = if let Some(binary) = std::env::var_os("LOUISELM_TEST_CAPTURE") {
            let state = root.path().to_path_buf();
            let registration_id = registration["registration_id"].as_str().unwrap().to_owned();
            let selection_sha256 = selection_sha256.clone();
            tokio::task::spawn_blocking(move || {
                let output = Command::new(binary)
                    .args([
                        "--require-interface=1",
                        "phone-control",
                        "admit-prepared",
                        "--registration-id",
                        &registration_id,
                        "--revision",
                        "1",
                        "--selection-sha256",
                        &selection_sha256,
                        "--generated-work-max",
                        "3",
                        "--park-ttl-ms",
                        "3600000",
                    ])
                    .env("LOUISELM_CAPTURE_STATE_DIR", state)
                    .output()
                    .unwrap();
                assert!(output.status.success());
                serde_json::from_slice::<Value>(&output.stdout).unwrap()
            })
            .await
            .unwrap()
        } else {
            exchange(&path, admission_request(&token, &selection_sha256)).await
        };
        assert_eq!(admitted["type"], "prepared_admitted");
        assert_eq!(admitted["run_id"], run_id);
        assert!(uuid::Uuid::parse_str(admitted["token"].as_str().unwrap()).is_ok());
        let bound = runs.run(run_id).unwrap();
        assert_eq!(bound.state, "admitted");
        assert!(bound.session_id.is_none());
        assert_eq!(bound.phone_binding.unwrap().agents, vec!["codex"]);
        assert_eq!(
            exchange(&path, waiting_request(&token, 1)).await["code"],
            "stale_subject"
        );
        let removed = exchange(&path, json!({"v":1,"type":"unpublish_prepared","capability":token,
            "registration_id":registration["registration_id"],"expected_revision":registration["revision"]})).await;
        assert_eq!(removed["type"], "prepared_unpublished");
        assert_eq!(
            exchange(&path, waiting_request(&token, 1)).await["code"],
            "stale_subject"
        );
        assert_eq!(exchange(&path, inspect()).await["selection"], selection);
        assert_eq!(
            exchange(
                &path,
                json!({"v":1,"type":"list_prepared","capability":token})
            )
            .await["items"]
                .as_array()
                .unwrap()
                .as_slice(),
            &[] as &[Value]
        );
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
    }

    #[tokio::test]
    async fn operator_socket_requires_capability_and_forget_revokes_both() {
        let root = tempfile::tempdir().unwrap();
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let identity = "01".repeat(32);
        let offer = pairing
            .issue("https://192.168.1.2:7391", &identity, 1_000, 10_000)
            .unwrap();
        let phone = pairing.consume(&offer.token, "phone", 1_001).unwrap();
        let authority =
            Arc::new(Authority::open(&root.path().join("grants/state"), &identity).unwrap());
        let path = root.path().join("operator.sock");
        let capability = root.path().join("operator-capability");
        let server =
            super::AdminSocket::bind(&path, &capability, authority.clone(), pairing.clone())
                .await
                .unwrap();
        let token = std::fs::read_to_string(&capability).unwrap();
        let task = tokio::spawn(server.serve());
        let wrong = exchange(
            &path,
            json!({"v":1,"type":"start","capability":"wrong","device_id":phone.device_id}),
        )
        .await;
        assert_eq!(wrong["code"], "unauthenticated");
        let started = exchange(
            &path,
            json!({"v":1,"type":"start","capability":token,"device_id":phone.device_id}),
        )
        .await;
        assert_eq!(started["type"], "enrollment_started");
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../android/app/src/test/resources/phone-control-v1.json"
        ))
        .unwrap();
        let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
        authority
            .submit(
                &phone.device_id,
                started["enrollment_id"].as_str().unwrap(),
                started["matching_code"].as_str().unwrap(),
                key,
                Instant::now(),
            )
            .unwrap();
        let old_approval = exchange(&path, json!({"v":1,"type":"approve","capability":token,
            "device_id":phone.device_id,"enrollment_id":started["enrollment_id"],"enrolled_key":key,
            "scope":{"projects":[root.path().to_str().unwrap()],"agents":["codex"],"groups":["read"]},"project_root_ids":[]})).await;
        assert_eq!(old_approval["code"], "unsupported");
        let approved = exchange(&path, json!({"v":2,"type":"approve","capability":token,
            "device_id":phone.device_id,"enrollment_id":started["enrollment_id"],"enrolled_key":key,
            "scope":{"projects":[root.path().to_str().unwrap()],"agents":["codex"],"groups":["read"]},"project_root_ids":[]})).await;
        assert_eq!(approved["type"], "enrollment_approved");
        assert_eq!(approved["v"], 2);
        assert_project_admin(&path, root.path(), &token, &phone.device_id, &approved).await;
        let disabled = exchange(
            &path,
            json!({"v":1,"type":"disable","capability":token,"device_id":phone.device_id}),
        )
        .await;
        assert_eq!(disabled["type"], "disabled");
        assert_eq!(
            pairing.authenticate_device(&phone.credential).unwrap(),
            Some(phone.device_id.clone())
        );
        assert_eq!(
            authority.challenge(
                &phone.device_id,
                approved["grant_id"].as_str().unwrap(),
                approved["grant_revision"].as_u64().unwrap(),
                Instant::now(),
                1_000
            ),
            Err(super::super::authority::Refusal::Revoked)
        );
        let forgotten = exchange(
            &path,
            json!({"v":1,"type":"forget","capability":token,"device_id":phone.device_id}),
        )
        .await;
        assert_eq!(forgotten["type"], "forgotten");
        assert_eq!(
            pairing.authenticate_device(&phone.credential).unwrap(),
            None
        );
        assert_eq!(
            authority.challenge(
                &phone.device_id,
                approved["grant_id"].as_str().unwrap(),
                approved["grant_revision"].as_u64().unwrap(),
                Instant::now(),
                1_000
            ),
            Err(super::super::authority::Refusal::Revoked)
        );
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn controller_attach_requires_capability_and_allocates_fresh_identity() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("controller.sock");
        let capability = root.path().join("operator-capability");
        let registry = Arc::new(super::ControllerRegistry::default());
        let server = super::ControllerSocket::bind(&path, &capability, registry.clone())
            .await
            .unwrap();
        let token = std::fs::read_to_string(&capability).unwrap();
        let task = tokio::spawn(server.serve());
        let denied = exchange(&path, json!({"v":1,"type":"attach","capability":"wrong"})).await;
        assert_eq!(denied["code"], "unauthenticated");
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream
            .write_all(
                format!("{}\n", json!({"v":1,"type":"attach","capability":token})).as_bytes(),
            )
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(&mut stream)
            .read_line(&mut line)
            .await
            .unwrap();
        let first: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(first["type"], "attached");
        let id = first["controller_id"].as_str().unwrap();
        assert!(registry.contains(id));
        let second = exchange(&path, json!({"v":1,"type":"attach","capability":token})).await;
        assert_eq!(second["type"], "attached");
        assert_ne!(second["controller_id"], id);
        let mut extra = UnixStream::connect(&path).await.unwrap();
        let pipelined = format!(
            "{}\n{{}}\n",
            json!({"v":1,"type":"attach","capability":token})
        );
        extra.write_all(pipelined.as_bytes()).await.unwrap();
        let mut attached = String::new();
        BufReader::new(&mut extra)
            .read_line(&mut attached)
            .await
            .unwrap();
        let pipelined_reply: Value = serde_json::from_str(&attached).unwrap();
        let pipelined_id = pipelined_reply["controller_id"].as_str().unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while registry.contains(pipelined_id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        drop(stream);
        task.abort();
        let _ = task.await;
    }

    #[tokio::test]
    async fn attached_controller_receives_one_read_and_returns_its_response() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("controller.sock");
        let capability = root.path().join("operator-capability");
        let registry = Arc::new(super::ControllerRegistry::default());
        let server = super::ControllerSocket::bind(&path, &capability, registry.clone())
            .await
            .unwrap();
        let token = std::fs::read_to_string(&capability).unwrap();
        let task = tokio::spawn(server.serve());
        let mut stream = UnixStream::connect(&path).await.unwrap();
        stream
            .write_all(
                format!("{}\n", json!({"v":1,"type":"attach","capability":token})).as_bytes(),
            )
            .await
            .unwrap();
        let mut line = String::new();
        let mut reader = BufReader::new(&mut stream);
        reader.read_line(&mut line).await.unwrap();
        let attached: Value = serde_json::from_str(&line).unwrap();
        let id = attached["controller_id"].as_str().unwrap().to_owned();
        let dispatcher = registry.clone();
        let controller = id.clone();
        let pending = tokio::spawn(async move {
            dispatcher
                .dispatch(&controller, b"{\"v\":1,\"type\":\"read_session\"}".to_vec())
                .await
        });
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        assert_eq!(line.trim_end(), "{\"v\":1,\"type\":\"read_session\"}");
        drop(reader);
        stream
            .write_all(b"{\"v\":1,\"type\":\"error\",\"code\":\"stale_subject\"}\n")
            .await
            .unwrap();
        assert_eq!(
            pending.await.unwrap().unwrap(),
            b"{\"v\":1,\"type\":\"error\",\"code\":\"stale_subject\"}"
        );
        drop(stream);
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while registry.contains(&id) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(registry.dispatch(&id, b"{}".to_vec()).await.is_err());
        task.abort();
        let _ = task.await;
    }

    #[test]
    fn controller_reply_must_be_closed_bounded_and_match_the_signed_request() {
        let controller = "01".repeat(16);
        let subject = "02".repeat(16);
        let operation = "03".repeat(16);
        let request: super::super::Request = serde_json::from_value(json!({
            "grant_id":"04".repeat(16), "grant_revision":1, "period_id":"05".repeat(16),
            "receiver_identity":"06".repeat(32), "controller_id":controller, "subject_id":subject,
            "action":1, "payload":"00000000000000000032", "payload_hash":"07".repeat(32),
            "operation_id":operation, "expected_revision":0, "interaction":1, "signature":"3006020101020101"
        })).unwrap();
        let page = json!({"v":2,"type":"session_page","controller_id":request.controller_id,
            "subject_id":request.subject_id,"operation_id":request.operation_id,"revision":1,"status":"ready",
            "entries":[{"kind":"message","seq":1,"role":"assistant","text":"hello","truncated":false}],
            "next_before":0,"complete":false,"source":{"replay":"unverified","archive":"unavailable",
                "redaction":"present","missing":true,"truncated":false}});
        assert_eq!(
            super::validate_read_response(&serde_json::to_vec(&page).unwrap(), &request).unwrap(),
            page
        );
        let mut older_request = request.clone();
        older_request.payload = "000000000000000a0032".into();
        older_request.expected_revision = 1;
        let mut skipped = page.clone();
        skipped["entries"] = json!([]);
        skipped["next_before"] = json!(9);
        assert_eq!(
            super::validate_read_response(&serde_json::to_vec(&skipped).unwrap(), &older_request)
                .unwrap(),
            skipped
        );
        skipped["entries"] =
            json!([{"kind":"message","seq":9,"role":"user","text":"visible","truncated":false}]);
        skipped["next_before"] = json!(8);
        assert_eq!(
            super::validate_read_response(&serde_json::to_vec(&skipped).unwrap(), &older_request)
                .unwrap(),
            skipped
        );
        skipped["entries"] = json!([]);
        skipped["next_before"] = json!(10);
        assert_eq!(
            super::validate_read_response(&serde_json::to_vec(&skipped).unwrap(), &older_request),
            Err(super::Refusal::Unavailable)
        );
        let mut wrong = page.clone();
        wrong["subject_id"] = json!("08".repeat(16));
        assert_eq!(
            super::validate_read_response(&serde_json::to_vec(&wrong).unwrap(), &request),
            Err(super::Refusal::Unavailable)
        );
        wrong = page.clone();
        wrong["entries"][0]["secret"] = json!("not a field");
        assert_eq!(
            super::validate_read_response(&serde_json::to_vec(&wrong).unwrap(), &request),
            Err(super::Refusal::Unavailable)
        );
        wrong = page.clone();
        wrong["source"]["redaction"] = json!("unknown-mode");
        assert_eq!(
            super::validate_read_response(&serde_json::to_vec(&wrong).unwrap(), &request),
            Err(super::Refusal::Unavailable)
        );
        wrong = page.clone();
        wrong["source"]["private"] = json!(true);
        assert_eq!(
            super::validate_read_response(&serde_json::to_vec(&wrong).unwrap(), &request),
            Err(super::Refusal::Unavailable)
        );
        wrong = page.clone();
        wrong["complete"] = json!(true);
        assert_eq!(
            super::validate_read_response(&serde_json::to_vec(&wrong).unwrap(), &request),
            Err(super::Refusal::Unavailable)
        );
        let duplicate = page
            .to_string()
            .replace("\"revision\":1", "\"revision\":1,\"revision\":1");
        assert_eq!(
            super::validate_read_response(duplicate.as_bytes(), &request),
            Err(super::Refusal::Unavailable)
        );
        assert_eq!(
            super::validate_read_response(
                br#"{"v":1,"type":"error","code":"stale_subject"}"#,
                &request
            ),
            Err(super::Refusal::StaleSubject)
        );
    }

    #[tokio::test]
    #[expect(
        clippy::used_underscore_binding,
        reason = "The test retains the socket lock descriptor to prove explicit unlock."
    )]
    async fn socket_lock_is_explicitly_released_with_a_cloned_descriptor_open() {
        let root = tempfile::tempdir().unwrap();
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let authority =
            Arc::new(Authority::open(&root.path().join("grants/state"), &"01".repeat(32)).unwrap());
        let path = root.path().join("operator.sock");
        let capability = root.path().join("operator-capability");
        let first =
            super::AdminSocket::bind(&path, &capability, authority.clone(), pairing.clone())
                .await
                .unwrap();
        let cloned = first.socket._lock.0.try_clone().unwrap();
        drop(first);
        let second = super::AdminSocket::bind(&path, &capability, authority, pairing)
            .await
            .unwrap();
        drop(second);
        drop(cloned);
    }
}
