//! Owner-only local observation channel for durable Run invalidations.

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io,
    path::{Path, PathBuf},
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
};

use crate::{RunStore, RunStoreError, RunView};

/// Failure while binding or serving the local Run observation socket.
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
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum ClientMessage {
    Snapshot,
}

/// Single owner of the local Run observation listener.
pub struct RunSocket {
    listener: UnixListener,
    store: RunStore,
    _lock: File,
}

impl RunSocket {
    /// Bind an owner-only listener, refusing live or non-socket collisions.
    pub async fn bind(path: impl AsRef<Path>, store: RunStore) -> Result<Self, RunSocketError> {
        let path = path.as_ref();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let lock_path = PathBuf::from(format!("{}.lock", path.display()));
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(lock_path)?;
        lock.try_lock_exclusive()
            .map_err(|_| io::Error::new(io::ErrorKind::AddrInUse, "Run socket is already owned"))?;
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
            _lock: lock,
        })
    }

    /// Accept observer clients until the owning task is cancelled.
    pub async fn serve(self) -> Result<(), RunSocketError> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            let store = self.store.clone();
            tokio::spawn(async move {
                let _ = serve_client(stream, store).await;
            });
        }
    }
}

async fn serve_client(stream: UnixStream, store: RunStore) -> Result<(), RunSocketError> {
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
