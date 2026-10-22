//! Separate root-authenticated endpoint; ordinary operator/Agent requests never
//! gain recovery authority. It carries only exact bindings and signed outcomes.

use super::{InspectError, OperatorServer, READY, TIMEOUT, exchange_document, peer_uid, wire};
use crate::{
    broker::consumed_recovery::Snapshot,
    launch_recovery::{Acknowledgement, Inspection, Proof},
};
use serde::{Deserialize, Serialize};
use std::{io, path::Path, time::Instant};

/// Fixed root-only protocol endpoint owned by the existing unprivileged broker.
pub const SOCKET: &str = "/run/louiselm-operator/recovery.sock";

/// Closed requests permitted only after the kernel proves the caller is root.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum Request {
    /// Read durable history and recovery, never live status or cleanup authority.
    #[serde(rename = "louiselm.consumed-recovery.inspect/1")]
    Inspect {
        /// Exact original consumed authorization identity.
        authorization_id: String,
    },
    /// Read exact private consumption metadata without changing its reservation.
    #[serde(rename = "louiselm.consumed-recovery.prepare/1")]
    Prepare {
        /// Original consumed authorization identity, never a guessed Session.
        authorization_id: String,
    },
    /// Store the actual root-signed recovery outcome; never execute mechanics.
    #[serde(rename = "louiselm.consumed-recovery.commit/1")]
    Commit {
        /// Exact signed terminal recovery outcome.
        proof: Box<Proof>,
    },
}

/// Durable broker observations; no response authorizes a second launch.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Response {
    /// Verified historical state and explicitly separate recovery outcome.
    Inspection {
        /// Durable evidence, not a current supervisor observation.
        inspection: Box<Inspection>,
    },
    /// Original consumption and any already completed recovery.
    Snapshot {
        /// Exact authenticated state.
        snapshot: Box<Snapshot>,
    },
    /// Exact signed outcome is durably owned by the broker.
    Acknowledged {
        /// Durable-before-reply acknowledgement.
        acknowledgement: Acknowledgement,
    },
}

/// Exchange one bounded recovery document with the installed broker identity.
/// # Errors
/// Refuses unavailable transport, foreign kernel identity or noncanonical replies.
pub fn exchange(path: &Path, broker_uid: u32, request: &Request) -> Result<Response, InspectError> {
    let bytes = exchange_document(
        path,
        broker_uid,
        request,
        TIMEOUT,
        crate::launch_protocol::MAX_PROTOCOL_MESSAGE_BYTES,
    )?;
    let response: Response =
        serde_json::from_slice(&bytes).map_err(|_| InspectError::StatusUnavailable)?;
    if serde_json::to_vec(&response).map_err(|_| InspectError::StatusUnavailable)? != bytes {
        return Err(InspectError::StatusUnavailable);
    }
    Ok(response)
}

impl OperatorServer {
    /// Serve only root recovery requests on a separately owned listener.
    /// Authenticate before reading or looking up any target. Blocking worker I/O.
    /// # Errors
    /// Returns listener failure; client/refusal failures remain connection-local.
    pub fn serve_recovery_once(
        &self,
        handle: impl FnOnce(Request) -> Result<Response, InspectError>,
    ) -> io::Result<()> {
        let (mut stream, _) = self.listener.accept()?;
        let deadline = Instant::now() + TIMEOUT;
        let result = (|| {
            if self.operator_uid != 0 || peer_uid(&stream)? != 0 {
                return Err(InspectError::AuthenticationRefused);
            }
            wire::write(&mut stream, READY, deadline)
                .map_err(|_| InspectError::BrokerUnavailable)?;
            let bytes =
                wire::read(&mut stream, deadline).map_err(|_| InspectError::InvalidRequest)?;
            let request: Request =
                serde_json::from_slice(&bytes).map_err(|_| InspectError::InvalidRequest)?;
            if serde_json::to_vec(&request).map_err(|_| InspectError::InvalidRequest)? != bytes {
                return Err(InspectError::InvalidRequest);
            }
            serde_json::to_vec(&handle(request)?).map_err(|_| InspectError::StatusUnavailable)
        })();
        let bytes = result.unwrap_or_else(InspectError::canonical_bytes);
        // Lost delivery never rolls back completed durable recovery.
        let _ = wire::write(&mut stream, &bytes, deadline);
        Ok(())
    }
}
