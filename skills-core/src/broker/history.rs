//! A failed Session history stays unusable independently of other Sessions.

use super::{
    BrokerError, BrokerService, read_record, record_name, sync_directory, write_new_record,
};
use crate::{
    broker::attention::{AttentionCondition, AttentionReason, AttentionSubject, ProjectionChange},
    launch_protocol::{ErrorCode, LaunchAuthorization, ProtocolError},
    launch_receipt::SignedReceipt,
};

fn refusal() -> BrokerError {
    ProtocolError::new(ErrorCode::ReceiptChainInvalid, None, None).into()
}

impl BrokerService {
    pub(super) fn require_trusted_history<F>(
        &self,
        session: &mut super::BrokerSession,
        verify: &mut F,
    ) -> Result<(), BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        let result = self
            .verified_history(session.authorization(), verify)
            .map(|_| ());
        if result.is_err() {
            session.commands = None;
            session.recovery_admitted_until = None;
            session.close();
        }
        result
    }

    pub(super) fn check_history(&self, session_id: &str) -> Result<(), BrokerError> {
        self.receipts().check_authority()?;
        if self
            .history_result(session_id, self.receipts().key_revocation(session_id))?
            .is_some()
        {
            return match self.history_result::<()>(session_id, Err(refusal())) {
                Err(BrokerError::Policy(error)) if error.code == ErrorCode::ReceiptChainInvalid => {
                    Err(ProtocolError::new(ErrorCode::SigningKeyRevoked, None, None).into())
                }
                result => result,
            };
        }
        let path = self
            .authorizations()
            .root
            .join("history-failures")
            .join(record_name(session_id)?);
        if let Some(condition) = read_record::<AttentionCondition>(&path)? {
            std::fs::File::open(&path)
                .and_then(|file| file.sync_all())
                .map_err(BrokerError::Storage)?;
            sync_directory(path.parent().ok_or(BrokerError::InvalidGrant)?)?;
            self.project_history_failure(session_id, condition)?;
            return Err(refusal());
        }
        Ok(())
    }

    fn project_history_failure(
        &self,
        session_id: &str,
        condition: AttentionCondition,
    ) -> Result<(), BrokerError> {
        if condition.subject != AttentionSubject::Session(session_id.into())
            || condition.reason != AttentionReason::SessionFailed
        {
            return Err(super::corrupt("invalid history failure subject"));
        }
        self.lifecycle.quarantine(session_id)?;
        self.attention.enqueue(
            &format!("history-{}", condition.operation_id),
            ProjectionChange::Upsert(condition),
        )?;
        Ok(())
    }

    pub(super) fn history_result<T>(
        &self,
        session_id: &str,
        result: Result<T, BrokerError>,
    ) -> Result<T, BrokerError> {
        match result {
            Ok(value) => Ok(value),
            Err(BrokerError::InstallationAuthority(error)) => {
                Err(BrokerError::InstallationAuthority(error))
            }
            Err(_) => {
                // Shared authority may have changed during signature verification.
                // Never turn that into a Session-local trust decision.
                self.receipts().check_authority()?;
                let directory = self.authorizations().root.join("history-failures");
                std::fs::create_dir_all(&directory).map_err(BrokerError::Storage)?;
                sync_directory(&self.authorizations().root)?;
                let path = directory.join(record_name(session_id)?);
                let condition = if let Some(condition) = read_record::<AttentionCondition>(&path)? {
                    condition
                } else {
                    let digest = crate::Digest::of(format!("history-{session_id}").as_bytes());
                    let hex = digest.hex();
                    let condition = AttentionCondition {
                        subject: AttentionSubject::Session(session_id.into()),
                        operation_id: format!(
                            "{}-{}-{}-{}-{}",
                            &hex[..8],
                            &hex[8..12],
                            &hex[12..16],
                            &hex[16..20],
                            &hex[20..32]
                        ),
                        created_at_ms: super::now_ms()?,
                        reason: AttentionReason::SessionFailed,
                    };
                    // No replacement: concurrent readers converge on the first record.
                    match write_new_record(&path, &condition) {
                        Ok(()) => condition,
                        Err(BrokerError::DuplicateAuthorization) => read_record(&path)?
                            .ok_or_else(|| super::corrupt("missing history failure"))?,
                        Err(error) => return Err(error),
                    }
                };
                std::fs::File::open(&path)
                    .and_then(|file| file.sync_all())
                    .map_err(BrokerError::Storage)?;
                sync_directory(&directory)?;
                self.project_history_failure(session_id, condition)?;
                Err(refusal())
            }
        }
    }

    pub(super) fn verified_history<F>(
        &self,
        authorization: &LaunchAuthorization,
        verify: &mut F,
    ) -> Result<Vec<SignedReceipt>, BrokerError>
    where
        F: FnMut(&str, &[u8], &str) -> bool,
    {
        self.check_history(&authorization.session_id)?;
        let result = self
            .receipts()
            .verified_chain(authorization, verify)
            .and_then(|chain| {
                if chain.is_empty() {
                    Err(BrokerError::ReceiptUnauthorized)
                } else {
                    Ok(chain)
                }
            });
        self.history_result(&authorization.session_id, result)
    }
}
