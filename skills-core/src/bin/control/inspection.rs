//! Operator CLI and daemon routing into the one owning Session worker.

use louiselm_skills::broker::provider_extension::{
    ExtensionError, ExtensionOutcome, ExtensionRequest,
};
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
        if !matches!(
            verb.to_str(),
            Some("inspect" | "conformance" | "retention" | "pin" | "unpin")
        ) || format != "--json"
        {
            return Err(InspectError::InvalidRequest);
        }
        let id = id.to_str().ok_or(InspectError::InvalidRequest)?;
        operator::validate_subject(id)?;
        let paths = super::installed_paths().map_err(|_| InspectError::BrokerUnavailable)?;
        let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
            .map_err(|_| InspectError::BrokerUnavailable)?;
        if matches!(verb.to_str(), Some("retention" | "pin" | "unpin")) {
            let pin = match verb.to_str() {
                Some("pin") => Some(true),
                Some("unpin") => Some(false),
                _ => None,
            };
            let inspection = operator::workspace_retention(
                Path::new(operator::SOCKET),
                config.broker_uid,
                id,
                pin,
                operator::TIMEOUT,
            )?;
            serde_json::to_vec(&inspection).map_err(|_| InspectError::StatusUnavailable)
        } else if verb == "conformance" {
            operator::inspect_conformance(
                Path::new(operator::SOCKET),
                config.broker_uid,
                id,
                operator::TIMEOUT,
            )?
            .canonical_bytes()
            .map_err(|_| InspectError::StatusUnavailable)
        } else {
            operator::inspect(
                Path::new(operator::SOCKET),
                config.broker_uid,
                id,
                operator::TIMEOUT,
            )
            .map(|status| status.canonical_bytes())
        }
    })();
    match result {
        Ok(bytes) => match io::stdout().lock().write_all(&bytes) {
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

pub(super) fn skill_cli(arguments: &[std::ffi::OsString]) -> u8 {
    use louiselm_skills::skill_request::SkillRequestOutcome;
    let result = (|| {
        let [verb, id, format] = arguments else {
            return Err(InspectError::InvalidRequest);
        };
        if format != "--json" {
            return Err(InspectError::InvalidRequest);
        }
        let outcome = match verb.to_str() {
            Some("inspect") => None,
            Some("reject") => Some(SkillRequestOutcome::Rejected),
            Some("cancel") => Some(SkillRequestOutcome::Cancelled),
            _ => return Err(InspectError::InvalidRequest),
        };
        let id = id.to_str().ok_or(InspectError::InvalidRequest)?;
        let paths = super::installed_paths().map_err(|_| InspectError::BrokerUnavailable)?;
        let config = louiselm_skills::launcher_install::public_runtime_config(&paths)
            .map_err(|_| InspectError::BrokerUnavailable)?;
        let status = operator::skill_request(
            Path::new(operator::SOCKET),
            config.broker_uid,
            id,
            outcome,
            operator::TIMEOUT,
        )?;
        serde_json::to_vec(&status).map_err(|_| InspectError::StatusUnavailable)
    })();
    match result {
        Ok(bytes) => match io::stdout().lock().write_all(&bytes) {
            Ok(()) => 0,
            Err(_) => InspectError::StatusUnavailable.exit_code(),
        },
        Err(error) => {
            let _ = io::stderr().lock().write_all(&error.canonical_bytes());
            error.exit_code()
        }
    }
}

struct Query {
    expires: Instant,
    reply: SyncSender<Result<SessionStatus, InspectError>>,
}

enum WorkerQuery {
    Status(Query),
    ProviderExtension {
        expires: Instant,
        request: ExtensionRequest,
        reply: SyncSender<Result<ExtensionOutcome, ExtensionError>>,
    },
    Waiver {
        expires: Instant,
        request: louiselm_skills::broker::waiver::Request,
        reply: SyncSender<
            Result<
                louiselm_skills::broker::waiver::Outcome,
                louiselm_skills::broker::waiver::WaiverError,
            >,
        >,
    },
}

pub(super) struct Queries {
    operator_uid: u32,
    sessions: Mutex<HashMap<String, SyncSender<WorkerQuery>>>,
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
                    if let Err(error) = endpoint.serve_once(
                        |id, candidates| {
                            broker
                                .dependency_control(owner.operator_uid, id, candidates)
                                .map_err(|_| InspectError::StatusUnavailable)
                        },
                        |id, deadline| owner.inspect(&broker, id, deadline),
                        |id| {
                            broker
                                .inspect_conformance(id)
                                .map_err(|_| InspectError::StatusUnavailable)?
                                .ok_or(InspectError::UnknownSession)
                        },
                        |id, outcome| {
                            broker
                                .skill_request_control(owner.operator_uid, id, outcome)
                                .map_err(|_| InspectError::StatusUnavailable)
                        },
                        |id, decision| {
                            broker
                                .beads_mutation_control(owner.operator_uid, id, decision)
                                .map_err(|_| InspectError::StatusUnavailable)
                        },
                        |id, pin| {
                            broker
                                .workspace_retention(owner.operator_uid, id, pin)
                                .map_err(|_| InspectError::StatusUnavailable)
                        },
                        |id, request, deadline| {
                            owner.waiver_request(&broker, id, request, deadline)
                        },
                        |id, request, deadline| owner.provider_extension(id, request, deadline),
                    ) {
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

    fn waiver_request(
        &self,
        broker: &InstalledBroker,
        id: &str,
        request: &louiselm_skills::broker::waiver::Request,
        deadline: Instant,
    ) -> Result<
        louiselm_skills::broker::waiver::Outcome,
        louiselm_skills::broker::waiver::WaiverError,
    > {
        use louiselm_skills::broker::waiver::{Request, WaiverError};
        let result = if broker
            .has_pending_launch(id)
            .map_err(|_| WaiverError::Unavailable)?
        {
            broker.pre_admission_waiver(self.operator_uid, id, request)
        } else if matches!(request, Request::Inspect | Request::Result { .. }) {
            broker.waiver_history(self.operator_uid, id, request)
        } else {
            return self.waiver(id, request, deadline);
        };
        result.map_err(|error| match error {
            BrokerError::Waiver(error) => error,
            _ => WaiverError::Unavailable,
        })
    }

    fn provider_extension(
        &self,
        id: &str,
        request: &ExtensionRequest,
        deadline: Instant,
    ) -> Result<ExtensionOutcome, ExtensionError> {
        let expires = deadline.min(Instant::now() + QUERY_TIMEOUT);
        let sender = self
            .sessions
            .lock()
            .map_err(|_| ExtensionError::Unavailable)?
            .get(id)
            .cloned()
            .ok_or(ExtensionError::Unknown)?;
        let (reply, receive) = mpsc::sync_channel(1);
        sender
            .try_send(WorkerQuery::ProviderExtension {
                expires,
                request: request.clone(),
                reply,
            })
            .map_err(|_| ExtensionError::Unavailable)?;
        receive
            .recv_timeout(expires.saturating_duration_since(Instant::now()))
            .map_err(|_| ExtensionError::Unavailable)?
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

    fn waiver(
        &self,
        id: &str,
        request: &louiselm_skills::broker::waiver::Request,
        deadline: Instant,
    ) -> Result<
        louiselm_skills::broker::waiver::Outcome,
        louiselm_skills::broker::waiver::WaiverError,
    > {
        use louiselm_skills::broker::waiver::WaiverError;
        let expires = deadline.min(Instant::now() + QUERY_TIMEOUT);
        let sender = self
            .sessions
            .lock()
            .map_err(|_| WaiverError::Unavailable)?
            .get(id)
            .cloned()
            .ok_or(WaiverError::Unknown)?;
        let (reply, receive) = mpsc::sync_channel(1);
        sender
            .try_send(WorkerQuery::Waiver {
                expires,
                request: request.clone(),
                reply,
            })
            .map_err(|_| WaiverError::Unavailable)?;
        receive
            .recv_timeout(expires.saturating_duration_since(Instant::now()))
            .map_err(|_| WaiverError::Unavailable)?
    }

    /// Answers one operator query on the Session's own worker turn.
    fn answer(&self, broker: &InstalledBroker, session: &mut BrokerSession, query: WorkerQuery) {
        match query {
            WorkerQuery::Waiver {
                expires,
                request,
                reply,
            } => {
                if Instant::now() < expires {
                    let result = broker
                        .waiver_control(session, self.operator_uid, &request)
                        .map_err(|error| match error {
                            BrokerError::Waiver(error) => error,
                            _ => louiselm_skills::broker::waiver::WaiverError::Unavailable,
                        });
                    // A disconnected operator can inspect the durable result later.
                    let _ = reply.send(result);
                }
            }
            WorkerQuery::ProviderExtension {
                expires,
                request,
                reply,
            } => {
                if Instant::now() < expires {
                    // A disconnected operator can retry the same request later.
                    let _ = reply.send(broker.extend_provider_budget(
                        session,
                        self.operator_uid,
                        &request,
                    ));
                }
            }
            WorkerQuery::Status(query) => {
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
            }
        }
    }

    pub(super) fn run_session(
        &self,
        broker: &Arc<InstalledBroker>,
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
            let mut posture_check = Instant::now();
            let mut posture_unavailable = false;
            let mut provider_hold_unavailable = false;
            let mut skill_quarantine_unavailable = false;
            loop {
                if Instant::now() >= posture_check {
                    report_transition(
                        broker.project_posture_attention(session).is_ok(),
                        &mut posture_unavailable,
                        "louiselm-control: posture Attention unavailable; retained conditions unchanged",
                    );
                    report_transition(
                        broker.settle_provider_hold(session).is_ok(),
                        &mut provider_hold_unavailable,
                        "louiselm-control: Provider budget hold not settled; retrying",
                    );
                    report_transition(
                        broker.settle_skill_quarantine(session).is_ok(),
                        &mut skill_quarantine_unavailable,
                        "louiselm-control: skill quarantine not settled; retrying",
                    );
                    posture_check = Instant::now() + Duration::from_secs(1);
                }
                if let Ok(query) = requests.try_recv() {
                    self.answer(broker, session, query);
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
                broker.drive_provider(session)?;
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
    sender: &SyncSender<WorkerQuery>,
    deadline: Instant,
) -> Result<SessionStatus, InspectError> {
    let now = Instant::now();
    if now >= deadline {
        return Err(InspectError::StatusUnavailable);
    }
    let expires = deadline.min(now + QUERY_TIMEOUT);
    let (reply, receive) = mpsc::sync_channel(1);
    sender
        .try_send(WorkerQuery::Status(Query { expires, reply }))
        .map_err(|_| InspectError::StatusUnavailable)?;
    receive
        .recv_timeout(expires.saturating_duration_since(Instant::now()))
        .map_err(|_| InspectError::StatusUnavailable)?
}

/// Reports a recurring upkeep failure once per failing stretch, not every tick.
fn report_transition(succeeded: bool, failing: &mut bool, message: &str) {
    if succeeded {
        *failing = false;
    } else if !*failing {
        eprintln!("{message}");
        *failing = true;
    }
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
        let (sender, receiver) = mpsc::sync_channel::<WorkerQuery>(1);
        let worker = thread::spawn(move || {
            if let Ok(WorkerQuery::Status(query)) =
                receiver.recv_timeout(Duration::from_millis(100))
            {
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
        let (sender, receiver) = mpsc::sync_channel::<WorkerQuery>(1);
        let deadline = Instant::now() + Duration::from_millis(50);
        let worker = thread::spawn(move || {
            let WorkerQuery::Status(query) = receiver.recv_timeout(Duration::from_secs(1)).unwrap()
            else {
                return;
            };
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
