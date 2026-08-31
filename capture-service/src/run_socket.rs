//! Owner-only local observation and operator channel for durable Runs.

use crate::time::now_ms;
use crate::{RunStore, RunStoreError, RunView};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    time::Duration,
};
use subtle::ConstantTimeEq;
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

const RESUME_LEASE_MS: u64 = 5 * 60 * 1_000;

/// Failure while binding or serving the local Run socket.
#[derive(Debug, Error)]
pub enum RunSocketError {
    #[error("Run socket I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error(transparent)]
    Run(#[from] RunStoreError),
    #[error("Run socket protocol failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// One newline-delimited server message with no secret capability material.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunSocketMessage {
    Snapshot { runs: Vec<RunView> },
    RunChanged { id: String, revision: u64 },
    MutationResult { request_id: String, run: RunView },
    MutationError { request_id: String, message: String },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Snapshot,
    Raise {
        request_id: String,
        id: String,
        expected_revision: u64,
        ceiling: u64,
        capability: String,
    },
    Resume {
        request_id: String,
        id: String,
        expected_revision: u64,
        operation_id: String,
        capability: String,
    },
    FinalizeResume {
        request_id: String,
        id: String,
        expected_revision: u64,
        operation_id: String,
        succeeded: bool,
        capability: String,
    },
}

/// Single owner of the local Run listener and operator capability.
pub struct RunSocket {
    listener: UnixListener,
    store: RunStore,
    operator_token_sha256: String,
    _lock: File,
}

impl RunSocket {
    /// Bind an owner-only listener, refusing live or non-socket collisions.
    pub async fn bind(
        path: impl AsRef<Path>,
        capability_path: impl AsRef<Path>,
        store: RunStore,
    ) -> Result<Self, RunSocketError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(PathBuf::from(format!("{}.lock", path.display())))?;
        lock.try_lock_exclusive()
            .map_err(|_| io::Error::new(io::ErrorKind::AddrInUse, "Run socket is already owned"))?;
        let operator_token = load_or_create_capability(capability_path.as_ref())?;
        if path.exists() {
            if UnixStream::connect(path).await.is_ok() {
                return Err(io::Error::new(io::ErrorKind::AddrInUse, "Run socket is live").into());
            }
            remove_stale_socket(path)?;
        }
        let listener = UnixListener::bind(path)?;
        set_owner_only(path)?;
        Ok(Self {
            listener,
            store,
            operator_token_sha256: token_sha256(&operator_token),
            _lock: lock,
        })
    }

    /// Accept clients until the owning task is cancelled.
    pub async fn serve(self) -> Result<(), RunSocketError> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            let store = self.store.clone();
            let operator_token_sha256 = self.operator_token_sha256.clone();
            tokio::spawn(async move {
                let _ = serve_client(stream, store, operator_token_sha256).await;
            });
        }
    }
}

async fn serve_client(
    stream: UnixStream,
    store: RunStore,
    operator_hash: String,
) -> Result<(), RunSocketError> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let initial = store.snapshot()?;
    let mut known = revisions(&initial);
    write_message(&mut writer, &RunSocketMessage::Snapshot { runs: initial }).await?;
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    interval.tick().await;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()); };
                match serde_json::from_str::<ClientMessage>(&line)? {
                    ClientMessage::Snapshot => {
                        let runs = store.snapshot()?;
                        known = revisions(&runs);
                        write_message(&mut writer, &RunSocketMessage::Snapshot { runs }).await?;
                    }
                    request => handle_mutation(&mut writer, &store, &operator_hash, request).await?,
                }
            }
            _ = interval.tick() => {
                let current = store.snapshot()?;
                for run in &current {
                    if known.get(&run.id).copied() != Some(run.revision) {
                        write_message(&mut writer, &RunSocketMessage::RunChanged { id: run.id.clone(), revision: run.revision }).await?;
                    }
                }
                known = revisions(&current);
            }
        }
    }
}

async fn handle_mutation(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    store: &RunStore,
    operator_hash: &str,
    request: ClientMessage,
) -> Result<(), RunSocketError> {
    let (request_id, capability) = match &request {
        ClientMessage::Raise {
            request_id,
            capability,
            ..
        }
        | ClientMessage::Resume {
            request_id,
            capability,
            ..
        }
        | ClientMessage::FinalizeResume {
            request_id,
            capability,
            ..
        } => (request_id.clone(), capability),
        ClientMessage::Snapshot => unreachable!("snapshot handled separately"),
    };
    if request_id.is_empty() || !verify_capability(operator_hash, capability) {
        return write_message(
            writer,
            &RunSocketMessage::MutationError {
                request_id,
                message: "operator capability is invalid".to_owned(),
            },
        )
        .await;
    }
    let result = match request {
        ClientMessage::Raise {
            id,
            expected_revision,
            ceiling,
            ..
        } => store.raise_generated_work_ceiling(&id, expected_revision, ceiling),
        ClientMessage::Resume {
            id,
            expected_revision,
            operation_id,
            ..
        } => store
            .begin_resume(
                &id,
                expected_revision,
                &operation_id,
                now_ms(),
                RESUME_LEASE_MS,
            )
            .and_then(|_| store.view(&id)),
        ClientMessage::FinalizeResume {
            id,
            expected_revision,
            operation_id,
            succeeded,
            ..
        } => store
            .finalize_resume(&id, expected_revision, &operation_id, succeeded)
            .and_then(|_| store.view(&id)),
        ClientMessage::Snapshot => unreachable!("snapshot handled separately"),
    };
    let message = match result {
        Ok(run) => RunSocketMessage::MutationResult { request_id, run },
        Err(error) => RunSocketMessage::MutationError {
            request_id,
            message: error.to_string(),
        },
    };
    write_message(writer, &message).await
}

fn revisions(runs: &[RunView]) -> BTreeMap<String, u64> {
    runs.iter()
        .map(|run| (run.id.clone(), run.revision))
        .collect()
}

async fn write_message(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    message: &RunSocketMessage,
) -> Result<(), RunSocketError> {
    writer.write_all(&serde_json::to_vec(message)?).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}

#[cfg(unix)]
fn remove_stale_socket(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::FileTypeExt;
    if !fs::symlink_metadata(path)?.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "Run socket path is not a socket",
        ));
    }
    fs::remove_file(path)
}

#[cfg(unix)]
fn set_owner_only(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

fn load_or_create_capability(path: &Path) -> io::Result<String> {
    if path.exists() {
        if fs::symlink_metadata(path)?.file_type().is_symlink() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "operator capability is a symlink",
            ));
        }
        set_owner_only(path)?;
        return valid_capability(fs::read_to_string(path)?.trim());
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let token = uuid::Uuid::new_v4().to_string();
    let temporary = path.with_extension(format!("tmp-{}", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temporary)?;
    set_owner_only(&temporary)?;
    file.write_all(token.as_bytes())?;
    file.sync_all()?;
    drop(file);
    fs::rename(&temporary, path)?;
    Ok(token)
}

fn valid_capability(value: &str) -> io::Result<String> {
    let parsed = uuid::Uuid::parse_str(value).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidData, "operator capability is invalid")
    })?;
    if parsed.to_string() != value {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "operator capability is not canonical",
        ));
    }
    Ok(value.to_owned())
}

fn token_sha256(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}
fn verify_capability(expected_hash: &str, supplied: &str) -> bool {
    expected_hash
        .as_bytes()
        .ct_eq(token_sha256(supplied).as_bytes())
        .into()
}
