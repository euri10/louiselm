//! Exact scope carried by authenticated Sender guard enrollment evidence.
use super::{ErrorCode, ProtocolError, validate_identifier, validate_schema, validate_version};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// Broker request for one supervisor-created, enrolled upstream socket.
pub const GUARD_SOCKET_REQUEST_SCHEMA: &str = "louiselm.launch.guard-socket-request/1";
/// Broker notice that its exact upstream descriptor has been closed.
pub const GUARD_SOCKET_RETIRE_SCHEMA: &str = "louiselm.launch.guard-socket-retire/1";

/// Exact destination chosen by the Control broker after request admission.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardSocketRequest {
    /// Closed wire schema.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Correlates the returned socket and authenticated ACK.
    pub request_id: String,
    /// Current enrolled listener and Session/Run/revision identity.
    pub enrollment: GuardEnrollment,
    /// Approved Provider address and port, never request bytes.
    pub destination: SocketAddr,
}

impl GuardSocketRequest {
    /// Serializes the closed request without whitespace.
    ///
    /// # Panics
    /// Panics only if a future schema change adds a fallible serializer.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema contains only JSON-native values."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a guard socket request is always serializable")
    }

    /// Refuses malformed scope or destination before any socket is created.
    /// # Errors
    /// Returns the exact protocol validation failure.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, GUARD_SOCKET_REQUEST_SCHEMA)?;
        validate_version(self.protocol_version)?;
        validate_identifier(&self.request_id)?;
        self.enrollment.validate()?;
        validate_destination(self.destination)
    }
}

/// One-way, authenticated retirement of a completed guarded upstream.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardSocketRetire {
    /// Closed wire schema.
    pub schema: String,
    /// Protocol version.
    pub protocol_version: u32,
    /// Owning enrollment, never an unscoped cookie.
    pub enrollment: GuardEnrollment,
    /// Kernel identity of the socket the broker closed.
    pub socket_cookie: u64,
}

impl GuardSocketRetire {
    /// Serializes the closed notice.
    ///
    /// # Panics
    /// Panics only if a future schema change adds a fallible serializer.
    #[must_use]
    #[expect(
        clippy::expect_used,
        reason = "Derived schema contains only JSON-native values."
    )]
    pub fn canonical_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("a guard socket retire notice is always serializable")
    }

    /// Validates scope and socket identity before retirement.
    /// # Errors
    /// Refuses malformed or unscoped notices.
    pub fn validate(&self) -> Result<(), ProtocolError> {
        validate_schema(&self.schema, GUARD_SOCKET_RETIRE_SCHEMA)?;
        validate_version(self.protocol_version)?;
        self.enrollment.validate()?;
        if self.socket_cookie == 0 {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        Ok(())
    }
}

/// One Session/Run/revision and its exclusive host monotonic deadline.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardScope {
    /// Owning Session.
    pub session_id: String,
    /// Owning Run.
    pub run_id: String,
    /// Current capability-envelope revision.
    pub revision: u64,
    /// Exclusive `CLOCK_MONOTONIC` deadline in nanoseconds.
    pub deadline_ns: u64,
}

/// Post-enrollment evidence, authoritative only on the original supervisor channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardEnrollment {
    /// Enrolled Session/Run/revision and monotonic deadline.
    pub scope: GuardScope,
    /// Retained private pin-namespace identity.
    pub guard_id: u64,
    /// Measured runtime process.
    pub runtime_pid: u32,
    /// Original enrolled Control broker process.
    pub broker_pid: u32,
    /// Exact loopback listener in the Session network namespace.
    pub address: SocketAddr,
    /// Kernel identity of the retained listener, which cannot be reused with its port.
    pub listener_cookie: u64,
    /// Inode of the retained Session network namespace.
    pub network_id: u32,
}

impl GuardEnrollment {
    pub(crate) fn validate(&self) -> Result<(), ProtocolError> {
        self.scope.validate()?;
        if self.guard_id == 0
            || self.runtime_pid == 0
            || self.broker_pid == 0
            || self.listener_cookie == 0
            || self.network_id == 0
            || self.address.port() == 0
            || !self.address.ip().is_loopback()
        {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        Ok(())
    }
}

impl GuardScope {
    pub(crate) fn validate(&self) -> Result<(), ProtocolError> {
        validate_identifier(&self.session_id)?;
        validate_identifier(&self.run_id)?;
        if self.revision == 0 || self.deadline_ns == 0 {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        Ok(())
    }
}

/// Exact supervisor-created upstream socket belonging to one enrolled endpoint.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GuardUpstream {
    /// Immutable runtime, broker and Session endpoint binding.
    pub enrollment: GuardEnrollment,
    /// Broker-selected destination; no request bytes or credentials are included.
    pub destination: SocketAddr,
    /// Kernel identity of this connected socket.
    pub socket_cookie: u64,
    /// Retained network namespace in which the supervisor connected it.
    pub network_id: u32,
}

impl GuardUpstream {
    pub(crate) fn validate(&self) -> Result<(), ProtocolError> {
        self.enrollment.validate()?;
        if self.socket_cookie == 0 || self.network_id == 0 {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        validate_destination(self.destination)
    }
}

fn validate_destination(destination: SocketAddr) -> Result<(), ProtocolError> {
    if destination.port() == 0
        || destination.ip().is_unspecified()
        || destination.ip().is_multicast()
    {
        Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None))
    } else {
        Ok(())
    }
}
