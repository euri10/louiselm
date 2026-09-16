//! UID-pinned broker projections and narrow read-only Run lifecycle facts.

use crate::operator_socket::{remove_stale_socket, verify_capability};
use crate::{AttentionSocketMessage, AttentionStore, BrokerProjection};
use fs2::FileExt;
use serde::Deserialize;
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read},
    os::unix::{
        fs::{MetadataExt, OpenOptionsExt, PermissionsExt},
        net::UnixListener as StdListener,
    },
    path::{Path, PathBuf},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::{UnixListener, UnixStream},
    task::JoinSet,
};

const CONFIG: &str = "/etc/louiselm-capture-broker.json";
const SOCKET: &str = "/run/louiselm-attention/project.sock";
const MAX_FRAME: u64 = 65_536;

/// Root-provisioned public authentication policy; contains no credential bytes.
#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BrokerAttentionConfig {
    /// Dedicated projection socket; never the operator's observer socket.
    pub socket: PathBuf,
    /// Kernel UID allowed to present the producer credential.
    pub broker_uid: u32,
    /// Lowercase SHA-256 of the dedicated broker credential.
    pub capability_sha256: String,
}

impl BrokerAttentionConfig {
    /// Read optional fixed system policy on a blocking worker.
    /// Missing configuration disables only broker projection; invalid policy fails closed.
    ///
    /// # Errors
    /// Refuses links, non-root/writable policy, oversized or malformed records.
    pub fn load_installed() -> io::Result<Option<Self>> {
        Self::load(Path::new(CONFIG))
    }

    fn load(path: &Path) -> io::Result<Option<Self>> {
        // With no broker policy, projection is disabled. A hardened user service
        // may see /etc's root owner as unmapped; that must not prevent capture.
        let metadata = match fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let parent = fs::symlink_metadata(
            path.parent()
                .ok_or_else(|| invalid("broker Attention policy needs a parent directory"))?,
        )?;
        if !parent.is_dir() || parent.uid() != 0 || parent.mode() & 0o022 != 0 {
            return Err(invalid("broker Attention policy directory is untrusted"));
        }
        if !metadata.is_file()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || metadata.len() > 4096
        {
            return Err(invalid(
                "broker Attention policy is not a trusted regular file",
            ));
        }
        let file = File::open(path)?;
        let opened = file.metadata()?;
        if (opened.dev(), opened.ino()) != (metadata.dev(), metadata.ino()) {
            return Err(invalid("broker Attention policy changed while opening"));
        }
        let mut bytes = Vec::new();
        file.take(4097).read_to_end(&mut bytes)?;
        if bytes.len() > 4096 {
            return Err(invalid("broker Attention policy is oversized"));
        }
        let config: Self = serde_json::from_slice(&bytes)
            .map_err(|_| invalid("broker Attention policy is malformed"))?;
        config.validate()?;
        if config.socket != std::path::Path::new(SOCKET) || config.broker_uid == 0 {
            return Err(invalid(
                "broker Attention policy has an invalid endpoint or identity",
            ));
        }
        Ok(Some(config))
    }

    fn validate(&self) -> io::Result<()> {
        if !self.socket.is_absolute()
            || self.capability_sha256.len() != 64
            || !self
                .capability_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(invalid("broker Attention policy is invalid"));
        }
        Ok(())
    }
}

/// Process-owned listener for broker projections and read-only Run facts.
pub struct BrokerAttentionSocket {
    listener: UnixListener,
    config: BrokerAttentionConfig,
    store: AttentionStore,
    _lock: File,
}

impl BrokerAttentionSocket {
    /// Bind inside a pre-provisioned owner-writable, cross-user traversable directory.
    /// The caller owns the trusted config; the installed CLI loads root policy.
    /// Filesystem work runs off the async executor. No credential is created here.
    ///
    /// # Errors
    /// Refuses invalid policy, insecure directories, live/path collisions or I/O failure.
    pub async fn bind(config: BrokerAttentionConfig, store: AttentionStore) -> io::Result<Self> {
        config.validate()?;
        let path = config.socket.clone();
        let (listener, lock) = tokio::task::spawn_blocking(move || {
            let parent = path
                .parent()
                .ok_or_else(|| invalid("projection socket needs a parent"))?;
            let directory = fs::symlink_metadata(parent)?;
            if !directory.is_dir() || directory.mode() & 0o777 != 0o711 {
                return Err(invalid(
                    "projection directory must be provisioned at mode 0711",
                ));
            }
            let lock = OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .mode(0o600)
                .open(path.with_extension("lock"))?;
            lock.try_lock_exclusive()?;
            match fs::symlink_metadata(&path) {
                Ok(_) => {
                    if std::os::unix::net::UnixStream::connect(&path).is_ok() {
                        return Err(io::Error::new(
                            io::ErrorKind::AddrInUse,
                            "projection socket is live",
                        ));
                    }
                    remove_stale_socket(&path)?;
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
            let listener = StdListener::bind(&path)?;
            if fs::symlink_metadata(&path)?.uid() != directory.uid() {
                return Err(invalid("projection socket and directory owners differ"));
            }
            // Only connect is public. SO_PEERCRED is checked before reading,
            // and a separate producer credential is required before mutation.
            fs::set_permissions(&path, fs::Permissions::from_mode(0o666))?;
            listener.set_nonblocking(true)?;
            Ok((listener, lock))
        })
        .await
        .map_err(io::Error::other)??;
        Ok(Self {
            listener: UnixListener::from_std(listener)?,
            config,
            store,
            _lock: lock,
        })
    }

    /// Serve bounded connections until cancelled; dropping this future aborts client tasks.
    ///
    /// # Errors
    /// Returns listener I/O failure. Rejected, malformed and timed-out peers are disconnected.
    pub async fn serve(self) -> io::Result<()> {
        let mut clients = JoinSet::new();
        loop {
            tokio::select! {
                accepted = self.listener.accept(), if clients.len() < 32 => {
                    let (stream, _) = accepted?;
                    match stream.peer_cred() {
                        Ok(peer) if peer.uid() == self.config.broker_uid => {},
                        // No snapshot or payload is sent to an untrusted UID.
                        Ok(_) | Err(_) => continue,
                    }
                    let store = self.store.clone();
                    let hash = self.config.capability_sha256.clone();
                    clients.spawn(async move {
                        tokio::time::timeout(Duration::from_secs(2), project(stream, store, hash)).await
                    });
                }
                // Each result terminates only its client. Authorization and
                // protocol failures deliberately disclose no payload in logs.
                completed = clients.join_next(), if !clients.is_empty() => {
                    if let Some(Err(error)) = completed {
                        return Err(io::Error::other(error));
                    }
                }
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum Request {
    Project {
        request_id: String,
        projection: BrokerProjection,
        capability: String,
    },
    RunLifecycle {
        request_id: String,
        run_id: String,
        capability: String,
    },
}

async fn project(stream: UnixStream, store: AttentionStore, hash: String) -> io::Result<()> {
    let mut reader = BufReader::new(stream).take(MAX_FRAME + 1);
    let mut bytes = Vec::new();
    reader.read_until(b'\n', &mut bytes).await?;
    if bytes.len() > usize::try_from(MAX_FRAME).map_err(io::Error::other)?
        || bytes.last() != Some(&b'\n')
    {
        return Err(invalid("projection frame is incomplete or oversized"));
    }
    let request: Request =
        serde_json::from_slice(&bytes).map_err(|_| invalid("broker request is malformed"))?;
    let (request_id, capability) = match &request {
        Request::Project {
            request_id,
            capability,
            ..
        }
        | Request::RunLifecycle {
            request_id,
            capability,
            ..
        } => (request_id, capability),
    };
    let message = if request_id.is_empty()
        || request_id.len() > 128
        || capability.len() > 256
        || !verify_capability(&hash, capability)
    {
        serde_json::to_value(AttentionSocketMessage::MutationError {
            request_id: String::new(),
            message: "broker authorization refused".into(),
        })?
    } else {
        tokio::task::spawn_blocking(move || reply(request, &store))
            .await
            .map_err(io::Error::other)??
    };
    let stream = reader.get_mut().get_mut();
    stream.write_all(&serde_json::to_vec(&message)?).await?;
    stream.write_all(b"\n").await
}

fn reply(request: Request, store: &AttentionStore) -> io::Result<serde_json::Value> {
    let (request_id, result) = match request {
        Request::Project {
            request_id,
            projection,
            ..
        } => {
            let result = store.project(&projection).map(|result| {
                serde_json::json!({"type":"projection_result","request_id":request_id,"result":result})
            });
            (request_id, result)
        }
        Request::RunLifecycle {
            request_id, run_id, ..
        } => {
            let result = store.broker_run_lifecycle(&run_id).map(|result| {
                serde_json::json!({"type":"run_lifecycle_result","request_id":request_id,"result":result})
            });
            (request_id, result)
        }
    };
    match result {
        Ok(value) => Ok(value),
        Err(_) => serde_json::to_value(AttentionSocketMessage::MutationError {
            request_id,
            message: "broker request refused".into(),
        })
        .map_err(io::Error::other),
    }
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Policy fixtures abort on setup failure."
)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn absent_policy_disables_projection_even_with_untrusted_parent() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let policy = directory.path().join("policy.json");

        assert!(BrokerAttentionConfig::load(&policy).unwrap().is_none());
    }

    #[test]
    fn existing_policy_in_untrusted_parent_still_fails_closed() {
        let directory = tempfile::tempdir().unwrap();
        fs::set_permissions(directory.path(), fs::Permissions::from_mode(0o777)).unwrap();
        let policy = directory.path().join("policy.json");
        fs::write(&policy, b"{}").unwrap();
        assert_eq!(
            BrokerAttentionConfig::load(&policy)
                .err()
                .unwrap()
                .to_string(),
            "broker Attention policy directory is untrusted"
        );

        // A dangling policy link is invalid configuration, not absent configuration.
        fs::remove_file(&policy).unwrap();
        symlink(directory.path().join("missing.json"), &policy).unwrap();
        assert!(BrokerAttentionConfig::load(&policy).is_err());
    }
}
