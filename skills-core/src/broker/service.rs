//! The broker's side of the fixed launch rendezvous.
//!
//! One authenticated supervisor connection carries one launch transaction: the
//! supervisor presents its exact launch request, the broker consumes the
//! pending authorization, and the two sequence-0 and sequence-1 receipts are
//! stored and acknowledged in order. Success hands the same connection to an
//! explicit Session owner for continued control. Every packet is authenticated
//! by kernel credentials, never by anything the peer says about itself.
//!
//! Failure is fail-closed at every step: a refusal answers with a stable typed
//! error, and a durability failure answers with nothing at all, because an
//! acknowledgement is the launch's proof that bytes reached the disk.

use std::{path::Path, sync::mpsc, time::Duration};

use crate::{
    broker::{AuditDecision, AuditEntry, AuditLog, AuthorizationStore, BrokerError, ReceiptStore},
    launch::PROTOCOL_VERSION,
    launch_protocol::{
        ErrorCode, LaunchAuthorization, ProtocolError, ProtocolMessage, ProtocolResponse,
        RESPONSE_SCHEMA, ResponseResult,
    },
    launch_receipt::{ReceiptHead, SessionState},
    launch_transport::{
        AuthenticatedPacket, CredentialPin, LauncherPacket, SeqpacketChannel, SeqpacketListener,
        TransportError,
    },
};

/// Longest a broker worker waits for one transport step to complete.
///
/// The transport itself has no deadline. Without one here a supervisor that
/// connects and then says nothing would hold this worker forever.
const STEP_TIMEOUT: Duration = Duration::from_secs(30);

/// Owns the authenticated supervisor connection after a durably completed launch.
///
/// Keep this owner alive for the continuing broker worker. Closing or dropping
/// it shuts down the connection, including any channel clones. That triggers the
/// supervisor's existing broker-loss handling; it is not proof that revocation
/// or process cleanup has completed. Listener lifetime is independent.
#[must_use = "Dropping the Session owner closes its supervisor connection."]
pub struct BrokerSession {
    authorization: LaunchAuthorization,
    launch_head: ReceiptHead,
    channel: SeqpacketChannel,
}

impl BrokerSession {
    /// Exact single-use authorization consumed for this launch.
    #[must_use]
    pub const fn authorization(&self) -> &LaunchAuthorization {
        &self.authorization
    }

    /// Initial sequence-1 durable head, not a live status or later receipt head.
    #[must_use]
    pub const fn launch_head(&self) -> &ReceiptHead {
        &self.launch_head
    }

    /// Original authenticated channel for the continuing broker worker.
    ///
    /// The worker owns asynchronous packet ordering and correlation. Access to
    /// this transport does not itself authorize Agent or tool effects.
    #[must_use]
    pub const fn channel(&self) -> &SeqpacketChannel {
        &self.channel
    }

    /// Closes the supervisor transport immediately; repeated calls are harmless.
    pub fn close(&self) {
        self.channel.close();
    }
}

impl Drop for BrokerSession {
    fn drop(&mut self) {
        self.close();
    }
}

/// Broker-owned state an operator may read about one Session.
///
/// Every field is a bounded normalized identifier, a slot number, a durable
/// head, or a stable typed failure. Nothing derived from a prompt, an
/// environment, a command, or a receipt payload appears here.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionInspection {
    /// Session being described.
    pub session_id: String,
    /// Run that owns the Session.
    pub run_id: String,
    /// Capability-envelope revision the launch was authorized for.
    pub envelope_revision: u64,
    /// Installed identity slot bound to the Session.
    pub identity_slot: u32,
    /// State the last durable receipt recorded.
    pub state: SessionState,
    /// Exact receipt head the broker durably stored.
    pub broker_head: Option<ReceiptHead>,
    /// Latest stable failure the broker recorded for this Session.
    pub last_failure: Option<ProtocolError>,
}

/// The Control broker's local-only rendezvous service.
pub struct BrokerService {
    listener: SeqpacketListener,
    authorizations: AuthorizationStore,
    receipts: ReceiptStore,
    audit: AuditLog,
    supervisor: CredentialPin,
}

impl BrokerService {
    /// Binds the fixed rendezvous and prepares the launch transaction.
    ///
    /// The path is never replaced: a rendezvous that already exists belongs to
    /// a running broker, and taking it over would strand that broker's
    /// Sessions.
    ///
    /// # Errors
    /// Returns [`BrokerError::Transport`] when the rendezvous cannot be bound.
    pub fn bind(
        socket_path: &Path,
        authorizations: AuthorizationStore,
        receipts: ReceiptStore,
        audit: AuditLog,
        supervisor: CredentialPin,
    ) -> Result<Self, BrokerError> {
        let listener = SeqpacketListener::bind(socket_path).map_err(BrokerError::Transport)?;
        Ok(Self {
            listener,
            authorizations,
            receipts,
            audit,
            supervisor,
        })
    }

    /// The operator record of this broker's decisions, oldest first.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] when the record cannot be read.
    pub fn audit(&self) -> Result<Vec<AuditEntry>, BrokerError> {
        self.audit.entries()
    }

    /// Returns broker-owned state for one Session, or `None` when this broker
    /// never launched it.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] when durable state cannot be read.
    pub fn inspect(&self, session_id: &str) -> Result<Option<SessionInspection>, BrokerError> {
        let Some(authorization) = self.authorizations.consumed_for_session(session_id)? else {
            return Ok(None);
        };
        let last_failure = self
            .audit()?
            .into_iter()
            .filter(|entry| entry.session_id == session_id)
            .filter_map(|entry| match entry.decision {
                AuditDecision::AuthorizationRefused { error }
                | AuditDecision::ReceiptRefused { error } => {
                    Some(ProtocolError::new(error, None, None))
                }
                _ => None,
            })
            .next_back();
        Ok(Some(SessionInspection {
            session_id: authorization.session_id.clone(),
            run_id: authorization.run_id.clone(),
            envelope_revision: authorization.envelope_revision,
            identity_slot: authorization.identity.slot,
            state: self
                .receipts
                .state(session_id)?
                .unwrap_or(SessionState::Starting),
            broker_head: self.receipts.head(session_id)?,
            last_failure,
        }))
    }

    /// The durable authorizations this service consumes.
    #[must_use]
    pub const fn authorizations(&self) -> &AuthorizationStore {
        &self.authorizations
    }

    /// The durable receipt chains this service appends to.
    #[must_use]
    pub const fn receipts(&self) -> &ReceiptStore {
        &self.receipts
    }

    /// Accepts one authenticated supervisor and runs its launch transaction.
    ///
    /// Returns once sequence 1 is durably acknowledged, which is the point at
    /// which the supervisor may report launch success. The returned owner keeps
    /// the original authenticated connection alive for continued control. This
    /// method blocks and belongs on the broker's I/O worker.
    ///
    /// # Errors
    /// Returns [`BrokerError::Transport`] for connection failures, the refusal
    /// this broker sent the supervisor when it refused the launch, and
    /// [`BrokerError::Storage`] when durable state failed. A refusal is
    /// answered on the wire before it is returned here; a storage failure is
    /// never acknowledged at all.
    pub fn serve_launch<F>(
        &self,
        now_ms: u64,
        mut verify_signature: F,
    ) -> Result<BrokerSession, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let channel = self.accept()?;
        match self.transaction(&channel, now_ms, &mut verify_signature) {
            Ok((authorization, launch_head)) => Ok(BrokerSession {
                authorization,
                launch_head,
                channel,
            }),
            Err(error) => {
                channel.close();
                Err(error)
            }
        }
    }

    /// Closes the rendezvous listener. Bound paths are not unlinked here: the
    /// installer owns the rendezvous path's lifetime. Returned Session owners
    /// remain usable and must be closed separately.
    pub fn close(&self) {
        self.listener.close();
    }

    fn transaction<F>(
        &self,
        channel: &SeqpacketChannel,
        now_ms: u64,
        verify_signature: &mut F,
    ) -> Result<(LaunchAuthorization, ReceiptHead), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let packet = receive(channel)?;
        let LauncherPacket::Request(ProtocolMessage::LaunchAuthorization(request)) = packet.packet
        else {
            // Nothing correlates a response to a packet that is not a launch
            // request, so the transaction ends without one.
            return Err(BrokerError::InvalidGrant);
        };

        let authorization = match self.authorizations.consume_for_launcher(&request, now_ms) {
            Ok(authorization) => authorization,
            Err(BrokerError::IdentityExhausted(exhaustion)) => {
                send(
                    channel,
                    response(
                        &request.request_id,
                        ResponseResult::IdentityExhaustion {
                            exhaustion: exhaustion.as_ref().clone(),
                        },
                    ),
                )?;
                return Err(BrokerError::IdentityExhausted(exhaustion));
            }
            Err(BrokerError::Storage(error)) => return Err(BrokerError::Storage(error)),
            Err(refusal) => {
                self.record(
                    &request.session_id,
                    &request.run_id,
                    &request.authorization_id,
                    None,
                    now_ms,
                    AuditDecision::AuthorizationRefused {
                        error: ErrorCode::InvalidRequest,
                    },
                )?;
                send(
                    channel,
                    response(
                        &request.request_id,
                        ResponseResult::Error {
                            error: ProtocolError::new(ErrorCode::InvalidRequest, None, None),
                        },
                    ),
                )?;
                return Err(refusal);
            }
        };
        self.record_for(&authorization, now_ms, AuditDecision::AuthorizationConsumed)?;
        send(
            channel,
            response(
                &request.request_id,
                ResponseResult::LaunchAuthorization {
                    authorization: authorization.clone(),
                },
            ),
        )?;

        self.acknowledge(channel, &authorization, 0, now_ms, verify_signature)?;
        let broker_head = self.acknowledge(channel, &authorization, 1, now_ms, verify_signature)?;
        Ok((authorization, broker_head))
    }

    /// Stores one exact signed receipt and acknowledges it after it is durable.
    fn acknowledge<F>(
        &self,
        channel: &SeqpacketChannel,
        authorization: &LaunchAuthorization,
        expected_sequence: u64,
        now_ms: u64,
        verify_signature: &mut F,
    ) -> Result<ReceiptHead, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let packet = receive(channel)?;
        let LauncherPacket::SignedReceipt(receipt) = &packet.packet else {
            return Err(BrokerError::ReceiptUnauthorized);
        };
        if receipt.payload.sequence != expected_sequence {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        // The exact bytes from the wire are what gets stored: nothing here
        // reserializes the supervisor's signed envelope.
        let acknowledgement =
            match self
                .receipts
                .append(authorization, &packet.bytes, &mut *verify_signature)
            {
                Ok(acknowledgement) => acknowledgement,
                Err(BrokerError::Storage(error)) => return Err(BrokerError::Storage(error)),
                Err(refusal) => {
                    self.record_for(
                        authorization,
                        now_ms,
                        AuditDecision::ReceiptRefused {
                            error: ErrorCode::ReceiptChainInvalid,
                        },
                    )?;
                    return Err(refusal);
                }
            };
        self.record_for(
            authorization,
            now_ms,
            AuditDecision::ReceiptStored {
                sequence: acknowledgement.sequence,
            },
        )?;
        let head = ReceiptHead {
            sequence: acknowledgement.sequence,
            digest: acknowledgement.receipt_digest.clone(),
        };
        send(channel, acknowledgement.canonical_bytes())?;
        Ok(head)
    }

    /// Records one decision the broker already made about an authorization.
    fn record_for(
        &self,
        authorization: &LaunchAuthorization,
        at_ms: u64,
        decision: AuditDecision,
    ) -> Result<(), BrokerError> {
        self.record(
            &authorization.session_id,
            &authorization.run_id,
            &authorization.authorization_id,
            Some(authorization.identity_slot),
            at_ms,
            decision,
        )
    }

    /// Records one normalized decision, including for a launch that never
    /// reached an authorization and so has no assigned slot.
    fn record(
        &self,
        session_id: &str,
        run_id: &str,
        authorization_id: &str,
        identity_slot: Option<u32>,
        at_ms: u64,
        decision: AuditDecision,
    ) -> Result<(), BrokerError> {
        self.audit.record(&AuditEntry {
            at_ms,
            session_id: session_id.to_owned(),
            run_id: run_id.to_owned(),
            authorization_id: authorization_id.to_owned(),
            identity_slot: identity_slot.unwrap_or_default(),
            decision,
        })
    }

    fn accept(&self) -> Result<SeqpacketChannel, BrokerError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.listener
            .accept(
                self.supervisor.clone(),
                Box::new(move |accepted| {
                    let _delivered = sender.send(accepted);
                }),
            )
            .map_err(BrokerError::Transport)?;
        settle(&receiver)
    }
}

/// Builds one correlated response for the supervisor.
fn response(request_id: &str, result: ResponseResult) -> Vec<u8> {
    ProtocolResponse {
        schema: RESPONSE_SCHEMA.to_owned(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.to_owned(),
        result,
    }
    .canonical_bytes()
}

/// Receives one authenticated packet, or fails the transaction.
fn receive(channel: &SeqpacketChannel) -> Result<AuthenticatedPacket, BrokerError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    channel
        .receive(Box::new(move |received| {
            let _delivered = sender.send(received);
        }))
        .map_err(BrokerError::Transport)?;
    settle(&receiver)
}

/// Sends one exact packet, or fails the transaction.
fn send(channel: &SeqpacketChannel, bytes: Vec<u8>) -> Result<(), BrokerError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    channel
        .send(
            bytes,
            Box::new(move |sent| {
                let _delivered = sender.send(sent);
            }),
        )
        .map_err(BrokerError::Transport)?;
    settle(&receiver)
}

/// Waits for one transport completion inside the worker's bounded deadline.
fn settle<T>(receiver: &mpsc::Receiver<Result<T, TransportError>>) -> Result<T, BrokerError> {
    match receiver.recv_timeout(STEP_TIMEOUT) {
        Ok(result) => result.map_err(BrokerError::Transport),
        Err(_) => Err(BrokerError::Transport(TransportError::Closed)),
    }
}
