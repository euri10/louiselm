//! Operator CLI and daemon routing into the one owning Session worker.

use louiselm_skills::{
    broker::{
        BrokerError, BrokerSession, InstalledBroker,
        lifecycle::LifecycleCaller,
        operator::{self, InspectError, OperatorServer},
    },
    launch_protocol::SessionStatus,
    launch_transport::TransportError,
};
use std::{
    collections::{HashMap, hash_map::Entry},
    io::{self, Write},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    sync::{
        Arc, Mutex,
        mpsc::{self, SyncSender},
    },
    thread,
    time::{Duration, Instant},
};

const QUERY_TIMEOUT: Duration = Duration::from_secs(30);
const IDLE_SLICE: Duration = Duration::from_millis(50);

pub(super) fn cli(arguments: &[std::ffi::OsString]) -> u8 {
    let result = (|| {
        let [verb, id, format] = arguments else {
            return Err(InspectError::InvalidRequest);
        };
        if verb != "inspect" || format != "--json" {
            return Err(InspectError::InvalidRequest);
        }
        let id = id.to_str().ok_or(InspectError::InvalidRequest)?;
        operator::validate_subject(id)?;
        let paths = super::installed_paths().map_err(|_| InspectError::BrokerUnavailable)?;
        let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
            .map_err(|_| InspectError::BrokerUnavailable)?;
        operator::inspect(
            Path::new(operator::SOCKET),
            config.broker_uid,
            id,
            operator::TIMEOUT,
        )
    })();
    match result {
        Ok(status) => match io::stdout().lock().write_all(&status.canonical_bytes()) {
            Ok(()) => 0,
            Err(_) => InspectError::StatusUnavailable.exit_code(),
        },
        Err(error) => {
            // A closed diagnostic sink cannot change the already refused query.
            let _ = io::stderr().lock().write_all(&error.canonical_bytes());
            error.exit_code()
        }
    }
}

struct Query {
    expires: Instant,
    reply: SyncSender<Result<SessionStatus, InspectError>>,
}

pub(super) struct Queries {
    operator_uid: u32,
    sessions: Mutex<HashMap<String, SyncSender<Query>>>,
}

impl Queries {
    pub(super) fn start(broker: Arc<InstalledBroker>) -> Result<Arc<Self>, BrokerError> {
        let config = louiselm_skills::launcher_install::public_runtime_config(
            &louiselm_skills::launcher_install::LauncherPaths::system(),
        )
        .map_err(BrokerError::InstallationAuthority)?;
        let parent = Path::new("/run/louiselm-operator");
        let metadata = std::fs::symlink_metadata(parent).map_err(BrokerError::Storage)?;
        let ancestor = std::fs::symlink_metadata("/run").map_err(BrokerError::Storage)?;
        if !metadata.is_dir()
            || metadata.uid() != config.broker_uid
            || metadata.gid() != config.broker_gid
            || metadata.permissions().mode() & 0o777 != 0o755
            || !ancestor.is_dir()
            || ancestor.uid() != 0
            || ancestor.mode() & 0o022 != 0
        {
            return Err(BrokerError::Installation);
        }
        let endpoint = OperatorServer::bind(Path::new(operator::SOCKET), config.operator_uid)
            .map_err(BrokerError::Storage)?;
        let queries = Arc::new(Self {
            operator_uid: config.operator_uid,
            sessions: Mutex::new(HashMap::new()),
        });
        let owner = Arc::clone(&queries);
        thread::Builder::new()
            .name("louiselm-operator-inspect".into())
            .spawn(move || {
                loop {
                    if let Err(error) =
                        endpoint.serve_once(|id, deadline| owner.inspect(&broker, id, deadline))
                    {
                        if error.kind() == io::ErrorKind::Interrupted {
                            continue;
                        }
                        eprintln!("louiselm-control: operator listener unavailable");
                        break;
                    }
                }
            })
            .map_err(|_| BrokerError::Transport(TransportError::WorkerUnavailable))?;
        Ok(queries)
    }

    fn inspect(
        &self,
        broker: &InstalledBroker,
        id: &str,
        deadline: Instant,
    ) -> Result<SessionStatus, InspectError> {
        let sender = self
            .sessions
            .lock()
            .map_err(|_| InspectError::StatusUnavailable)?
            .get(id)
            .cloned();
        let Some(sender) = sender else {
            return match broker.inspect(id) {
                Ok(None) => Err(InspectError::UnknownSession),
                Ok(Some(_)) | Err(_) => Err(InspectError::StatusUnavailable),
            };
        };
        request_status(&sender, deadline)
    }

    pub(super) fn run_session(
        &self,
        broker: &InstalledBroker,
        session: &mut BrokerSession,
    ) -> Result<(), BrokerError> {
        let id = session.authorization().session_id.clone();
        let (sender, requests) = mpsc::sync_channel(1);
        match self
            .sessions
            .lock()
            .map_err(|_| BrokerError::InvalidGrant)?
            .entry(id.clone())
        {
            Entry::Vacant(entry) => {
                entry.insert(sender);
            }
            Entry::Occupied(_) => return Err(BrokerError::DuplicateAuthorization),
        }
        let result = (|| {
            loop {
                if let Ok(query) = requests.try_recv() {
                    if Instant::now() < query.expires {
                        let result = broker.session_status(
                            session,
                            &LifecycleCaller::Operator {
                                uid: self.operator_uid,
                            },
                        );
                        // This query is observational. Client timeout/disconnect
                        // cannot cancel another operation or renew authority.
                        let _ = query
                            .reply
                            .send(result.map_err(|_| InspectError::StatusUnavailable));
                    }
                    if session.channel().is_closed() {
                        return Err(BrokerError::Transport(TransportError::Closed));
                    }
                }
                let (ready, received) = mpsc::sync_channel(1);
                session
                    .channel()
                    .wait_readable(
                        IDLE_SLICE,
                        Box::new(move |result| {
                            let _ = ready.send(result);
                        }),
                    )
                    .map_err(BrokerError::Transport)?;
                let readable = received
                    .recv()
                    .map_err(|_| BrokerError::Transport(TransportError::Closed))?
                    .map_err(BrokerError::Transport)?;
                if readable && broker.step(session)? {
                    return Ok(());
                }
            }
        })();
        self.sessions
            .lock()
            .map_err(|_| BrokerError::InvalidGrant)?
            .remove(&id);
        result
    }
}

fn request_status(
    sender: &SyncSender<Query>,
    deadline: Instant,
) -> Result<SessionStatus, InspectError> {
    let now = Instant::now();
    if now >= deadline {
        return Err(InspectError::StatusUnavailable);
    }
    let expires = deadline.min(now + QUERY_TIMEOUT);
    let (reply, receive) = mpsc::sync_channel(1);
    sender
        .try_send(Query { expires, reply })
        .map_err(|_| InspectError::StatusUnavailable)?;
    receive
        .recv_timeout(expires.saturating_duration_since(Instant::now()))
        .map_err(|_| InspectError::StatusUnavailable)?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(
        clippy::unwrap_used,
        reason = "Bounded queue fixture asserts the deadline contract."
    )]
    fn expired_exchange_never_queues_or_renews_a_worker_wait() {
        let (sender, receiver) = mpsc::sync_channel::<Query>(1);
        let worker = thread::spawn(move || {
            if let Ok(query) = receiver.recv_timeout(Duration::from_millis(100)) {
                let _ = query.reply.send(Err(InspectError::UnknownSession));
                true
            } else {
                false
            }
        });
        let result = request_status(&sender, Instant::now());
        drop(sender);
        let queued = worker.join().unwrap();
        assert_eq!(result, Err(InspectError::StatusUnavailable));
        assert!(!queued);
    }

    #[test]
    #[allow(
        clippy::unwrap_used,
        reason = "Bounded queue fixture asserts the deadline contract."
    )]
    fn queue_expiry_and_wait_are_clamped_to_the_exchange_deadline() {
        let (sender, receiver) = mpsc::sync_channel::<Query>(1);
        let deadline = Instant::now() + Duration::from_millis(50);
        let worker = thread::spawn(move || {
            let query = receiver.recv_timeout(Duration::from_secs(1)).unwrap();
            assert_eq!(query.expires, deadline);
            thread::sleep(Duration::from_millis(150));
            let _ = query.reply.send(Err(InspectError::UnknownSession));
        });
        assert_eq!(
            request_status(&sender, deadline),
            Err(InspectError::StatusUnavailable)
        );
        worker.join().unwrap();
    }
}
