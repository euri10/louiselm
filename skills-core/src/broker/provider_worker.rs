//! Bounded per-connection Provider network work; only the Session owner admits.

use super::{BrokerError, BrokerSession};
use crate::{
    launch_protocol::{
        GUARD_SOCKET_REQUEST_SCHEMA, GuardEnrollment, GuardSocketRequest, GuardSocketRetire,
        PROTOCOL_VERSION,
    },
    provider_request::ProviderRequest,
};
use std::{
    collections::BTreeMap,
    io::{self, Read},
    net::SocketAddr,
    sync::{
        Arc, Weak,
        atomic::{AtomicBool, Ordering},
        mpsc::{self, Receiver, SyncSender},
    },
    time::{Duration, Instant},
};

pub(super) struct ProviderAssignment {
    pub(super) request: ProviderRequest,
    pub(super) admission: super::provider_service::AdmittedProviderRequest,
    pub(super) socket: super::GuardedUpstream,
    pub(super) retirement: SyncSender<GuardSocketRetire>,
}

pub(super) struct ProviderJob {
    pub(super) request: ProviderRequest,
    pub(super) reply: SyncSender<Result<ProviderAssignment, BrokerError>>,
}

pub(super) struct PendingProvider {
    pub(super) job: ProviderJob,
    pub(super) admission: super::provider_service::AdmittedProviderRequest,
    pub(super) destination: SocketAddr,
}

/// One Session owner's bounded mailbox; workers never own policy or protocol I/O.
pub(super) struct ProviderWork {
    pub(super) jobs: (SyncSender<ProviderJob>, Receiver<ProviderJob>),
    pub(super) retired: (SyncSender<GuardSocketRetire>, Receiver<GuardSocketRetire>),
    pub(super) pending: BTreeMap<String, PendingProvider>,
    pub(super) connections: Vec<Weak<super::provider_listener::ConnectionLease>>,
    pub(super) cancelled: Arc<AtomicBool>,
    pub(super) sequence: u64,
    #[cfg(test)]
    test_root: Option<ureq::tls::Certificate<'static>>,
}

impl Default for ProviderWork {
    fn default() -> Self {
        Self {
            jobs: mpsc::sync_channel(128),
            retired: mpsc::sync_channel(128),
            pending: BTreeMap::new(),
            connections: Vec::new(),
            cancelled: Arc::new(AtomicBool::new(false)),
            sequence: 0,
            #[cfg(test)]
            test_root: None,
        }
    }
}

impl ProviderWork {
    #[cfg(test)]
    pub(super) fn set_test_root(&mut self, root: ureq::tls::Certificate<'static>) {
        self.test_root = Some(root);
    }

    #[cfg(test)]
    pub(super) fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    pub(super) fn cancel(&self) {
        self.cancelled.store(true, Ordering::Release);
    }

    pub(super) fn shutdown_connections(&self) {
        for connection in self.connections.iter().filter_map(Weak::upgrade) {
            let _ = connection.shutdown();
        }
    }
}

struct RetiringBody {
    inner: Option<Box<dyn Read + Send>>,
    notice: Option<GuardSocketRetire>,
    retirements: SyncSender<GuardSocketRetire>,
}

impl Read for RetiringBody {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        self.inner
            .as_mut()
            .ok_or_else(|| io::Error::other("upstream body closed"))?
            .read(bytes)
    }
}

impl Drop for RetiringBody {
    fn drop(&mut self) {
        // Close TLS and socket before the supervisor is told to retire its copy.
        self.inner.take();
        if let Some(notice) = self.notice.take() {
            let _ = self.retirements.send(notice);
        }
    }
}

impl super::InstalledBroker {
    /// Accepts local connections and processes complete request/retirement mailboxes.
    /// The Session owner alone performs policy admission and supervisor protocol I/O;
    /// each connection worker only parses, sends assigned HTTPS, and relays.
    /// # Errors
    /// Refuses lost scope, local worker capacity, durable admission or transport loss.
    #[expect(
        clippy::too_many_lines,
        reason = "One owner tick accepts and routes bounded Provider mailboxes."
    )]
    pub fn drive_provider(
        self: &Arc<Self>,
        session: &mut BrokerSession,
    ) -> Result<(), BrokerError> {
        let enrollment = session
            .provider_listener
            .as_ref()
            .map(|listener| listener.enrollment.clone());
        if let Some(enrollment) = enrollment {
            session
                .provider_work
                .connections
                .retain(|lease| lease.strong_count() != 0);
            if session.provider_work.connections.len() < 128 {
                let listener = session
                    .provider_listener
                    .as_ref()
                    .ok_or(BrokerError::InvalidGrant)?;
                if let Some(stream) = listener.accept()? {
                    let lease = stream.lease();
                    let jobs = session.provider_work.jobs.0.clone();
                    let cancelled = Arc::clone(&session.provider_work.cancelled);
                    let host = enrollment.address.to_string();
                    let broker = Arc::clone(self);
                    #[cfg(test)]
                    let test_root = session.provider_work.test_root.clone();
                    std::thread::Builder::new()
                        .name("louiselm-provider-connection".into())
                        .spawn(move || {
                            let _ = super::provider_endpoint::serve_provider_connection(
                                stream,
                                host,
                                |request| {
                                    if cancelled.load(Ordering::Acquire) {
                                        return Err(BrokerError::ProviderUnavailable);
                                    }
                                    let (reply, receive) = mpsc::sync_channel(1);
                                    jobs.try_send(ProviderJob { request, reply })
                                        .map_err(|_| BrokerError::ProviderUnavailable)?;
                                    let deadline = Instant::now() + Duration::from_secs(30);
                                    let assignment = loop {
                                        if cancelled.load(Ordering::Acquire)
                                            || Instant::now() >= deadline
                                        {
                                            return Err(BrokerError::ProviderUnavailable);
                                        }
                                        match receive.recv_timeout(Duration::from_millis(50)) {
                                            Ok(result) => break result?,
                                            Err(mpsc::RecvTimeoutError::Timeout) => {},
                                            Err(mpsc::RecvTimeoutError::Disconnected) => {
                                                return Err(BrokerError::ProviderUnavailable);
                                            }
                                        }
                                    };
                                    let evidence = assignment.socket.evidence();
                                    let notice = GuardSocketRetire {
                                        schema: crate::launch_protocol::GUARD_SOCKET_RETIRE_SCHEMA.into(),
                                        protocol_version: PROTOCOL_VERSION,
                                        enrollment: evidence.enrollment.clone(),
                                        socket_cookie: evidence.socket_cookie,
                                    };
                                    #[cfg(not(test))]
                                    let transport = super::provider_transport::GuardedHttpsProviderTransport::new(assignment.socket);
                                    #[cfg(test)]
                                    let transport = match test_root.clone() {
                                        Some(root) => super::provider_transport::GuardedHttpsProviderTransport::with_test_root(assignment.socket, root),
                                        None => super::provider_transport::GuardedHttpsProviderTransport::new(assignment.socket),
                                    };
                                    match assignment.admission.send(
                                        &broker.provider_credentials,
                                        &transport,
                                        &assignment.request,
                                    ) {
                                        Ok(mut response) => {
                                            response.body = Box::new(RetiringBody {
                                                inner: Some(response.body),
                                                notice: Some(notice),
                                                retirements: assignment.retirement,
                                            });
                                            Ok(response)
                                        }
                                        Err(error) => {
                                            let _ = assignment.retirement.send(notice);
                                            Err(error)
                                        }
                                    }
                                },
                            );
                        })
                        .map_err(|_| BrokerError::ProviderUnavailable)?;
                    session
                        .provider_work
                        .connections
                        .retain(|connection| connection.strong_count() != 0);
                    session.provider_work.connections.push(lease);
                }
            }
        }
        while let Ok(notice) = session.provider_work.retired.1.try_recv() {
            session.provider_sockets.remove(&notice.socket_cookie);
            if session
                .provider_listener
                .as_ref()
                .is_some_and(|listener| listener.enrollment == notice.enrollment)
                && !session.channel().is_closed()
            {
                super::service::send(session.channel(), notice.canonical_bytes())?;
            }
        }
        while session.provider_work.pending.is_empty() {
            let Ok(job) = session.provider_work.jobs.1.try_recv() else {
                break;
            };
            if session.provider_sockets.len() + session.provider_work.pending.len() >= 128 {
                let _ = job.reply.send(Err(BrokerError::ProviderUnavailable));
                continue;
            }
            let result = self.admit_provider_job(session, &job.request);
            match result {
                Ok((admission, destination, enrollment)) => {
                    session.provider_work.sequence = session
                        .provider_work
                        .sequence
                        .checked_add(1)
                        .ok_or(BrokerError::ProviderUnavailable)?;
                    let request_id = format!("provider-{}", session.provider_work.sequence);
                    let request = GuardSocketRequest {
                        schema: GUARD_SOCKET_REQUEST_SCHEMA.into(),
                        protocol_version: PROTOCOL_VERSION,
                        request_id: request_id.clone(),
                        enrollment,
                        destination,
                    };
                    request.validate()?;
                    session.provider_work.pending.insert(
                        request_id,
                        PendingProvider {
                            job,
                            admission,
                            destination,
                        },
                    );
                    super::service::send(session.channel(), request.canonical_bytes())?;
                }
                Err(error) => {
                    if matches!(
                        error,
                        BrokerError::Policy(_)
                            | BrokerError::ProviderBudgetExhausted
                            | BrokerError::Expired
                            | BrokerError::InvalidGrant
                    ) {
                        let _ = job.reply.send(Err(error));
                    } else {
                        let _ = job.reply.send(Err(BrokerError::ProviderUnavailable));
                        session.close();
                        return Err(error);
                    }
                }
            }
        }
        Ok(())
    }

    fn admit_provider_job(
        &self,
        session: &mut BrokerSession,
        request: &ProviderRequest,
    ) -> Result<
        (
            super::provider_service::AdmittedProviderRequest,
            SocketAddr,
            GuardEnrollment,
        ),
        BrokerError,
    > {
        let enrollment = session
            .provider_listener
            .as_ref()
            .ok_or(BrokerError::InvalidGrant)?
            .enrollment
            .clone();
        let mut verification_failure = None;
        let result = self.service.admit_provider_request(
            session,
            &self.provider_credentials,
            request,
            super::now_ms()?,
            |key, payload, signature| match self.verifier.verify(key, payload, signature) {
                Ok(()) => true,
                Err(error) => {
                    verification_failure = Some(error);
                    false
                }
            },
        );
        let admission = match verification_failure {
            Some(error) => return Err(BrokerError::Verification(error)),
            None => result?,
        };
        let permission = admission.permission();
        let port = url::Url::parse(&permission.upstream)
            .map_err(|_| BrokerError::InvalidGrant)?
            .port_or_known_default()
            .ok_or(BrokerError::InvalidGrant)?;
        let address = permission
            .addresses
            .first()
            .copied()
            .ok_or(BrokerError::InvalidGrant)?;
        Ok((admission, SocketAddr::new(address, port), enrollment))
    }
}
