//! A broker-owned upstream stream keeps its protected namespace leases alive.
use super::BrokerError;
use crate::{
    launch_protocol::{GuardEnrollment, GuardUpstream, ResponseResult},
    launch_transport::{AuthenticatedPacket, KernelCredentials, LauncherPacket},
};
use std::{
    fs::File,
    io::{self, Read, Write},
    net::{SocketAddr, TcpStream},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::Arc,
    time::Duration,
};

pub(super) struct SocketLease {
    socket: TcpStream,
    pins: File,
    network: File,
    _owner_lease: Arc<()>,
}

impl SocketLease {
    pub(super) fn shutdown(&self) -> io::Result<()> {
        match self.socket.shutdown(std::net::Shutdown::Both) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotConnected => Ok(()),
            other => other,
        }
    }
}

/// One authenticated guarded upstream connection. No API removes its namespace
/// leases or duplicates the socket. Drop it before acknowledging Session disposal.
pub struct GuardedUpstream {
    pub(super) lease: Arc<SocketLease>,
    evidence: GuardUpstream,
}

impl GuardedUpstream {
    pub(super) fn adopt(
        packet: AuthenticatedPacket,
        supervisor: KernelCredentials,
        enrollment: &GuardEnrollment,
        request_id: &str,
        destination: SocketAddr,
        owner_lease: Arc<()>,
    ) -> Result<Self, BrokerError> {
        if packet.peer_credentials != supervisor || packet.message_credentials != supervisor {
            return Err(BrokerError::InvalidGrant);
        }
        let LauncherPacket::Response(response) = packet.packet else {
            return Err(BrokerError::InvalidGrant);
        };
        let ResponseResult::SenderGuardUpstream { socket: evidence } = response.result else {
            return Err(BrokerError::InvalidGrant);
        };
        evidence.validate()?;
        if response.request_id != request_id
            || evidence.enrollment != *enrollment
            || evidence.destination != destination
        {
            return Err(BrokerError::RequestMismatch);
        }
        let [socket, pins, network] = packet.descriptors.ok_or(BrokerError::InvalidGrant)?;
        let lease = Arc::new(SocketLease {
            socket: TcpStream::from(socket),
            pins: File::from(pins),
            network: File::from(network),
            _owner_lease: owner_lease,
        });
        for (file, kind, id) in [
            (&lease.pins, "mnt", evidence.enrollment.guard_id),
            (&lease.network, "net", u64::from(evidence.network_id)),
        ] {
            if file
                .metadata()
                .map_err(|_| BrokerError::InvalidGrant)?
                .ino()
                != id
                || std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
                    .map_err(|_| BrokerError::InvalidGrant)?
                    != std::path::Path::new(&format!("{kind}:[{id}]"))
            {
                return Err(BrokerError::InvalidGrant);
            }
        }
        if lease
            .socket
            .peer_addr()
            .map_err(|_| BrokerError::InvalidGrant)?
            != destination
            || rustix::net::sockopt::socket_cookie(&lease.socket)
                .map_err(|_| BrokerError::InvalidGrant)?
                != evidence.socket_cookie
            || rustix::net::sockopt::socket_type(&lease.socket)
                .map_err(|_| BrokerError::InvalidGrant)?
                != rustix::net::SocketType::STREAM
        {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(Self { lease, evidence })
    }

    /// Exact immutable socket evidence used by retirement and acknowledgements.
    #[must_use]
    pub const fn evidence(&self) -> &GuardUpstream {
        &self.evidence
    }

    /// Bounds blocking reads and writes; TLS/HTTP owners must use their remaining expiry.
    /// # Errors
    /// Returns the underlying socket option failure; zero durations are invalid.
    pub fn set_timeout(&self, timeout: Duration) -> io::Result<()> {
        self.lease.socket.set_read_timeout(Some(timeout))?;
        self.lease.socket.set_write_timeout(Some(timeout))
    }
}

impl Read for GuardedUpstream {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        (&self.lease.socket).read(buffer)
    }
}
impl Write for GuardedUpstream {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        (&self.lease.socket).write(bytes)
    }
    fn flush(&mut self) -> io::Result<()> {
        (&self.lease.socket).flush()
    }
}

impl Drop for GuardedUpstream {
    fn drop(&mut self) {
        // The supervisor owns another copy for retirement. Shut down all copies
        // before releasing this receiver's leases. Failure cannot acknowledge
        // cleanup: the supervisor still requires its own successful retirement.
        let _ = self.lease.shutdown();
    }
}
