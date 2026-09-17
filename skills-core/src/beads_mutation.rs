//! Exact requests to mediate one canonical Beads mutation through the broker.
//!
//! This opt-in path lets the Control broker invoke `br` against a configured
//! canonical `.beads` database. Direct-access cutover is separate. This module describes the immutable request
//! and its durable, bounded outcome; it performs no I/O and holds no
//! authority of its own.

use serde::{Deserialize, Serialize};

mod permission;
pub use permission::{ApprovedBeadsMutations, BeadsCapability, BeadsEffect, BeadsRole};
mod control;
pub use control::{
    BeadsControlDecision, BeadsInspection, BeadsInspectionDetail, BeadsReconciliation,
    BeadsResolution,
};

/// One canonical Beads mutation a Session may ask the broker to perform.
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
    /// Atomically claims the issue for the authenticated actor.
    Claim {
        /// Exact target issue.
        issue_id: String,
    },
    /// Changes a nonterminal status; upstream Beads enforces its workflow.
    StatusUpdate {
        /// Exact target issue.
        issue_id: String,
        /// Exact approved status. Claim and closure use their dedicated operations.
        status: String,
    },
    /// Adds one exact approved label.
    LabelAdd {
        /// Exact target issue.
        issue_id: String,
        /// Label identifier, never a CLI option.
        label: String,
    },
    /// Removes one exact approved label.
    LabelRemove {
        /// Exact target issue.
        issue_id: String,
        /// Label identifier, never a CLI option.
        label: String,
    },
    /// Adds one typed edge with both endpoints in the approved scope.
    DependencyAdd {
        /// Dependent issue.
        issue_id: String,
        /// Prerequisite or related issue.
        depends_on_id: String,
        /// Exact approved edge kind.
        dependency_type: BeadsDependencyKind,
    },
    /// Removes one edge; coordinator authority is required because it may block work.
    DependencyRemove {
        /// Dependent issue.
        issue_id: String,
        /// Prerequisite or related issue.
        depends_on_id: String,
        /// Exact approved edge kind, including when the pair has multiple edges.
        dependency_type: BeadsDependencyKind,
    },
    /// Closes with one explicit typed verdict; never forces or bypasses upstream policy.
    Close {
        /// Exact target issue.
        issue_id: String,
        /// Explanation without additional typed-verdict tokens.
        reason: String,
        /// One checkable close verdict.
        verdict: BeadsCloseVerdict,
    },
}

impl BeadsMutationKind {
    fn valid(&self) -> bool {
        if !issue_identifier(self.issue_id()) {
            return false;
        }
        match self {
            Self::CommentAdd { text, .. } => text_valid(text),
            Self::Claim { .. } => true,
            Self::StatusUpdate { status, .. } => status_valid(status),
            Self::LabelAdd { label, .. } | Self::LabelRemove { label, .. } => label_valid(label),
            Self::DependencyAdd { depends_on_id, .. }
            | Self::DependencyRemove { depends_on_id, .. } => {
                issue_identifier(depends_on_id) && depends_on_id != self.issue_id()
            }
            Self::Close {
                reason, verdict, ..
            } => {
                text_valid(reason)
                    && verdict.valid()
                    && !["consumer:", "gate:", "live:", "inert:", "none:"]
                        .iter()
                        .any(|token| reason.contains(token))
            }
        }
    }

    /// Exact primary issue, independent of the operation's payload.
    #[must_use]
    pub fn issue_id(&self) -> &str {
        match self {
            Self::CommentAdd { issue_id, .. }
            | Self::Claim { issue_id }
            | Self::StatusUpdate { issue_id, .. }
            | Self::LabelAdd { issue_id, .. }
            | Self::LabelRemove { issue_id, .. }
            | Self::DependencyAdd { issue_id, .. }
            | Self::DependencyRemove { issue_id, .. }
            | Self::Close { issue_id, .. } => issue_id,
        }
    }
}

/// Supported upstream edge kinds; the broker never implements graph semantics.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadsDependencyKind {
    /// A readiness blocker, restricted to coordinators.
    Blocks,
    /// Hierarchical ownership/readiness, restricted to coordinators.
    ParentChild,
    /// An informational relationship without blocker authority.
    Related,
}

impl BeadsDependencyKind {
    /// The upstream CLI spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Blocks => "blocks",
            Self::ParentChild => "parent-child",
            Self::Related => "related",
        }
    }
}

/// Exactly one project close verdict, distinct from a mutation's process outcome.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadsVerdictKind {
    /// Production consumer.
    Consumer,
    /// Runnable regression gate.
    Gate,
    /// Maintainer-confirmed live behavior.
    Live,
    /// Inert implementation with an explicit follow-up issue.
    Inert,
    /// No behavioral acceptance applies.
    None,
}

impl BeadsVerdictKind {
    /// The upstream typed-reference spelling.
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Consumer => "consumer",
            Self::Gate => "gate",
            Self::Live => "live",
            Self::Inert => "inert",
            Self::None => "none",
        }
    }
}

/// Bounded evidence reference supplied to upstream closure policy.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeadsCloseVerdict {
    /// Exactly one verdict kind.
    pub kind: BeadsVerdictKind,
    /// Non-whitespace reference; `inert` names the follow-up issue.
    pub reference: String,
}

impl BeadsCloseVerdict {
    fn valid(&self) -> bool {
        !self.reference.is_empty()
            && self.reference.len() <= 512
            && self
                .reference
                .chars()
                .all(|c| !c.is_whitespace() && !c.is_control())
            && (self.kind != BeadsVerdictKind::Inert || issue_identifier(&self.reference))
            && !["consumer:", "gate:", "live:", "inert:", "none:"]
                .iter()
                .any(|token| self.reference.contains(token))
    }
}

fn text_valid(value: &str) -> bool {
    !value.is_empty() && value.len() <= 8192 && !value.contains('\0')
}

fn status_valid(value: &str) -> bool {
    identifier(value)
        && !value.bytes().any(|byte| byte.is_ascii_uppercase())
        && !matches!(value, "closed" | "tombstone" | "in_progress" | "inprogress")
}

fn label_valid(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':' | b'/'))
}

/// Immutable request content. No actor, path or broker-derived state.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeadsMutationRequest {
    /// Client retry identity; changing content requires a fresh identity.
    pub request_id: String,
    /// Whether inability to perform this work requires operator attention.
    /// This requests escalation only; it never grants mutation authority.
    pub required: bool,
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

/// One durable required-action escalation; it is never authorization to retry a write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeadsEscalation {
    /// Stable broker operation UUID, also used by the Attention item.
    pub operation_id: String,
    /// Exact minimum capability required for one attempt; no budget is granted.
    pub capability: BeadsCapability,
}

impl BeadsEscalation {
    /// Validates the bounded condition without changing authority.
    #[must_use]
    pub fn valid(&self) -> bool {
        canonical_uuid(&self.operation_id) && self.capability.valid()
    }
}

/// Durable outcome of one broker-mediated mutation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BeadsMutationOutcome {
    /// An interrupted attempt may already have mutated Beads. Never auto-retry it.
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
            required: false,
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

    #[test]
    fn typed_effects_accept_exact_arguments_without_actor_or_shell_authority() {
        for kind in [
            serde_json::json!({"kind":"claim","issue_id":"test-1"}),
            serde_json::json!({"kind":"status_update","issue_id":"test-1","status":"blocked"}),
            serde_json::json!({"kind":"label_add","issue_id":"test-1","label":"needs-design"}),
            serde_json::json!({"kind":"label_remove","issue_id":"test-1","label":"needs-design"}),
            serde_json::json!({"kind":"dependency_add","issue_id":"test-1","depends_on_id":"test-2","dependency_type":"blocks"}),
            serde_json::json!({"kind":"dependency_remove","issue_id":"test-1","depends_on_id":"test-2","dependency_type":"blocks"}),
            serde_json::json!({"kind":"close","issue_id":"test-1","reason":"Verified exact behavior","verdict":{"kind":"gate","reference":"tests/fixture.rs:12"}}),
        ] {
            let value = serde_json::json!({"request_id":"one","required":false,"kind":kind});
            let parsed = serde_json::from_value::<BeadsMutationRequest>(value.clone());
            assert!(parsed.is_ok_and(|request| request.valid()), "{value}");
            let mut forged = value;
            forged["kind"]["actor"] = "forged/session".into();
            assert!(serde_json::from_value::<BeadsMutationRequest>(forged).is_err());
        }
    }

    #[test]
    fn typed_effects_reject_authority_bypasses_and_ambiguous_close_evidence() {
        for kind in [
            serde_json::json!({"kind":"status_update","issue_id":"test-1","status":"closed"}),
            serde_json::json!({"kind":"status_update","issue_id":"test-1","status":"in_progress"}),
            serde_json::json!({"kind":"status_update","issue_id":"test-1","status":"inprogress"}),
            serde_json::json!({"kind":"status_update","issue_id":"test-1","status":"IN_PROGRESS"}),
            serde_json::json!({"kind":"status_update","issue_id":"test-1","status":"CLOSED"}),
            serde_json::json!({"kind":"status_update","issue_id":"test-1","status":"tombstone"}),
            serde_json::json!({"kind":"label_add","issue_id":"test-1","label":"one,two"}),
            serde_json::json!({"kind":"label_remove","issue_id":"test-1","label":"--all"}),
            serde_json::json!({"kind":"dependency_add","issue_id":"test-1","depends_on_id":"test-1","dependency_type":"blocks"}),
            serde_json::json!({"kind":"dependency_remove","issue_id":"test-1","depends_on_id":"--all","dependency_type":"blocks"}),
            serde_json::json!({"kind":"close","issue_id":"test-1","reason":"gate:second","verdict":{"kind":"gate","reference":"first"}}),
            serde_json::json!({"kind":"close","issue_id":"test-1","reason":"Checked","verdict":{"kind":"gate","reference":"first none:second"}}),
            serde_json::json!({"kind":"close","issue_id":"test-1","reason":"Deferred","verdict":{"kind":"inert","reference":"not/an/issue"}}),
        ] {
            let value = serde_json::json!({"request_id":"one","required":true,"kind":kind});
            assert!(
                serde_json::from_value::<BeadsMutationRequest>(value.clone())
                    .is_ok_and(|request| !request.valid()),
                "{value}"
            );
        }
    }
}
