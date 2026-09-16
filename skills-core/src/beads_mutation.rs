//! Exact requests to mediate one canonical Beads mutation through the broker.
//!
//! This opt-in path lets the Control broker invoke `br` against a configured
//! canonical `.beads` database. Direct-access cutover is separate. This module describes the immutable request
//! and its durable, bounded outcome; it performs no I/O and holds no
//! authority of its own.

use serde::{Deserialize, Serialize};

/// One canonical Beads mutation a Session may ask the broker to perform.
///
/// Deliberately one operation for now. Further kinds (claim, status/label
/// update, dependency edit, close-with-verdict) are follow-up slices, not
/// stubbed here ahead of need.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BeadsMutationKind {
    /// `br comments add <issue_id> -m <text>`.
    CommentAdd {
        /// Target issue identifier.
        issue_id: String,
        /// Exact comment body, passed to `br` unmodified.
        text: String,
    },
}

impl BeadsMutationKind {
    fn valid(&self) -> bool {
        match self {
            Self::CommentAdd { issue_id, text } => {
                issue_identifier(issue_id)
                    && !text.is_empty()
                    && text.len() <= 8192
                    && !text.contains('\0')
            }
        }
    }
}

/// Explicit comment capability issued by the trusted controller for one launch.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedBeadsComments {
    /// Sorted, unique exact issue IDs; prefixes do not confer authority.
    pub issue_ids: Vec<String>,
    /// Non-refundable maximum number of distinct attempts (1..=64).
    pub max_comments: u32,
    /// Exclusive absolute expiry; retries never renew this permission.
    pub expires_at_ms: u64,
}

impl ApprovedBeadsComments {
    /// Checks bounded scope, budget and lifetime without external effects.
    #[must_use]
    pub fn valid(&self, now_ms: u64) -> bool {
        !self.issue_ids.is_empty()
            && self.issue_ids.len() <= 64
            && self.issue_ids.iter().all(|id| issue_identifier(id))
            && self.issue_ids.windows(2).all(|pair| pair[0] < pair[1])
            && (1..=64).contains(&self.max_comments)
            && now_ms < self.expires_at_ms
    }

    /// Checks exact comment scope within the approved envelope.
    #[must_use]
    pub fn permits(&self, request: &BeadsMutationRequest, now_ms: u64) -> bool {
        let BeadsMutationKind::CommentAdd { issue_id, .. } = &request.kind;
        self.valid(now_ms) && request.valid() && self.issue_ids.contains(issue_id)
    }
}

/// Immutable request content. No actor, path or broker-derived state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeadsMutationRequest {
    /// Client retry identity; changing content requires a fresh identity.
    pub request_id: String,
    /// The exact mutation to perform.
    pub kind: BeadsMutationKind,
}

impl BeadsMutationRequest {
    /// Checks bounded, exact content without invoking `br` or touching state.
    #[must_use]
    pub fn valid(&self) -> bool {
        identifier(&self.request_id) && self.kind.valid()
    }
}

/// Durable outcome of one broker-mediated mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BeadsMutationOutcome {
    /// An interrupted attempt may already have written the comment. Never auto-retry it.
    Unknown,
    /// `br` exited zero.
    Completed,
    /// `br` exited non-zero or was terminated by a signal.
    Failed {
        /// The process exit code, when the process ran and could be observed.
        exit_code: Option<i32>,
    },
}

/// Bounded result returned to the exact requesting Session.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeadsMutationStatus {
    /// Original caller retry identity.
    pub request_id: String,
    /// Broker-created stable operation UUID.
    pub operation_id: String,
    /// Durable outcome; the same request never executes again.
    pub outcome: BeadsMutationOutcome,
}

impl BeadsMutationStatus {
    /// Checks the bounded retry identity and canonical operation UUID.
    #[must_use]
    pub fn valid(&self) -> bool {
        identifier(&self.request_id) && canonical_uuid(&self.operation_id)
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
}

/// A Beads issue identifier: like [`identifier`], but also allowing `.` for
/// hierarchical issue IDs (e.g. `louiselm-qbr.5.1.5`), and never starting
/// with `-` so it cannot be mistaken for a flag when passed as a CLI argument.
fn issue_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn canonical_uuid(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(index, byte)| {
            if matches!(index, 8 | 13 | 18 | 23) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
}

#[cfg(test)]
mod tests {
    use super::{BeadsMutationKind, BeadsMutationRequest, BeadsMutationStatus};
    use crate::beads_mutation::BeadsMutationOutcome;

    fn request(issue_id: &str, text: &str) -> BeadsMutationRequest {
        BeadsMutationRequest {
            request_id: "req-1".to_owned(),
            kind: BeadsMutationKind::CommentAdd {
                issue_id: issue_id.to_owned(),
                text: text.to_owned(),
            },
        }
    }

    #[test]
    fn accepts_hierarchical_issue_ids() {
        assert!(request("louiselm-qbr.5.1.5", "hello").valid());
    }

    #[test]
    fn rejects_empty_comment_text() {
        assert!(!request("louiselm-qbr.5.1.5", "").valid());
    }

    #[test]
    fn rejects_flag_shaped_issue_id() {
        assert!(!request("--actor", "hello").valid());
    }

    #[test]
    fn rejects_empty_request_id() {
        let mut request = request("louiselm-qbr.5.1.5", "hello");
        request.request_id = String::new();
        assert!(!request.valid());
    }

    #[test]
    fn status_requires_canonical_uuid() {
        let status = BeadsMutationStatus {
            request_id: "req-1".to_owned(),
            operation_id: "not-a-uuid".to_owned(),
            outcome: BeadsMutationOutcome::Completed,
        };
        assert!(!status.valid());
    }
}
