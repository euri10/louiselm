//! Native map encoding; each object owns one complete Rust scope.
use crate::launch_protocol::GuardScope;
use std::net::SocketAddr;

pub(super) struct Rule {
    pub(super) launch: u64,
    pub(super) revision: u64,
    pub(super) listener: u64,
    pub(super) deadline: u64,
    pub(super) namespace: u32,
    pub(super) family: u32,
    pub(super) address: [u8; 16],
    pub(super) port: u32,
}

impl Rule {
    pub(super) fn new(
        scope: &GuardScope,
        launch: u64,
        listener: u64,
        namespace: u32,
        address: SocketAddr,
    ) -> Self {
        let mut bytes = [0; 16];
        match address.ip() {
            std::net::IpAddr::V4(ip) => bytes[..4].copy_from_slice(&ip.octets()),
            std::net::IpAddr::V6(ip) => bytes.copy_from_slice(&ip.octets()),
        }
        Self {
            launch,
            revision: scope.revision,
            listener,
            deadline: scope.deadline_ns,
            namespace,
            family: if address.is_ipv4() { 2 } else { 10 },
            address: bytes,
            port: u32::from(address.port()),
        }
    }

    pub(super) fn bytes(&self) -> Vec<u8> {
        // One guard object owns one full Rust scope. These map-local role/scope
        // tokens cannot cross objects; no truncated hash authenticates a scope.
        let mut bytes: Vec<u8> = [
            self.launch,
            1,
            1,
            self.revision,
            self.listener,
            self.deadline,
        ]
        .into_iter()
        .flat_map(u64::to_ne_bytes)
        .collect();
        bytes.extend_from_slice(&self.namespace.to_ne_bytes());
        bytes.extend_from_slice(&self.family.to_ne_bytes());
        bytes.extend_from_slice(&self.address);
        bytes
    }
}
