//! Dedicated, UID-pinned broker projection endpoint; no observer or Run authority.

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
    path::PathBuf,
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
        let parent = fs::symlink_metadata("/etc")?;
        if !parent.is_dir() || parent.uid() != 0 || parent.mode() & 0o022 != 0 {
            return Err(invalid("broker Attention policy directory is untrusted"));
        }
        let metadata = match fs::symlink_metadata(CONFIG) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if !metadata.is_file()
            || metadata.uid() != 0
            || metadata.mode() & 0o022 != 0
            || metadata.len() > 4096
        {
            return Err(invalid(
                "broker Attention policy is not a trusted regular file",
            ));
        }
        let file = File::open(CONFIG)?;
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

/// Process-owned listener and bounded client tasks for broker projections only.
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
    let Request::Project {
        request_id,
        projection,
        capability,
    } = serde_json::from_slice(&bytes).map_err(|_| invalid("projection request is malformed"))?;
    let message = if request_id.is_empty()
        || request_id.len() > 128
        || capability.len() > 256
        || !verify_capability(&hash, &capability)
    {
        AttentionSocketMessage::MutationError {
            request_id: String::new(),
            message: "projection authorization refused".into(),
        }
    } else {
        match tokio::task::spawn_blocking(move || store.project(&projection))
            .await
            .map_err(io::Error::other)?
        {
            Ok(result) => AttentionSocketMessage::ProjectionResult { request_id, result },
            Err(_) => AttentionSocketMessage::MutationError {
                request_id,
                message: "projection refused".into(),
            },
        }
    };
    let stream = reader.get_mut().get_mut();
    stream.write_all(&serde_json::to_vec(&message)?).await?;
    stream.write_all(b"\n").await
}

fn invalid(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}
