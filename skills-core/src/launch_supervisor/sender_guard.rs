//! Supervisor-owned kernel enforcement. Loading never enables an endpoint.
//!
//! Call on the privileged launch worker, before releasing the measured exec
//! stop. Only the embedded object is accepted. Endpoint/socket handoff must
//! carry the returned namespace leases, and the broker must receive the
//! authenticated post-enrollment supervisor response before serving requests.

use std::{
    collections::BTreeMap,
    fs::File,
    net::{SocketAddr, TcpListener, TcpStream},
    os::{
        fd::{AsFd, AsRawFd, BorrowedFd, OwnedFd},
        unix::fs::MetadataExt,
    },
    time::Duration,
};

use libbpf_rs::{MapCore, MapFlags, MapHandle, ObjectBuilder};
use rustix::{
    mount::MountFlags,
    process::{Pid, PidfdFlags, pidfd_open},
};
use thiserror::Error;

use crate::launch_protocol::{GuardEnrollment, GuardScope};
use crate::launch_transport::{KernelProcess, SeqpacketChannel};

mod binding;
mod handoff;
mod sockets;
use binding::Rule;
pub use sockets::GuardedSocket;
use sockets::shutdown;

mod namespace;
use namespace::PinNamespace;

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::panic,
    reason = "Privileged fixture aborts on protocol/setup failure."
)]
mod launch_fixture;
#[cfg(test)]
mod tests;

/// Sanitized guard failures; libbpf diagnostics are disabled at this boundary.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum GuardError {
    /// Required privilege, namespaces, BPF LSM, BTF or embedded hooks are unavailable.
    #[error("Sender guard platform unavailable")]
    Unavailable,
    /// Scope is invalid, expired or belongs to another launch/revision.
    #[error("Sender guard authority mismatch")]
    Authority,
    /// The original owner/runtime/broker was lost; this guard cannot be repaired.
    #[error("Sender guard lifetime lost")]
    Lost,
    /// The endpoint or socket cannot be bound to enforcement.
    #[error("Sender guard socket setup failed")]
    Socket,
    /// Required map/pin/enrollment state could not be established.
    #[error("Sender guard enrollment failed")]
    Enrollment,
    /// Descriptor shutdown could not be proved; identities must not be reused.
    #[error("Sender guard cleanup unproven")]
    Cleanup,
    /// Close a completed upstream connection before opening another.
    #[error("Sender guard upstream capacity exhausted")]
    Capacity,
}

struct Endpoint {
    listener: TcpListener,
    network: File,
    rule: Rule,
}

/// Privileged owner of one Session's guard. There is no recovery enrollment API.
///
/// Protected pins are never unlinked/unmounted by Drop. They survive this
/// process through the endpoint leases. Disposal must close broker endpoints
/// and descendants before the supervisor releases its identity lease.
pub struct SenderGuard {
    // Declaration order is Drop order: no retained socket may outlive pins.
    endpoint: Option<Endpoint>,
    upstreams: BTreeMap<u64, TcpStream>,
    maps: BTreeMap<String, MapHandle>,
    scope: GuardScope,
    broker: SeqpacketChannel,
    broker_pin: OwnedFd,
    enrolled: bool,
    announced: bool,
    handoff: Option<GuardEnrollment>,
    runtime_pid: u32,
    pins: PinNamespace,
}

impl SenderGuard {
    /// Exact resources whose release the installed certifier must prove.
    pub(crate) fn conformance_map_ids(&self) -> Result<Vec<u32>, GuardError> {
        self.maps
            .values()
            .map(|map| {
                map.info()
                    .map(|info| info.info.id)
                    .map_err(|_| GuardError::Unavailable)
            })
            .collect()
    }
    /// Enrolls at the production measured exec stop before the runtime can run.
    /// # Errors
    /// Returns the sandbox's startup/cleanup failure when enrollment is refused.
    pub(crate) fn start(
        &mut self,
        prepared: crate::sandbox::PreparedSession,
    ) -> Result<crate::sandbox::SandboxedSession, crate::sandbox::SandboxError> {
        let mut session = prepared.start_with_enrollment(|process| {
            self.enroll_at_exec_stop(process)
                .map_err(std::io::Error::other)
        })?;
        if !self.enrolled {
            session.dispose()?;
            return Err(crate::sandbox::SandboxError::Refused(
                "Sender guard requires a measured runtime".into(),
            ));
        }
        Ok(session)
    }
    /// Loads/attaches the embedded object, protects pins, and pins both owners.
    ///
    /// Performs blocking privileged I/O on the launch worker. The authenticated
    /// broker connection must be the install-authorized non-root peer.
    /// No listener exists and no runtime is enrolled when this returns.
    /// # Errors
    /// Refuses invalid scope, a lost/root broker, or missing enforcement support.
    pub fn load(scope: GuardScope, broker: SeqpacketChannel) -> Result<Self, GuardError> {
        validate_scope(&scope)?;
        let credentials = broker.peer_credentials();
        if credentials.uid == 0 || broker.is_closed() {
            return Err(GuardError::Authority);
        }
        let broker_pin = pidfd_open(
            Pid::from_raw(i32::try_from(credentials.pid).map_err(|_| GuardError::Authority)?)
                .ok_or(GuardError::Authority)?,
            PidfdFlags::empty(),
        )
        .map_err(|_| GuardError::Lost)?;
        let pins = PinNamespace::create()?;
        libbpf_rs::set_print(None);
        let object = ObjectBuilder::default()
            .open_memory(super::SENDER_GUARD_OBJECT)
            .and_then(libbpf_rs::OpenObject::load)
            .map_err(|_| GuardError::Unavailable)?;
        let mut programs = Vec::new();
        for program in object.progs_mut() {
            let name = program
                .name()
                .to_str()
                .ok_or(GuardError::Unavailable)?
                .to_owned();
            let mut link = program.attach().map_err(|_| GuardError::Unavailable)?;
            link.pin(format!("/sys/fs/bpf/{name}"))
                .map_err(|_| GuardError::Unavailable)?;
            programs.push(name);
        }
        programs.sort();
        if programs
            != [
                "endpoint_send",
                "invalidate_exec",
                "invalidate_listener",
                "owner_exec",
                "owner_exit",
                "protect_runtime",
            ]
        {
            return Err(GuardError::Unavailable);
        }
        rustix::mount::mount_remount("/sys/fs/bpf", MountFlags::RDONLY, "")
            .map_err(|_| GuardError::Unavailable)?;
        let maps = object
            .maps()
            .map(|map| {
                let name = map
                    .name()
                    .to_str()
                    .ok_or(GuardError::Unavailable)?
                    .to_owned();
                Ok((
                    name,
                    MapHandle::try_from(&map).map_err(|_| GuardError::Unavailable)?,
                ))
            })
            .collect::<Result<_, GuardError>>()?;
        let guard = Self {
            maps,
            pins,
            scope,
            broker,
            broker_pin,
            endpoint: None,
            enrolled: false,
            announced: false,
            handoff: None,
            runtime_pid: 0,
            upstreams: BTreeMap::new(),
        };
        guard.put(
            "lost",
            &0_u32.to_ne_bytes(),
            &0_u32.to_ne_bytes(),
            MapFlags::NO_EXIST,
        )?;
        let owner = pidfd_open(rustix::process::getpid(), PidfdFlags::empty())
            .map_err(|_| GuardError::Lost)?;
        guard.owner(owner.as_fd())?;
        guard.owner(guard.broker_pin.as_fd())?;
        guard.principal(guard.broker_pin.as_fd(), 2)?;
        Ok(guard)
    }

    /// Binds one loopback endpoint in the calling thread's network namespace.
    /// Reservation is installed before listen; policy stays absent until activation.
    /// # Errors
    /// Refuses repeated/non-loopback placement, socket failure or lost ownership.
    pub fn bind_endpoint(
        &mut self,
        address: SocketAddr,
    ) -> Result<GuardedSocket<TcpListener>, GuardError> {
        use rustix::net::{AddressFamily, SocketFlags, SocketType, bind, listen, socket_with};
        if self.endpoint.is_some() || !address.ip().is_loopback() || self.enrolled {
            return Err(GuardError::Authority);
        }
        self.live()?;
        let network = File::open("/proc/thread-self/ns/net").map_err(|_| GuardError::Socket)?;
        let socket = socket_with(
            if address.is_ipv4() {
                AddressFamily::INET
            } else {
                AddressFamily::INET6
            },
            SocketType::STREAM,
            SocketFlags::CLOEXEC,
            None,
        )
        .map_err(|_| GuardError::Socket)?;
        bind(&socket, &address).map_err(|_| GuardError::Socket)?;
        let listener = TcpListener::from(socket);
        let address = listener.local_addr().map_err(|_| GuardError::Socket)?;
        let namespace = namespace_id(&network)?;
        self.put(
            "ports",
            &u32::from(address.port()).to_ne_bytes(),
            &namespace.to_ne_bytes(),
            MapFlags::NO_EXIST,
        )?;
        listen(&listener, 16).map_err(|_| GuardError::Socket)?;
        let cookie =
            rustix::net::sockopt::socket_cookie(&listener).map_err(|_| GuardError::Socket)?;
        let rule = Rule::new(&self.scope, 1, cookie, namespace, address);
        let result = GuardedSocket {
            socket: listener.try_clone().map_err(|_| GuardError::Socket)?,
            pins: self.pins.lease()?,
            network: network.try_clone().map_err(|_| GuardError::Socket)?,
        };
        self.endpoint = Some(Endpoint {
            listener,
            network,
            rule,
        });
        Ok(result)
    }

    // Only the sandbox's measured exec stop calls this, before detaching.
    pub(crate) fn enroll_at_exec_stop(
        &mut self,
        process: &KernelProcess,
    ) -> Result<(), GuardError> {
        if self.enrolled || self.endpoint.is_none() {
            return Err(GuardError::Enrollment);
        }
        let network = File::open(format!("/proc/{}/ns/net", process.credentials().pid))
            .map_err(|_| GuardError::Enrollment)?;
        if self
            .endpoint
            .as_ref()
            .is_none_or(|endpoint| namespace_id(&network) != Ok(endpoint.rule.namespace))
        {
            return Err(GuardError::Authority);
        }
        self.live()?;
        self.principal(process.pidfd(), 1)?;
        self.owner(process.pidfd())?;
        for name in ["tasks", "owners", "lost", "ports"] {
            self.map(name)?
                .freeze()
                .map_err(|_| GuardError::Enrollment)?;
        }
        process
            .bind_sender_guard(self.map("tasks")?, self.map("lost")?)
            .map_err(|_| GuardError::Enrollment)?;
        self.enrolled = true;
        self.runtime_pid = process.credentials().pid;
        self.live()
    }

    /// Publishes the endpoint after enrollment and the authenticated owner response.
    /// The handoff caller owns that protocol ordering; loading is never activation.
    /// # Errors
    /// Refuses missing enrollment, a stale scope or a lost owner.
    pub fn activate(&mut self, scope: &GuardScope) -> Result<(), GuardError> {
        self.check_scope(scope)?;
        if !self.enrolled || !self.announced {
            return Err(GuardError::Enrollment);
        }
        let endpoint = self.endpoint.as_ref().ok_or(GuardError::Enrollment)?;
        self.put(
            "listeners",
            &endpoint.rule.listener.to_ne_bytes(),
            &1_u32.to_ne_bytes(),
            MapFlags::ANY,
        )?;
        self.put(
            "policy",
            &endpoint.rule.port.to_ne_bytes(),
            &endpoint.rule.bytes(),
            MapFlags::ANY,
        )
    }

    /// Revokes before closing retained socket copies. Broker copies still require
    /// explicit shutdown/closure by their owner before identity release.
    /// # Errors
    /// A failed revoke/shutdown is cleanup uncertainty, never permission to reuse.
    pub fn revoke(&mut self) -> Result<(), GuardError> {
        // A revoked enrollment must never activate the same revision again.
        // revise() closes this handoff and requires a fresh broker response.
        self.announced = false;
        let mut failed = false;
        if let Some(endpoint) = &self.endpoint {
            let key = endpoint.rule.port.to_ne_bytes();
            failed = match self.map("policy") {
                Ok(map) => match map.lookup(&key, MapFlags::ANY) {
                    Ok(Some(_)) => map.delete(&key).is_err(),
                    Ok(None) => false,
                    Err(_) => true,
                },
                Err(_) => true,
            };
        }
        for socket in self.upstreams.values() {
            if shutdown(socket).is_err() {
                failed = true;
            }
        }
        if failed {
            Err(GuardError::Cleanup)
        } else {
            self.upstreams.clear();
            Ok(())
        }
    }

    /// Revokes sockets and closes the supervisor's endpoint copy. Call after
    /// confined descendants are gone; the handoff owner closes broker copies
    /// before releasing its leases. Pins are never explicitly removed here.
    /// # Errors
    /// Cleanup uncertainty prevents identity reuse and retains owned resources.
    pub fn dispose(&mut self) -> Result<(), GuardError> {
        self.revoke()?;
        self.close_handoff(Duration::from_secs(5))?;
        self.endpoint = None;
        Ok(())
    }

    /// Applies a newer broker-authorized revision without changing enrolled
    /// processes. Old socket bindings remain invalid permanently. The broker
    /// authorizes the new deadline; this layer enforces it, never grants it.
    /// Requires a new authenticated enrollment response before reactivation.
    /// # Errors
    /// Refuses stale/foreign scope, an expired deadline, lost owners or failed cleanup.
    pub fn revise(&mut self, scope: GuardScope) -> Result<(), GuardError> {
        validate_scope(&scope)?;
        if scope.session_id != self.scope.session_id
            || scope.run_id != self.scope.run_id
            || scope.revision <= self.scope.revision
        {
            return Err(GuardError::Authority);
        }
        self.live()?;
        self.revoke()?;
        self.close_handoff(Duration::from_secs(5))?;
        let endpoint = self.endpoint.as_mut().ok_or(GuardError::Enrollment)?;
        endpoint.rule.revision = scope.revision;
        endpoint.rule.deadline = scope.deadline_ns;
        self.scope = scope;
        self.announced = false;
        Ok(())
    }

    fn check_scope(&self, scope: &GuardScope) -> Result<(), GuardError> {
        if scope != &self.scope {
            return Err(GuardError::Authority);
        }
        validate_scope(&self.scope)?;
        self.live()
    }

    fn live(&self) -> Result<(), GuardError> {
        if self.broker.is_closed()
            || self
                .map("lost")?
                .lookup(&0_u32.to_ne_bytes(), MapFlags::ANY)
                .map_err(|_| GuardError::Lost)?
                .as_deref()
                != Some(&0_u32.to_ne_bytes())
        {
            return Err(GuardError::Lost);
        }
        Ok(())
    }

    fn map(&self, name: &str) -> Result<&MapHandle, GuardError> {
        self.maps.get(name).ok_or(GuardError::Enrollment)
    }

    fn put(&self, name: &str, key: &[u8], value: &[u8], flags: MapFlags) -> Result<(), GuardError> {
        self.map(name)?
            .update(key, value, flags)
            .map_err(|_| GuardError::Enrollment)
    }

    fn owner(&self, pidfd: BorrowedFd<'_>) -> Result<(), GuardError> {
        self.put(
            "owners",
            &pidfd.as_raw_fd().to_ne_bytes(),
            &0_u32.to_ne_bytes(),
            MapFlags::NO_EXIST,
        )
    }

    fn principal(&self, pidfd: BorrowedFd<'_>, role: u64) -> Result<(), GuardError> {
        let bytes: Vec<u8> = [role, 1, 1, 0]
            .into_iter()
            .flat_map(u64::to_ne_bytes)
            .collect();
        self.put(
            "tasks",
            &pidfd.as_raw_fd().to_ne_bytes(),
            &bytes,
            MapFlags::NO_EXIST,
        )
    }
}

fn validate_scope(scope: &GuardScope) -> Result<(), GuardError> {
    scope.validate().map_err(|_| GuardError::Authority)?;
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let now = u64::try_from(now.tv_sec)
        .ok()
        .and_then(|secs| secs.checked_mul(1_000_000_000))
        .and_then(|secs| {
            u64::try_from(now.tv_nsec)
                .ok()
                .and_then(|ns| secs.checked_add(ns))
        });
    if now.is_none_or(|now| now >= scope.deadline_ns) {
        return Err(GuardError::Authority);
    }
    Ok(())
}

fn namespace_id(namespace: &File) -> Result<u32, GuardError> {
    u32::try_from(namespace.metadata().map_err(|_| GuardError::Socket)?.ino())
        .map_err(|_| GuardError::Socket)
}
