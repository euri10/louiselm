//! Ownership of an authenticated Session listener and its namespace leases.

use std::{
    fs::File,
    net::{TcpListener, TcpStream},
    os::{fd::AsRawFd, unix::fs::MetadataExt},
    sync::Arc,
    time::Duration,
};

use crate::{
    launch_protocol::{GuardEnrollment, GuardScope, ResponseResult},
    launch_transport::{AuthenticatedPacket, KernelCredentials, LauncherPacket},
};

use super::BrokerError;

/// Fields drop in socket-before-lease order. Accepted sockets never escape the
/// serialized Session worker, which retains this owner until serving completes.
pub(super) struct ProviderListener {
    listener: TcpListener,
    pins: File,
    network: File,
    pub(super) enrollment: GuardEnrollment,
    _owner_lease: Arc<()>,
}

impl ProviderListener {
    pub(super) fn adopt(
        packet: AuthenticatedPacket,
        supervisor: KernelCredentials,
        scope: &GuardScope,
        runtime_pid: u32,
        owner_lease: Arc<()>,
    ) -> Result<(Self, String), BrokerError> {
        if packet.peer_credentials != supervisor || packet.message_credentials != supervisor {
            return Err(BrokerError::InvalidGrant);
        }
        let LauncherPacket::Response(response) = packet.packet else {
            return Err(BrokerError::InvalidGrant);
        };
        let ResponseResult::SenderGuardEnrolled { enrollment } = response.result else {
            return Err(BrokerError::InvalidGrant);
        };
        enrollment.validate()?;
        if enrollment.scope != *scope
            || enrollment.runtime_pid != runtime_pid
            || enrollment.broker_pid != std::process::id()
        {
            return Err(BrokerError::RequestMismatch);
        }
        let [socket, pins, network] = packet.descriptors.ok_or(BrokerError::InvalidGrant)?;
        // Construct socket ownership first: every failure drops it before leases.
        let listener = TcpListener::from(socket);
        let pins = File::from(pins);
        let network = File::from(network);
        let owner = Self {
            listener,
            pins,
            network,
            enrollment,
            _owner_lease: owner_lease,
        };
        owner.validate()?;
        owner
            .listener
            .set_nonblocking(true)
            .map_err(BrokerError::Storage)?;
        Ok((owner, response.request_id))
    }

    fn validate(&self) -> Result<(), BrokerError> {
        use rustix::net::{AddressFamily, SocketType, sockopt};
        let refused = |_| BrokerError::InvalidGrant;
        let expected = &self.enrollment;
        for (file, kind, id) in [
            (&self.pins, "mnt", expected.guard_id),
            (&self.network, "net", u64::from(expected.network_id)),
        ] {
            if std::fs::read_link(format!("/proc/self/fd/{}", file.as_raw_fd()))
                .map_err(|_| BrokerError::InvalidGrant)?
                != std::path::Path::new(&format!("{kind}:[{id}]"))
            {
                return Err(BrokerError::InvalidGrant);
            }
        }
        let domain = if expected.address.is_ipv4() {
            AddressFamily::INET
        } else {
            AddressFamily::INET6
        };
        if sockopt::socket_domain(&self.listener).map_err(refused)? != domain
            || sockopt::socket_type(&self.listener).map_err(refused)? != SocketType::STREAM
            || !sockopt::socket_acceptconn(&self.listener).map_err(refused)?
            || sockopt::socket_cookie(&self.listener).map_err(refused)? != expected.listener_cookie
            || self
                .listener
                .local_addr()
                .map_err(|_| BrokerError::InvalidGrant)?
                != expected.address
            || self
                .pins
                .metadata()
                .map_err(|_| BrokerError::InvalidGrant)?
                .ino()
                != expected.guard_id
            || self
                .network
                .metadata()
                .map_err(|_| BrokerError::InvalidGrant)?
                .ino()
                != u64::from(expected.network_id)
        {
            return Err(BrokerError::InvalidGrant);
        }
        Ok(())
    }

    pub(super) fn accept(&self) -> Result<Option<TcpStream>, BrokerError> {
        let stream = match self.listener.accept() {
            Ok((stream, _)) => stream,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
            Err(_) => return Err(BrokerError::ProviderUnavailable),
        };
        // Idle or incomplete runtime requests cannot hold the Session worker
        // forever. The upstream exchange has its separately enforced expiry.
        let timeout = Some(Duration::from_secs(1));
        stream
            .set_read_timeout(timeout)
            .map_err(|_| BrokerError::ProviderUnavailable)?;
        stream
            .set_write_timeout(timeout)
            .map_err(|_| BrokerError::ProviderUnavailable)?;
        Ok(Some(stream))
    }
}

#[cfg(test)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        clippy::panic,
        reason = "Fixtures assert exact handoff refusals."
    )]
    use super::*;
    use crate::launch_protocol::{PROTOCOL_VERSION, ProtocolResponse, RESPONSE_SCHEMA};
    use std::os::fd::AsFd;

    fn fixture_packet() -> (AuthenticatedPacket, GuardScope, KernelCredentials) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let pins = File::open("/proc/self/ns/mnt").unwrap();
        let network = File::open("/proc/self/ns/net").unwrap();
        let scope = GuardScope {
            session_id: "session".into(),
            run_id: "run".into(),
            revision: 1,
            deadline_ns: u64::MAX,
        };
        let enrollment = GuardEnrollment {
            scope: scope.clone(),
            guard_id: pins.metadata().unwrap().ino(),
            runtime_pid: 123,
            broker_pid: std::process::id(),
            address: listener.local_addr().unwrap(),
            listener_cookie: rustix::net::sockopt::socket_cookie(&listener).unwrap(),
            network_id: network.metadata().unwrap().ino().try_into().unwrap(),
        };
        let response = ProtocolResponse {
            schema: RESPONSE_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "handoff".into(),
            result: ResponseResult::SenderGuardEnrolled { enrollment },
        };
        let credentials = KernelCredentials {
            pid: std::process::id(),
            uid: 0,
            gid: 0,
        };
        (
            AuthenticatedPacket {
                bytes: response.canonical_bytes(),
                packet: LauncherPacket::Response(Box::new(response)),
                peer_credentials: credentials,
                message_credentials: credentials,
                descriptors: Some([
                    listener.as_fd().try_clone_to_owned().unwrap(),
                    pins.into(),
                    network.into(),
                ]),
            },
            scope,
            credentials,
        )
    }

    #[test]
    fn exact_listener_is_retained_and_foreign_scope_is_refused() {
        let (packet, scope, peer) = fixture_packet();
        let (owner, request) =
            ProviderListener::adopt(packet, peer, &scope, 123, Arc::new(())).unwrap();
        assert_eq!(request, "handoff");
        assert!(owner.accept().unwrap().is_none());
        let (packet, mut scope, peer) = fixture_packet();
        scope.session_id = "other".into();
        assert!(matches!(
            ProviderListener::adopt(packet, peer, &scope, 123, Arc::new(())),
            Err(BrokerError::RequestMismatch)
        ));
    }

    #[test]
    fn bad_cookie_missing_leases_and_wrong_runtime_are_refused() {
        for mutation in 0..3 {
            let (mut packet, scope, peer) = fixture_packet();
            if mutation == 0 {
                let LauncherPacket::Response(response) = &mut packet.packet else {
                    panic!("response")
                };
                let ResponseResult::SenderGuardEnrolled { enrollment } = &mut response.result
                else {
                    panic!("enrollment")
                };
                enrollment.listener_cookie += 1;
            } else if mutation == 1 {
                packet.descriptors = None;
            }
            let error = ProviderListener::adopt(
                packet,
                peer,
                &scope,
                if mutation == 2 { 124 } else { 123 },
                Arc::new(()),
            );
            assert!(matches!(
                error,
                Err(BrokerError::InvalidGrant | BrokerError::RequestMismatch)
            ));
        }
    }
}
