//! Exact scope carried by authenticated Sender guard enrollment evidence.
use super::{ErrorCode, ProtocolError, validate_identifier};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

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
        if self.socket_cookie == 0
            || self.network_id == 0
            || self.destination.port() == 0
            || self.destination.ip().is_unspecified()
            || self.destination.ip().is_multicast()
        {
            return Err(ProtocolError::new(ErrorCode::InvalidRequest, None, None));
        }
        Ok(())
    }
}
