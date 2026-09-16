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

use std::{
    path::Path,
    sync::{Arc, mpsc},
    time::{Duration, Instant},
};

#[path = "command_service.rs"]
mod command_service;

#[path = "lifecycle_service.rs"]
mod lifecycle_service;

#[path = "reconnect.rs"]
mod reconnect;

#[path = "controller_loss.rs"]
mod controller_loss;

#[path = "conformance_transfer.rs"]
mod conformance_transfer;

use crate::{
    broker::{AuditDecision, AuditEntry, AuditLog, AuthorizationStore, BrokerError, ReceiptStore},
    launch::PROTOCOL_VERSION,
    launch_protocol::{
        ErrorCode, LaunchAuthorization, ProtocolError, ProtocolMessage, ProtocolResponse,
        RESPONSE_SCHEMA, ReceiptAcknowledgement, ResponseResult,
    },
    launch_receipt::{LaunchEvidence, ReceiptHead, ReceiptOutcome, SessionState, StartEvidence},
    launch_supervisor::CapabilityBinding,
    launch_transport::{
        AuthenticatedPacket, CredentialPin, LauncherPacket, SeqpacketChannel, SeqpacketListener,
        TransportError,
    },
};

/// Longest a broker worker waits for a handshake or operation response.
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
    pub(in crate::broker) posture_evidence: super::posture::LaunchPostureEvidence,
    require_cold_recovery: bool,
    pub(in crate::broker) recovery_admitted_until: Option<Instant>,
    authorization: LaunchAuthorization,
    launch_head: ReceiptHead,
    channel: SeqpacketChannel,
    pub(in crate::broker) commands: Option<super::commands::CommandAuthority>,
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
/// Evidence contains only validated measurements and identifiers, never prompts,
/// environments or command payloads. Durable state is not current liveness or
/// proof that a supervisor received its final acknowledgement.
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
    /// Sequence-zero prerequisites fixed before restricted initialization.
    pub launch_evidence: Option<LaunchEvidence>,
    /// Initial signed Agent/isolation proof; never a current liveness claim.
    pub start_evidence: Option<StartEvidence>,
    /// What this caller can establish about the completed launch/channel.
    pub launch: LaunchObservation,
}

/// Initial launch acknowledgement and locally owned transport observation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchObservation {
    /// Only stored evidence is available; no completed handshake is inferred.
    DurableOnly,
    /// The owning worker sent the final durable ACK and returned a Session.
    Acknowledged {
        /// Whether that owned channel is locally open, not a peer liveness proof.
        channel_open: bool,
    },
}

/// The Control broker's local-only rendezvous service.
pub struct BrokerService {
    listener: SeqpacketListener,
    authorizations: AuthorizationStore,
    receipts: ReceiptStore,
    audit: Arc<AuditLog>,
    supervisor: CredentialPin,
    pub(super) lifecycle: super::lifecycle::LifecycleStore,
    pub(super) attention: super::attention::Outbox,
    pub(super) skill_requests: super::skill_requests::SkillRequests,
    pub(super) verification_inputs: std::path::PathBuf,
}

impl BrokerService {
    pub(in crate::broker) fn command_audit(&self) -> Arc<AuditLog> {
        Arc::clone(&self.audit)
    }
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
        Self::over(
            listener,
            socket_path,
            authorizations,
            receipts,
            audit,
            supervisor,
        )
    }

    /// Serves an already-listening rendezvous this process did not bind.
    ///
    /// `socket_path` still names where that rendezvous lives, because sibling
    /// state is resolved relative to it; it is not opened or replaced here.
    ///
    /// # Errors
    /// Returns [`BrokerError::Storage`] for unavailable durable state and
    /// [`BrokerError::InvalidGrant`] when `socket_path` has no parent.
    pub fn over(
        listener: SeqpacketListener,
        socket_path: &Path,
        authorizations: AuthorizationStore,
        receipts: ReceiptStore,
        audit: AuditLog,
        supervisor: CredentialPin,
    ) -> Result<Self, BrokerError> {
        let lifecycle =
            super::lifecycle::LifecycleStore::open(&authorizations.root.join("lifecycle"))?;
        let attention =
            super::attention::Outbox::open(&authorizations.root.join("attention-outbox"))?;
        let skill_requests = super::skill_requests::SkillRequests::open(
            &authorizations.root.join("skill-requests"),
        )?;
        skill_requests.reconcile(&attention)?;
        Ok(Self {
            listener,
            authorizations,
            receipts,
            audit: Arc::new(audit),
            supervisor,
            lifecycle,
            attention,
            skill_requests,
            verification_inputs: socket_path
                .parent()
                .ok_or(BrokerError::InvalidGrant)?
                .join("verification-inputs"),
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
    /// Returns a canonical receipt-chain refusal for quarantined history,
    /// installed shared-authority failure, or unavailable durable reporting.
    /// Installed stores reverify history before returning measurements.
    pub fn inspect(&self, session_id: &str) -> Result<Option<SessionInspection>, BrokerError> {
        let Some(authorization) = self.authorizations.consumed_for_session(session_id)? else {
            return Ok(None);
        };
        self.check_history(session_id)?;
        let chain = self.history_result(
            session_id,
            self.receipts
                .inspection_chain(&authorization.launch_authorization()),
        )?;
        let head = chain.last();
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
            state: head.map_or(SessionState::Starting, |receipt| {
                receipt.payload.resulting_state
            }),
            broker_head: head.map(|receipt| ReceiptHead {
                sequence: receipt.payload.sequence,
                digest: receipt.digest().to_string(),
            }),
            last_failure,
            launch_evidence: chain
                .first()
                .and_then(|receipt| match &receipt.payload.outcome {
                    ReceiptOutcome::Launch { evidence, .. } => Some((**evidence).clone()),
                    _ => None,
                }),
            start_evidence: chain
                .get(1)
                .and_then(|receipt| match &receipt.payload.outcome {
                    ReceiptOutcome::Start { evidence, .. } => Some(evidence.clone()),
                    _ => None,
                }),
            launch: LaunchObservation::DurableOnly,
        }))
    }

    /// Adds the completed handshake and local channel observation from its owner.
    /// Neither an open channel nor a stored Running receipt proves current liveness.
    ///
    /// # Errors
    /// Refuses missing/foreign authorization or unreadable durable evidence.
    pub fn inspect_active(
        &self,
        session: &BrokerSession,
    ) -> Result<SessionInspection, BrokerError> {
        let authorization = self
            .authorizations
            .consumed_for_session(&session.authorization.session_id)?
            .filter(|pending| pending.authorization_id == session.authorization.authorization_id)
            .ok_or(BrokerError::InvalidGrant)?;
        let mut inspection = self
            .inspect(&authorization.session_id)?
            .ok_or(BrokerError::InvalidGrant)?;
        inspection.launch = LaunchObservation::Acknowledged {
            channel_open: !session.channel.is_closed(),
        };
        Ok(inspection)
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
        let clock = Instant::now();
        let channel = self.accept()?;
        let result = (|| {
            let packet = receive(&channel)?;
            let LauncherPacket::Request(ProtocolMessage::LaunchAuthorization(request)) =
                packet.packet
            else {
                // Nothing correlates a response to a packet that is not a launch
                // request, so the transaction ends without one.
                return Err(BrokerError::InvalidGrant);
            };
            let now_ms = now_ms
                .saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX));
            self.launch_on(&channel, &request, now_ms, &mut verify_signature)
        })();
        if result.is_err() {
            channel.close();
        }
        result
    }

    /// Accepts one supervisor and routes it by the connection's first packet.
    ///
    /// A running broker cannot know in advance whether the peer dialling its
    /// rendezvous is starting a new launch or reattaching after a restart, so
    /// the first packet decides. Anything else is refused without a response,
    /// because nothing correlates one to an unrecognised packet.
    ///
    /// # Errors
    /// Returns the same failures as [`Self::serve_launch`] and
    /// [`Self::serve_reconnect`], according to which the peer asked for.
    pub fn serve_connection<F>(&self, now_ms: u64, verify: F) -> Result<BrokerSession, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = Instant::now();
        let channel = self.accept()?;
        let now_ms =
            now_ms.saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX));
        self.serve_accepted(channel, now_ms, verify)
    }

    /// Runs the launch or reconnect handshake on an already accepted channel.
    ///
    /// The daemon accepts independently, then gives this blocking transaction
    /// its own worker. The channel must match this service's supervisor pin;
    /// every packet remains authenticated and failures close only this channel.
    ///
    /// # Errors
    /// Returns peer, protocol, authorization, verification or durability failure.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "The worker transfers its accepted channel; only the returned Session retains ownership after success."
    )]
    pub fn serve_accepted<F>(
        &self,
        channel: SeqpacketChannel,
        now_ms: u64,
        mut verify: F,
    ) -> Result<BrokerSession, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = Instant::now();
        let result = (|| {
            if !self
                .supervisor
                .matches(channel.peer_credentials())
                .map_err(BrokerError::Transport)?
            {
                return Err(BrokerError::InvalidGrant);
            }
            let packet = receive(&channel)?;
            let now_ms = now_ms
                .saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX));
            match packet.packet {
                LauncherPacket::Request(ProtocolMessage::LaunchAuthorization(request)) => {
                    self.launch_on(&channel, &request, now_ms, &mut verify)
                }
                LauncherPacket::Request(ProtocolMessage::BrokerReconnect(request)) => {
                    self.reconnect_on(&channel, &request, now_ms, &mut verify)
                }
                _ => Err(BrokerError::InvalidGrant),
            }
        })();
        if result.is_err() {
            channel.close();
        }
        result
    }

    /// Waits for one authenticated connection without waiting for its first packet.
    ///
    /// An idle rendezvous has no deadline. Closing the listener interrupts this
    /// wait; connection handshakes retain their own bounded packet deadlines.
    /// Run on the daemon accept worker, then dispatch [`Self::serve_accepted`].
    ///
    /// # Errors
    /// Returns listener closure, peer authentication or transport setup failure.
    pub fn accept_connection(&self) -> Result<SeqpacketChannel, BrokerError> {
        self.accept_pending()?
            .recv()
            .map_err(|_| BrokerError::Transport(TransportError::Closed))?
            .map_err(BrokerError::Transport)
    }

    /// Closes the rendezvous listener without unlinking its bound path.
    /// Provisioners must satisfy the installed broker's `private_directory`
    /// invariant, rechecked on every start. Returned Session owners remain
    /// usable and must be closed separately.
    pub fn close(&self) {
        self.listener.close();
    }

    #[expect(
        clippy::too_many_lines,
        reason = "One ordered transaction consumes approval, stores both receipts and binds policy before the final ACK."
    )]
    fn launch_on<F>(
        &self,
        channel: &SeqpacketChannel,
        request: &crate::launch::LaunchRequest,
        now_ms: u64,
        verify_signature: &mut F,
    ) -> Result<BrokerSession, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let clock = Instant::now();
        let consumed_at_ms =
            now_ms.saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX));
        let authorization = match self
            .authorizations
            .consume_for_launcher(request, consumed_at_ms)
        {
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

        let pending = self
            .authorizations
            .consumed_for_session(&authorization.session_id)?
            .filter(|pending| pending.authorization_id == authorization.authorization_id)
            .ok_or(BrokerError::InvalidGrant)?;
        let cold_target = self.cold_target(&authorization.session_id)?.is_some();
        let approved = pending
            .commands
            .as_ref()
            .filter(|_| !cold_target)
            .map(|commands| {
                let policy_at_ms = now_ms
                    .saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX));
                commands.policy(&authorization.authorization_id, policy_at_ms)
            })
            .transpose()?;
        let (ack, _) =
            self.store_next_receipt(channel, &authorization, 0, now_ms, clock, verify_signature)?;
        check_conformance_waiver(&authorization, now_ms, clock)?;
        send(channel, ack.canonical_bytes())?;
        let (ack, evidence) =
            self.store_next_receipt(channel, &authorization, 1, now_ms, clock, verify_signature)?;
        let evidence = evidence.ok_or(BrokerError::ReceiptUnauthorized)?;
        let commands = approved
            .map(|policy| {
                super::commands::CommandAuthority::new(
                    CapabilityBinding {
                        session_id: authorization.session_id.clone(),
                        run_id: authorization.run_id.clone(),
                        channel_id: "agent-capability".to_owned(),
                        envelope_revision: authorization.envelope_revision,
                        identity_slot: authorization.identity_slot,
                        assigned_uid: evidence.assigned_uid,
                        assigned_gid: evidence.assigned_gid,
                        agent_pid: evidence.agent_pid,
                    },
                    policy,
                    Arc::clone(&self.audit),
                )
                .map_err(|error| match error {
                    super::delegation::DelegationError::Audit(error) => error,
                    _ => BrokerError::InvalidGrant,
                })
            })
            .transpose()?;
        let broker_head = ReceiptHead {
            sequence: ack.sequence,
            digest: ack.receipt_digest.clone(),
        };
        let posture_evidence = self.retain_launch_posture(&authorization, verify_signature)?;
        check_conformance_waiver(&authorization, now_ms, clock)?;
        send(channel, ack.canonical_bytes())?;
        Ok(BrokerSession {
            posture_evidence,
            require_cold_recovery: pending.require_cold_recovery,
            recovery_admitted_until: None,
            authorization,
            launch_head: broker_head,
            channel: channel.clone(),
            commands,
        })
    }

    /// Stores exact signed bytes; the caller sends the ACK after policy setup.
    fn store_next_receipt<F>(
        &self,
        channel: &SeqpacketChannel,
        authorization: &LaunchAuthorization,
        expected_sequence: u64,
        now_ms: u64,
        clock: Instant,
        verify_signature: &mut F,
    ) -> Result<(ReceiptAcknowledgement, Option<StartEvidence>), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let deadline = Instant::now() + STEP_TIMEOUT;
        let packet = receive(channel)?;
        let LauncherPacket::SignedReceipt(receipt) = &packet.packet else {
            return Err(BrokerError::ReceiptUnauthorized);
        };
        if receipt.payload.sequence != expected_sequence {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        let evidence = match &receipt.payload.outcome {
            ReceiptOutcome::Launch { .. } if expected_sequence == 0 => None,
            ReceiptOutcome::Start { evidence, .. } if expected_sequence == 1 => {
                Some(evidence.clone())
            }
            _ => return Err(BrokerError::ReceiptUnauthorized),
        };
        // The exact bytes from the wire are what gets stored: nothing here
        // reserializes the supervisor's signed envelope.
        let report = conformance_transfer::receive_report(channel, receipt, deadline)?;
        check_conformance_waiver(authorization, now_ms, clock)?;
        let acknowledgement = match self.receipts.append(
            authorization,
            &packet.bytes,
            report.as_deref(),
            &mut *verify_signature,
        ) {
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
        Ok((acknowledgement, evidence))
    }

    /// Original proof observation, not a new check or an operator-view read.
    pub(in crate::broker) fn start_receipt_stored_at(
        &self,
        authorization: &LaunchAuthorization,
    ) -> Result<Option<u64>, BrokerError> {
        Ok(self
            .audit
            .find(|entry| {
                entry.session_id == authorization.session_id
                    && entry.run_id == authorization.run_id
                    && entry.authorization_id == authorization.authorization_id
                    && entry.identity_slot == authorization.identity_slot
                    && entry.decision == (AuditDecision::ReceiptStored { sequence: 1 })
            })?
            .map(|entry| entry.at_ms))
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
        settle(&self.accept_pending()?)
    }

    fn accept_pending(
        &self,
    ) -> Result<mpsc::Receiver<Result<SeqpacketChannel, TransportError>>, BrokerError> {
        let (sender, receiver) = mpsc::sync_channel(1);
        self.listener
            .accept(
                self.supervisor.clone(),
                Box::new(move |accepted| {
                    let _delivered = sender.send(accepted);
                }),
            )
            .map_err(BrokerError::Transport)?;
        Ok(receiver)
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

fn check_conformance_waiver(
    authorization: &LaunchAuthorization,
    now_ms: u64,
    clock: Instant,
) -> Result<(), BrokerError> {
    authorization
        .conformance
        .validate_for(
            &authorization.session_id,
            &authorization.request_digest,
            authorization.controller_uid,
            now_ms.saturating_add(u64::try_from(clock.elapsed().as_millis()).unwrap_or(u64::MAX)),
        )
        .map_err(|_| BrokerError::ReceiptUnauthorized)
}

/// Receives one authenticated packet, or fails the transaction.
pub(super) fn receive(channel: &SeqpacketChannel) -> Result<AuthenticatedPacket, BrokerError> {
    receive_for(channel, STEP_TIMEOUT)
}

/// Verification commands have their own approved, bounded job deadline.
pub(super) fn receive_for(
    channel: &SeqpacketChannel,
    timeout: Duration,
) -> Result<AuthenticatedPacket, BrokerError> {
    receive_with_timeout(channel, Some(timeout))
}

/// An idle Session has no pending operation to time out. Peer/owner closure
/// still cancels the transport receive and wakes this worker immediately.
pub(super) fn receive_next(channel: &SeqpacketChannel) -> Result<AuthenticatedPacket, BrokerError> {
    receive_with_timeout(channel, None)
}

fn receive_with_timeout(
    channel: &SeqpacketChannel,
    timeout: Option<Duration>,
) -> Result<AuthenticatedPacket, BrokerError> {
    let (sender, receiver) = mpsc::sync_channel(1);
    channel
        .receive(Box::new(move |received| {
            let _delivered = sender.send(received);
        }))
        .map_err(BrokerError::Transport)?;
    let packet: AuthenticatedPacket = match timeout {
        Some(timeout) => receiver.recv_timeout(timeout).map_err(|_| ()),
        None => receiver.recv().map_err(|_| ()),
    }
    .map_err(|()| BrokerError::Transport(TransportError::Closed))?
    .map_err(BrokerError::Transport)?;
    if packet.peer_credentials != channel.peer_credentials()
        || packet.message_credentials != packet.peer_credentials
    {
        return Err(BrokerError::InvalidGrant);
    }
    Ok(packet)
}

/// Sends one exact packet, or fails the transaction.
pub(super) fn send(channel: &SeqpacketChannel, bytes: Vec<u8>) -> Result<(), BrokerError> {
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
