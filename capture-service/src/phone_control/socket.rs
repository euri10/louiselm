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
    task::JoinSet,
};

use super::{
    MAX_FRAME, MAX_INTEGER,
    authority::{Authority, EnrollmentStatus, Refusal, Scope, random_hex},
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
    attached: Mutex<BTreeMap<String, bool>>,
}

impl ControllerRegistry {
    pub(super) fn contains(&self, id: &str) -> bool {
        self.attached
            .lock()
            .is_ok_and(|entries| entries.get(id) == Some(&true))
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
            *active = false;
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
                if !attached.contains_key(&id) {
                    attached.insert(id.clone(), true);
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
    // The integration slice supplies read dispatch; unsolicited controller
    // input closes this registration instead of becoming authority.
    let mut byte = [0; 1];
    let _ = reader.read(&mut byte).await?;
    Ok(())
}

pub(super) struct AdminSocket {
    socket: LockedListener,
    authority: Arc<Authority>,
    pairing: Arc<PairingRegistry>,
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
        })
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
                    let hash = self.socket.operator_hash.clone();
                    clients.spawn(async move { serve_admin(stream, authority, pairing, &hash).await });
                }
                Some(_) = clients.join_next(), if !clients.is_empty() => {}
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum AdminRequest {
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
    fn common(&self) -> (u64, &str, &str) {
        match self {
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
            } => (*v, capability, device_id),
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
            } else if version != 1 {
                Err(Refusal::Unsupported)
            } else if !verify_capability(hash, token) {
                Err(Refusal::Unauthenticated)
            } else if !uuid::Uuid::parse_str(device).is_ok_and(|id| id.to_string() == device) {
                Err(Refusal::Malformed)
            } else {
                tokio::task::spawn_blocking(move || process_admin(&authority, &pairing, request))
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
    request: AdminRequest,
) -> Result<Value, Refusal> {
    let (_, _, device) = request.common();
    if !pairing
        .status()
        .map_err(|_| Refusal::Unavailable)?
        .devices
        .iter()
        .any(|status| status.device_id == device)
    {
        return Err(Refusal::Unauthenticated);
    }
    match request {
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
            ..
        } => {
            let grant = authority.approve(
                &device_id,
                &enrollment_id,
                &enrolled_key,
                scope,
                Instant::now(),
            )?;
            Ok(
                json!({"v":1,"type":"enrollment_approved","device_id":device_id,
                "grant_id":grant.grant_id,"grant_revision":grant.grant_revision}),
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
        let approved = exchange(&path, json!({"v":1,"type":"approve","capability":token,
            "device_id":phone.device_id,"enrollment_id":started["enrollment_id"],"enrolled_key":key,
            "scope":{"projects":[root.path().to_str().unwrap()],"agents":["codex"],"groups":["read"]}})).await;
        assert_eq!(approved["type"], "enrollment_approved");
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
