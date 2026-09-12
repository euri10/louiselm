//! Explicit preview then commit by a trusted operator process, never an Agent.

use super::{ApplicationResult, ChangePreview, Changes, Destination, StepEvent, transfer};
use crate::{
    Digest,
    broker::promotion::{OperatorMessage, PromotionRequest, PromotionStatus, Reply},
    workspace::WorkspaceError,
};
use std::{
    fs,
    net::Shutdown,
    os::unix::{fs::MetadataExt, net::UnixStream},
    path::{Path, PathBuf},
    time::Instant,
};

/// A preview bound to one pinned checkout, exact bytes and authenticated broker.
///
/// Preparation changes no checkout bytes. The caller must exclude editor/build
/// writers through commit; the advisory lock only serializes promotion clients.
/// Dropping cancels the connection. An interrupted commit is never auto-retried;
/// inspect the local journal and broker status before preparing new work.
pub struct PromotionClient {
    stream: UnixStream,
    request: PromotionRequest,
    destination: Destination,
    changes: Changes,
    journal: PathBuf,
    deadline: Instant,
}

impl PromotionClient {
    /// Receives and verifies exact bytes from the pinned dedicated broker UID.
    /// `journal_parent` must be outside the checkout, private to this operator.
    /// The caller selects the broker UID from trusted installed configuration.
    /// All filesystem and stream I/O is blocking; call on an owned worker.
    /// # Errors
    /// Refuses foreign peers/destinations, malformed or changed bytes, baseline
    /// conflicts, unsafe journal storage, spent requests, expiry and I/O failure.
    pub fn prepare(
        mut stream: UnixStream,
        broker_uid: u32,
        request: PromotionRequest,
        checkout: &Path,
        journal_parent: &Path,
    ) -> Result<Self, WorkspaceError> {
        request
            .validate()
            .map_err(|_| WorkspaceError::Invalid("invalid promotion request"))?;
        transfer::configure_stream(&stream)?;
        if rustix::net::sockopt::socket_peercred(&stream)
            .map_err(std::io::Error::from)?
            .uid
            .as_raw()
            != broker_uid
            || broker_uid == 0
            || broker_uid == rustix::process::geteuid().as_raw()
        {
            return Err(WorkspaceError::Invalid(
                "promotion broker identity mismatch",
            ));
        }
        let destination = Destination::open(checkout)?;
        if destination.identity != request.destination {
            return Err(WorkspaceError::Invalid(
                "promotion destination identity changed",
            ));
        }
        let journal_parent = fs::canonicalize(journal_parent)?;
        if journal_parent.starts_with(fs::canonicalize(checkout)?) {
            return Err(WorkspaceError::Invalid(
                "promotion journal must be outside checkout",
            ));
        }
        let metadata = super::super::filesystem::open_directory(&journal_parent)?.metadata()?;
        if metadata.uid() != request.destination.uid || metadata.mode() & 0o777 != 0o700 {
            return Err(WorkspaceError::Invalid(
                "promotion journal parent must be private and operator-owned",
            ));
        }
        let journal = journal_parent.join(&request.request_id);
        if journal.try_exists()? {
            return Err(WorkspaceError::Invalid(
                "promotion already attempted; inspect journal and broker status",
            ));
        }
        let deadline = Instant::now()
            .checked_add(transfer::remaining(request.expires_at_ms)?)
            .ok_or(WorkspaceError::Invalid("promotion deadline unavailable"))?;
        transfer::send(&mut stream, &request)?;
        if !matches!(transfer::receive::<Reply>(&mut stream)?, Reply::Prepared) {
            return Err(WorkspaceError::Invalid(
                "promotion already attempted; inspect broker status",
            ));
        }
        let changes = transfer::receive_changes(&mut stream, &request.job)?;
        destination.check(&changes.baseline)?;
        Ok(Self {
            stream,
            request,
            destination,
            changes,
            journal,
            deadline,
        })
    }

    /// Exact paths shown before the operator chooses to commit this preview.
    #[must_use]
    pub const fn preview(&self) -> &ChangePreview {
        &self.changes.preview
    }

    /// Applies the approved preview, requiring a fresh broker permit for every effect.
    /// This consumes the handle even on failure; no automatic merge or replay occurs.
    /// Caller must keep all other checkout writers stopped throughout this operation.
    /// # Errors
    /// Refuses stale bytes, expiry or denied permits. Filesystem/persistence or
    /// connection failures may follow completed effects; inspect the retained journal.
    pub fn commit(mut self) -> Result<ApplicationResult, WorkspaceError> {
        self.destination.check(&self.changes.baseline)?;
        transfer::remaining(self.request.expires_at_ms)?;
        if Instant::now() >= self.deadline {
            return Err(WorkspaceError::Invalid("promotion expired"));
        }
        let digest = Digest::of(&serde_json::to_vec(&self.request)?).to_string();
        transfer::send(
            &mut self.stream,
            &OperatorMessage::Commit {
                request_digest: digest,
            },
        )?;
        let result = self
            .destination
            .apply(&self.changes, &self.journal, |event| {
                let (index, begin) = match event {
                    StepEvent::Begin { index } => (index, true),
                    StepEvent::Done { index } => (index, false),
                };
                transfer::send(&mut self.stream, &OperatorMessage::Step { event })?;
                let reply = transfer::receive::<Reply>(&mut self.stream)?;
                let valid = match reply {
                    Reply::Granted { index: n } => begin && n == index,
                    Reply::Recorded { index: n } => !begin && n == index,
                    _ => false,
                };
                if !valid {
                    return Err(WorkspaceError::Invalid(
                        "promotion effect acknowledgement mismatch",
                    ));
                }
                if begin {
                    transfer::remaining(self.request.expires_at_ms)?;
                    if Instant::now() >= self.deadline {
                        return Err(WorkspaceError::Invalid("promotion expired"));
                    }
                }
                Ok(())
            })?;
        transfer::send(
            &mut self.stream,
            &OperatorMessage::Complete {
                result: result.clone(),
            },
        )?;
        let Reply::Finished {
            status: PromotionStatus::Completed { result: observed },
        } = transfer::receive(&mut self.stream)?
        else {
            return Err(WorkspaceError::Invalid(
                "promotion completion is not durable",
            ));
        };
        if observed != result {
            return Err(WorkspaceError::Invalid("promotion completion mismatch"));
        }
        Ok(result)
    }
}

impl Drop for PromotionClient {
    fn drop(&mut self) {
        // Closing a control stream revokes future effects. Shutdown failure cannot
        // make an unknown write completed or refund its durable broker permit.
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}
