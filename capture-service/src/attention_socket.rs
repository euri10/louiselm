//! Owner-only local protocol for Attention mutations and snapshots.

use crate::operator_socket::{
    load_or_create_capability, remove_stale_socket, set_owner_only, token_sha256, verify_capability,
};
use crate::{AttentionDraft, AttentionError, AttentionKey, AttentionSnapshot, AttentionStore};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
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

/// Failure while binding or serving the local Attention socket.
#[derive(Debug, Error)]
pub enum AttentionSocketError {
    /// Unix socket or lock operation failed.
    #[error("Attention socket I/O failed: {0}")]
    Io(#[from] io::Error),
    /// Attention persistence failed.
    #[error(transparent)]
    Attention(#[from] AttentionError),
    /// A framed protocol message was malformed.
    #[error("Attention socket protocol failed: {0}")]
    Json(#[from] serde_json::Error),
}

/// One newline-delimited message from the Attention socket.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttentionSocketMessage {
    /// Complete current state, without the operator capability.
    Snapshot { snapshot: AttentionSnapshot },
    /// Invalidation containing only the new generation.
    AttentionChanged { generation: u64 },
    /// Result of one accepted or idempotent mutation.
    MutationResult {
        request_id: String,
        snapshot: AttentionSnapshot,
    },
    /// Rejected mutation with a sanitized message.
    MutationError { request_id: String, message: String },
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum ClientMessage {
    Snapshot,
    Upsert {
        request_id: String,
        attention: AttentionDraft,
        capability: String,
    },
    SetEligible {
        request_id: String,
        key: AttentionKey,
        eligible: bool,
        capability: String,
    },
    Clear {
        request_id: String,
        key: AttentionKey,
        capability: String,
    },
}

/// Single owner of the local Attention listener and operator capability.
pub struct AttentionSocket {
    listener: UnixListener,
    store: AttentionStore,
    operator_token_sha256: String,
    _lock: File,
}

impl AttentionSocket {
    /// Bind an owner-only listener, refusing live or non-socket collisions.
    ///
    /// # Errors
    ///
    /// Returns an error when the socket is already owned or state cannot be read.
    pub async fn bind(
        path: impl AsRef<Path>,
        capability_path: impl AsRef<Path>,
        store: AttentionStore,
    ) -> Result<Self, AttentionSocketError> {
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
        lock.try_lock_exclusive().map_err(|_| {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                "Attention socket is already owned",
            )
        })?;
        let operator_token = load_or_create_capability(capability_path.as_ref())?;
        if path.exists() {
            if UnixStream::connect(path).await.is_ok() {
                return Err(
                    io::Error::new(io::ErrorKind::AddrInUse, "Attention socket is live").into(),
                );
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
    pub async fn serve(self) -> Result<(), AttentionSocketError> {
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
    store: AttentionStore,
    operator_hash: String,
) -> Result<(), AttentionSocketError> {
    let (reader, mut writer) = stream.into_split();
    let mut lines = BufReader::new(reader).lines();
    let initial = store.snapshot()?;
    let mut known_generation = initial.generation;
    write_message(
        &mut writer,
        &AttentionSocketMessage::Snapshot { snapshot: initial },
    )
    .await?;
    let mut interval = tokio::time::interval(Duration::from_millis(50));
    interval.tick().await;
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { return Ok(()); };
                match serde_json::from_str::<ClientMessage>(&line)? {
                    ClientMessage::Snapshot => {
                        let snapshot = store.snapshot()?;
                        known_generation = snapshot.generation;
                        write_message(&mut writer, &AttentionSocketMessage::Snapshot { snapshot }).await?;
                    }
                    request => handle_mutation(&mut writer, &store, &operator_hash, request).await?,
                }
            }
            _ = interval.tick() => {
                let snapshot = store.snapshot()?;
                if snapshot.generation != known_generation {
                    write_message(&mut writer, &AttentionSocketMessage::AttentionChanged { generation: snapshot.generation }).await?;
                    known_generation = snapshot.generation;
                }
            }
        }
    }
}

async fn handle_mutation(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    store: &AttentionStore,
    operator_hash: &str,
    request: ClientMessage,
) -> Result<(), AttentionSocketError> {
    let (request_id, capability) = match &request {
        ClientMessage::Upsert {
            request_id,
            capability,
            ..
        }
        | ClientMessage::SetEligible {
            request_id,
            capability,
            ..
        }
        | ClientMessage::Clear {
            request_id,
            capability,
            ..
        } => (request_id.clone(), capability),
        ClientMessage::Snapshot => unreachable!("snapshot handled separately"),
    };
    if request_id.is_empty()
        || request_id.len() > 128
        || !verify_capability(operator_hash, capability)
    {
        return write_message(
            writer,
            &AttentionSocketMessage::MutationError {
                request_id,
                message: "operator capability is invalid".to_owned(),
            },
        )
        .await;
    }
    let result = match request {
        ClientMessage::Upsert { attention, .. } => store.upsert(attention),
        ClientMessage::SetEligible { key, eligible, .. } => store.set_eligible(key, eligible),
        ClientMessage::Clear { key, .. } => store.clear(key),
        ClientMessage::Snapshot => unreachable!("snapshot handled separately"),
    };
    let message = match result {
        Ok(snapshot) => AttentionSocketMessage::MutationResult {
            request_id,
            snapshot,
        },
        Err(error) => AttentionSocketMessage::MutationError {
            request_id,
            message: error.to_string(),
        },
    };
    write_message(writer, &message).await
}

async fn write_message(
    writer: &mut tokio::net::unix::OwnedWriteHalf,
    message: &AttentionSocketMessage,
) -> Result<(), AttentionSocketError> {
    writer.write_all(&serde_json::to_vec(message)?).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}
