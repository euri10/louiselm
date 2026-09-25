//! Socket handoff leases, exact upstream registration and shutdown.
use super::{GuardError, Rule, SenderGuard, namespace_id};
use crate::launch_protocol::GuardScope;
use libbpf_rs::{MapCore, MapFlags};
use std::{
    fs::File,
    net::{SocketAddr, TcpListener, TcpStream},
    os::fd::{AsFd, AsRawFd, BorrowedFd},
    time::Duration,
};

/// A socket and the namespace references which must accompany every handoff.
///
/// Drop/shutdown the socket before releasing either lease. No BPF descriptor
/// is exposed. The authenticated handoff layer must retain the leases for its
/// accepted connections too, including on partial transfer and broker crash.
pub struct GuardedSocket<T> {
    pub(super) socket: T,
    pub(super) pins: File,
    pub(super) network: File,
}

impl<T: AsFd> GuardedSocket<T> {
    /// Ordered handoff bundle: socket, read-only pin namespace, network namespace.
    /// The receiver must retain both leases until all derived sockets are closed.
    #[must_use]
    pub fn descriptors(&self) -> [BorrowedFd<'_>; 3] {
        [self.socket.as_fd(), self.pins.as_fd(), self.network.as_fd()]
    }

    /// Kernel socket identity used to retire a completed upstream connection.
    /// # Errors
    /// Returns a sanitized failure when the socket identity cannot be read.
    pub fn cookie(&self) -> Result<u64, GuardError> {
        rustix::net::sockopt::socket_cookie(&self.socket).map_err(|_| GuardError::Socket)
    }
}

impl GuardedSocket<TcpListener> {
    /// Bound endpoint address, including the assigned ephemeral port.
    /// # Errors
    /// Returns a sanitized socket observation failure.
    pub fn local_addr(&self) -> Result<SocketAddr, GuardError> {
        self.socket.local_addr().map_err(|_| GuardError::Socket)
    }
}

impl SenderGuard {
    /// Places the loopback listener in the exact blocked Session namespace.
    /// The namespace leader must still belong to its proved process tree.
    /// A scoped worker enters only that network namespace and exits after bind;
    /// the supervisor keeps its original network namespace for upstream sockets.
    /// # Errors
    /// Refuses foreign/unproved Sessions, inaccessible namespaces or bind failure.
    pub fn bind_session_endpoint(
        &mut self,
        prepared: &crate::sandbox::PreparedSession,
        address: SocketAddr,
    ) -> Result<GuardedSocket<TcpListener>, GuardError> {
        if prepared.session_id() != self.scope.session_id {
            return Err(GuardError::Authority);
        }
        let leader = prepared.sandbox_leader_pid().ok_or(GuardError::Authority)?;
        if !prepared
            .processes()
            .map_err(|_| GuardError::Authority)?
            .contains(&leader)
        {
            return Err(GuardError::Authority);
        }
        let network =
            File::open(format!("/proc/{leader}/ns/net")).map_err(|_| GuardError::Socket)?;
        self.bind_in_namespace(&network, address)
    }

    pub(super) fn bind_in_namespace(
        &mut self,
        network: &File,
        address: SocketAddr,
    ) -> Result<GuardedSocket<TcpListener>, GuardError> {
        std::thread::scope(|threads| {
            std::thread::Builder::new()
                .name("louiselm-guard-bind".into())
                .spawn_scoped(threads, || {
                    rustix::thread::move_into_link_name_space(
                        network.as_fd(),
                        Some(rustix::thread::LinkNameSpaceType::Network),
                    )
                    .map_err(|_| GuardError::Socket)?;
                    self.bind_endpoint(address)
                })
                .map_err(|_| GuardError::Socket)?
                .join()
                .map_err(|_| GuardError::Socket)?
        })
    }

    /// Connects and registers one broker-approved destination before descriptor handoff.
    /// No TLS/request bytes are sent here. Call only after broker request admission;
    /// this method performs mechanics and never chooses a destination or retries.
    /// # Errors
    /// Refuses stale/lost authority or failed connection/enforcement registration.
    pub fn connect_upstream(
        &mut self,
        scope: &GuardScope,
        destination: SocketAddr,
        timeout: Duration,
    ) -> Result<GuardedSocket<TcpStream>, GuardError> {
        self.check_scope(scope)?;
        // Bound retained descriptors even if the handoff consumer stops retiring
        // completed connections. This is a mechanics limit, not request policy.
        if self.upstreams.len() >= 128 {
            return Err(GuardError::Capacity);
        }
        let endpoint = self.endpoint.as_ref().ok_or(GuardError::Enrollment)?;
        let key = endpoint.rule.port.to_ne_bytes();
        if self
            .map("policy")?
            .lookup(&key, MapFlags::ANY)
            .map_err(|_| GuardError::Enrollment)?
            .is_none()
        {
            return Err(GuardError::Enrollment);
        }
        let network = File::open("/proc/thread-self/ns/net").map_err(|_| GuardError::Socket)?;
        let socket =
            TcpStream::connect_timeout(&destination, timeout).map_err(|_| GuardError::Socket)?;
        self.check_scope(scope)?;
        let rule = Rule::new(
            scope,
            2,
            endpoint.rule.listener,
            namespace_id(&network)?,
            destination,
        );
        let mut bytes = rule.bytes();
        bytes.extend_from_slice(&endpoint.rule.port.to_ne_bytes());
        bytes.extend_from_slice(&rule.port.to_ne_bytes());
        self.put(
            "upstreams",
            &socket.as_raw_fd().to_ne_bytes(),
            &bytes,
            MapFlags::NO_EXIST,
        )?;
        let retained = socket.try_clone().map_err(|_| GuardError::Socket)?;
        let pins = self.pins.lease()?;
        let cookie =
            rustix::net::sockopt::socket_cookie(&socket).map_err(|_| GuardError::Socket)?;
        self.upstreams.insert(cookie, retained);
        Ok(GuardedSocket {
            socket,
            pins,
            network,
        })
    }

    /// Shuts down all descriptor copies of one completed upstream connection.
    /// Retire by its cookie after request completion; at most 128 are retained.
    /// The broker still closes its descriptors and accompanying namespace leases.
    /// # Errors
    /// Refuses foreign/stale scope, an unknown cookie or unproven shutdown.
    pub fn retire_upstream(&mut self, scope: &GuardScope, cookie: u64) -> Result<(), GuardError> {
        self.check_scope(scope)?;
        let socket = self.upstreams.get(&cookie).ok_or(GuardError::Socket)?;
        shutdown(socket)?;
        self.upstreams.remove(&cookie);
        Ok(())
    }
}

pub(super) fn shutdown(socket: &TcpStream) -> Result<(), GuardError> {
    match socket.shutdown(std::net::Shutdown::Both) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotConnected => Ok(()),
        Err(_) => Err(GuardError::Cleanup),
    }
}
