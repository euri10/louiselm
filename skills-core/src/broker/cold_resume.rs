//! Operator-only, single-use reconstruction of a disposed Session.

use super::ApprovedCommands;
mod budget;
use super::{
    BrokerError, BrokerService, GrantRequest, lifecycle::LifecycleCaller, read_record, record_name,
    sync_directory, write_new_record,
};
use crate::{launch::LaunchRequest, launch_protocol::RetentionEvidence};
use budget::remaining_commands;
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[path = "cold_resume/restore.rs"]
mod restore;
pub use restore::ColdLoadOutcome;
#[path = "cold_resume/installed.rs"]
mod installed;

/// Why the original command permission is absent from a reconstructed Session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WithheldCommand {
    /// No command effect was approved for the source.
    NotApproved,
    /// The original absolute permission expiry passed.
    Expired,
    /// All finite uses were spent or reserved.
    Exhausted,
    /// The bounded permission's complete accounting could not be established.
    AccountingUnavailable,
}

/// Immutable, single-use allocation, persisted before fresh launch authorization.
/// The target initially has no command owner; successful restore/load finalization
/// may enable only these remaining permissions. This is not a running-state claim.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColdResumeAllocation {
    /// Authenticated retained point; paths and live grants never transfer.
    pub source: RetentionEvidence,
    /// Distinct launch selected by the authenticated operator.
    pub target: LaunchRequest,
    /// Original controller identity.
    pub controller_uid: u32,
    /// Unchanged original launch/retention deadline, whichever is earlier.
    pub expires_at_ms: u64,
    /// Unchanged recovery requirement.
    pub require_cold_recovery: bool,
    /// Unchanged broker-loss grace.
    pub broker_loss_grace_ms: u32,
    /// Remaining exact approval, never a copy of spent live grants.
    pub commands: Option<ApprovedCommands>,
    /// Stable reason when the original command cannot continue.
    pub withheld_command: Option<WithheldCommand>,
}

impl ColdResumeAllocation {
    fn grant(&self) -> GrantRequest {
        GrantRequest {
            require_cold_recovery: self.require_cold_recovery,
            request: self.target.clone(),
            controller_uid: self.controller_uid,
            expires_at_ms: self.expires_at_ms,
            broker_loss_grace_ms: self.broker_loss_grace_ms,
            commands: self.commands.clone(),
        }
    }
}

impl BrokerService {
    /// Allocates the source exactly once and authorizes its distinct replacement.
    /// Caller must be the trusted authenticated operator, never an Agent role.
    /// Blocking durable I/O belongs on the broker worker. Identical retries keep
    /// the original target and allowance; a different target cannot spend them.
    /// # Errors
    /// Refuses live/unsettled sources, invalid evidence, expiry, changed bindings,
    /// competing reconstruction, exhausted identity pool or uncertain persistence.
    pub fn authorize_cold_resume<F>(
        &self,
        source_id: &str,
        target: &LaunchRequest,
        caller: &LifecycleCaller,
        now_ms: u64,
        mut verify: F,
    ) -> Result<ColdResumeAllocation, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let original = self
            .authorizations()
            .consumed_for_session(source_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        check_operator(caller, original.controller_uid)?;
        let source = self.cold_source(source_id, now_ms, &mut verify)?;
        let expires_at_ms = original.expires_at_ms.min(source.request.expires_at_ms);
        if now_ms >= expires_at_ms {
            return Err(BrokerError::Expired);
        }
        crate::launch_protocol::validate_reconstruction(&source.launch, target)?;
        let path = self.cold_path(source_id, "allocation")?;
        let allocation = if let Some(prior) = read_record::<ColdResumeAllocation>(&path)? {
            if prior.source != source || prior.target != *target {
                return Err(BrokerError::RequestMismatch);
            }
            prior
        } else {
            if self.authorizations().has_session(&target.session_id)? {
                return Err(BrokerError::DuplicateAuthorization);
            }
            let audit = self.audit();
            let commands = remaining_commands(&original, audit.as_deref().ok(), now_ms);
            let withheld_command = match (&original.commands, &commands) {
                (None, _) => Some(WithheldCommand::NotApproved),
                (Some(approved), _) if approved.expires_at_ms <= now_ms => {
                    Some(WithheldCommand::Expired)
                }
                (_, Some(remaining)) if remaining.uses == Some(0) => {
                    Some(WithheldCommand::Exhausted)
                }
                (_, None) => Some(WithheldCommand::AccountingUnavailable),
                _ => None,
            };
            let allocation = ColdResumeAllocation {
                source,
                target: target.clone(),
                controller_uid: original.controller_uid,
                expires_at_ms,
                require_cold_recovery: original.require_cold_recovery,
                broker_loss_grace_ms: original.broker_loss_grace_ms,
                commands: commands.filter(|c| c.uses != Some(0)),
                withheld_command,
            };
            // create_new is the cross-thread/process single winner. A loser retries
            // reading the exact durable allocation, never calculates another one.
            write_new_record(&path, &allocation)?;
            allocation
        };
        durable_again(&path)?;
        let target_path = self.cold_path(&target.session_id, "target")?;
        identical_record(&target_path, &source_id.to_owned())?;
        // Replayed authorization never replenishes a consumed target. A crash
        // between pending removal and consumed publication remains a refusal.
        self.authorizations()
            .authorize_cold_target(&allocation.grant(), now_ms)?;
        Ok(allocation)
    }

    pub(super) fn cold_target(
        &self,
        session_id: &str,
    ) -> Result<Option<ColdResumeAllocation>, BrokerError> {
        let path = self
            .authorizations()
            .root
            .join("cold-resume")
            .join(format!("target-{}", record_name(session_id)?));
        let Some(source_id) = read_record::<String>(&path)? else {
            return Ok(None);
        };
        let allocation: ColdResumeAllocation =
            read_record(&self.cold_path(&source_id, "allocation")?)?
                .ok_or(BrokerError::InvalidGrant)?;
        if allocation.source.launch.session_id != source_id
            || allocation.target.session_id != session_id
        {
            return Err(BrokerError::RequestMismatch);
        }
        Ok(Some(allocation))
    }

    fn cold_source<F>(
        &self,
        source_id: &str,
        now_ms: u64,
        verify: &mut F,
    ) -> Result<RetentionEvidence, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        use crate::launch_receipt::{ReceiptAuthority, ReceiptCause, ReceiptOutcome, SessionState};
        let original = self
            .authorizations()
            .consumed_for_session(source_id)?
            .ok_or(BrokerError::UnknownAuthorization)?;
        let chain = self.verified_history(&original.launch_authorization(), verify)?;
        let terminal = chain.last().ok_or(BrokerError::ReceiptUnauthorized)?;
        if terminal.payload.resulting_state != SessionState::Terminal
            || !matches!(
                terminal.payload.outcome,
                ReceiptOutcome::Disposal {
                    authority: ReceiptAuthority::Cause {
                        cause: ReceiptCause::ControllerLost
                    }
                }
            )
        {
            return Err(BrokerError::ReceiptUnauthorized);
        }
        self.recoverable_loss(source_id, now_ms)?;
        self.registered_recovery(source_id, now_ms)
    }

    fn cold_path(&self, source_id: &str, kind: &str) -> Result<PathBuf, BrokerError> {
        let name = record_name(source_id)?;
        let directory = self.authorizations().root.join("cold-resume");
        fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
        sync_directory(&self.authorizations().root)?;
        Ok(directory.join(format!("{kind}-{name}")))
    }
}

fn check_operator(caller: &LifecycleCaller, uid: u32) -> Result<(), BrokerError> {
    if matches!(caller, LifecycleCaller::Operator { uid: actual } if *actual == uid) {
        Ok(())
    } else {
        Err(BrokerError::ControllerMismatch)
    }
}

fn durable_again(path: &std::path::Path) -> Result<(), BrokerError> {
    fs::File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(BrokerError::Storage)?;
    sync_directory(path.parent().ok_or(BrokerError::InvalidGrant)?)
}

fn identical_record<T: Serialize + for<'de> Deserialize<'de> + PartialEq>(
    path: &std::path::Path,
    value: &T,
) -> Result<(), BrokerError> {
    if let Some(prior) = read_record::<T>(path)? {
        if prior != *value {
            return Err(BrokerError::RequestMismatch);
        }
        durable_again(path)
    } else {
        write_new_record(path, value)
    }
}
