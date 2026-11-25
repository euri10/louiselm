//! Bounded, read-only review of the broker's retained exact result.

use super::{BrokerError, BrokerService, PromotionStatus};
use crate::{
    Digest,
    broker::verification::VerificationStatus,
    workspace::{
        self,
        promotion::{ChangePreview, Changes, DestinationIdentity, transfer},
        provenance::OutputProvenanceCode,
    },
};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, path::PathBuf};

/// Which retained version of a changed path a page displays.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SavedReviewSide {
    /// Bytes in the immutable baseline.
    Before,
    /// Bytes in the verified candidate.
    After,
}

/// Position in one exact saved-result revision.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedReviewCursor {
    /// Zero-based changed-path index.
    pub file: usize,
    /// Version to display.
    pub side: SavedReviewSide,
    /// UTF-8 byte offset in that version.
    pub offset: usize,
}

/// Bounded inert text from one changed path.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedReviewFile {
    /// Validated path relative to the retained source tree.
    pub path: String,
    /// `added`, `modified`, or `deleted`.
    pub change: String,
    /// Version represented by this page.
    pub side: SavedReviewSide,
    /// UTF-8 byte offset of the page.
    pub offset: usize,
    /// Inert UTF-8 content; empty for binary data.
    pub text: String,
    /// The retained file is not displayable as UTF-8 text.
    pub binary: bool,
    /// Executable intent of this version.
    pub executable: bool,
}

/// Exact historical result and one bounded page; none of these fields grant action authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SavedReviewPage {
    /// Fixed closed review schema.
    pub schema: String,
    /// Exact saved publication.
    pub result_id: String,
    /// Original Run, which need not still be active.
    pub run_id: String,
    /// Registered Agent bound to the producer's authenticated launch.
    pub agent_id: String,
    /// Operator-selected destination, for a separate scope check.
    pub checkout: PathBuf,
    /// Pinned identity of that destination.
    pub destination: DestinationIdentity,
    /// Exact Run-branch baseline.
    pub base_commit: String,
    /// Stable digest of current saved state and current broker observations.
    pub revision: String,
    /// Exact historical verification record digest.
    pub verification_digest: String,
    /// Exact historical rules digest.
    pub plan_digest: String,
    /// Exact historical verifier inputs digest.
    pub verifier_inputs_digest: String,
    /// Exact candidate inventory digest.
    pub result_digest: String,
    /// Historical number of successfully completed verification commands.
    pub commands_passed: usize,
    /// Current broker evidence state: `matching`, `changed`, `quarantined`, or `unavailable`.
    pub current_verification: String,
    /// Current producer output provenance, with missing evidence represented as unknown.
    pub current_provenance: OutputProvenanceCode,
    /// One retained version of a changed path, if any.
    pub file: Option<SavedReviewFile>,
    /// Next position in this exact revision, or none at the end.
    pub next: Option<SavedReviewCursor>,
}

fn file_page(
    preview: &ChangePreview,
    changes: &Changes,
    before: &BTreeMap<String, workspace::SourceFile>,
    mut position: SavedReviewCursor,
    limit: usize,
) -> Result<(Option<SavedReviewFile>, Option<SavedReviewCursor>), BrokerError> {
    let paths = preview
        .added
        .iter()
        .map(|path| (path, "added"))
        .chain(preview.modified.iter().map(|path| (path, "modified")))
        .chain(preview.deleted.iter().map(|path| (path, "deleted")))
        .collect::<Vec<_>>();
    if position.file > paths.len() || (position.file == paths.len() && position.offset != 0) {
        return Err(BrokerError::InvalidGrant);
    }
    while let Some((path, change)) = paths.get(position.file) {
        let source = match position.side {
            SavedReviewSide::Before => before.get(*path),
            SavedReviewSide::After => changes.files.get(*path),
        };
        if let Some(source) = source {
            let text = std::str::from_utf8(&source.bytes)
                .ok()
                .filter(|text| !text.contains('\0'));
            let (text, binary, end) = match text {
                Some(text)
                    if position.offset <= text.len() && text.is_char_boundary(position.offset) =>
                {
                    let mut end = position.offset.saturating_add(limit).min(text.len());
                    while !text.is_char_boundary(end) {
                        end -= 1;
                    }
                    (text[position.offset..end].to_owned(), false, end)
                }
                None if position.offset == 0 => (String::new(), true, source.bytes.len()),
                _ => return Err(BrokerError::InvalidGrant),
            };
            let next = if end < source.bytes.len() {
                Some(SavedReviewCursor {
                    offset: end,
                    ..position.clone()
                })
            } else if position.side == SavedReviewSide::Before && changes.files.contains_key(*path)
            {
                Some(SavedReviewCursor {
                    file: position.file,
                    side: SavedReviewSide::After,
                    offset: 0,
                })
            } else if position.file + 1 < paths.len() {
                Some(SavedReviewCursor {
                    file: position.file + 1,
                    side: SavedReviewSide::Before,
                    offset: 0,
                })
            } else {
                None
            };
            return Ok((
                Some(SavedReviewFile {
                    path: (*path).clone(),
                    change: (*change).into(),
                    side: position.side,
                    offset: position.offset,
                    text,
                    binary,
                    executable: source.executable,
                }),
                next,
            ));
        }
        if position.offset != 0 {
            return Err(BrokerError::InvalidGrant);
        }
        if position.side == SavedReviewSide::Before {
            position.side = SavedReviewSide::After;
        } else {
            position.file += 1;
            position.side = SavedReviewSide::Before;
        }
    }
    Ok((None, None))
}

impl BrokerService {
    /// Reads a bounded page through the exact durable saved-result owner.
    /// This blocks on retained-store I/O and never grants promotion or Run authority.
    /// # Errors
    /// Refuses a foreign operator or Run, changed revision, invalid cursor, decided result,
    /// missing or corrupt retained evidence, or a page larger than the fixed boundary.
    pub fn saved_review_page(
        &self,
        uid: u32,
        id: &str,
        run_id: &str,
        expected_revision: Option<&str>,
        cursor: SavedReviewCursor,
        limit: usize,
    ) -> Result<SavedReviewPage, BrokerError> {
        if !(4..=4096).contains(&limit) {
            return Err(BrokerError::InvalidGrant);
        }
        let (saved, directory) = self.saved_revision(uid, id)?;
        if saved.selection.run_id != run_id
            || saved.discarded
            || saved.accepted.is_some()
            || saved.application != PromotionStatus::NotRequested
        {
            return Err(BrokerError::RequestMismatch);
        }
        let current = match self.verification_status(&saved.request.verifier_session_id) {
            Ok(VerificationStatus::Completed(record)) if *record == saved.verification => {
                "matching"
            }
            Ok(VerificationStatus::Completed(_)) => "changed",
            Ok(VerificationStatus::Quarantined { .. }) => "quarantined",
            Ok(VerificationStatus::NotRequested | VerificationStatus::Unknown) | Err(_) => {
                "unavailable"
            }
        };
        let provenance = self
            .workspace_output_provenance_for_session(&saved.request.producer_session_id)
            .map_or(OutputProvenanceCode::Unknown, |value| value.code);
        let revision = Digest::of(
            &serde_json::to_vec(&(&saved, current, provenance))
                .map_err(|_| BrokerError::InvalidGrant)?,
        )
        .to_string();
        if expected_revision.is_some_and(|expected| expected != revision) {
            return Err(BrokerError::RequestMismatch);
        }

        let changes = transfer::load(&directory.join("transfer"), &saved.request.job)?;
        let snapshot = Digest::parse(&saved.request.job.snapshot_digest)
            .map_err(|_| BrokerError::InvalidGrant)?;
        let before = workspace::snapshot_files(&directory.join("transfer/snapshot"), &snapshot)?;
        let (file, next) = file_page(&saved.changes, &changes, &before, cursor, limit)?;
        Ok(SavedReviewPage {
            schema: "louiselm.saved-review-page/1".into(),
            result_id: id.into(),
            run_id: run_id.into(),
            agent_id: saved.verification.producer.request.launch.agent_id,
            checkout: saved.selection.checkout,
            destination: saved.request.destination,
            base_commit: saved.selection.expected_head,
            revision,
            verification_digest: saved.request.verification_digest,
            plan_digest: saved.request.job.plan_digest,
            verifier_inputs_digest: saved.verification.execution.verifier_inputs_digest,
            result_digest: saved.request.job.result_digest,
            commands_passed: saved.verification.execution.steps.len(),
            current_verification: current.into(),
            current_provenance: provenance,
            file,
            next,
        })
    }
}
