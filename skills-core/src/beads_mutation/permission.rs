//! Exact effect grants are controller authority, never request payload authority.

use super::{
    BeadsDependencyKind, BeadsMutationKind, BeadsMutationRequest, issue_identifier, label_valid,
    status_valid,
};
use serde::{Deserialize, Serialize};

/// Trusted role chosen at launch; workers cannot grant themselves coordinator rights.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BeadsRole {
    /// Ordinary work within an explicitly approved scope.
    Worker,
    /// May receive closure and graph-control authority; still needs exact effect grants.
    Coordinator,
}

/// One independently approved operation; status/label values are exact, not wildcards.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BeadsEffect {
    /// Add a comment without authority over other issue fields.
    CommentAdd,
    /// Atomically claim for this Session.
    Claim,
    /// Set exactly one approved nonterminal status.
    StatusUpdate {
        /// Exact status.
        status: String,
    },
    /// Add exactly one approved label.
    LabelAdd {
        /// Exact label.
        label: String,
    },
    /// Remove exactly one approved label.
    LabelRemove {
        /// Exact label.
        label: String,
    },
    /// Add exactly one approved edge kind.
    DependencyAdd {
        /// Exact upstream edge kind.
        dependency_type: BeadsDependencyKind,
    },
    /// Remove one approved edge kind between approved issues; coordinator only.
    DependencyRemove {
        /// Exact upstream edge kind.
        dependency_type: BeadsDependencyKind,
    },
    /// Close through upstream policy with a typed verdict; coordinator only.
    Close,
}

impl BeadsEffect {
    /// Required authority without comment/reason payloads.
    #[must_use]
    pub fn for_mutation(kind: &BeadsMutationKind) -> Self {
        match kind {
            BeadsMutationKind::CommentAdd { .. } => Self::CommentAdd,
            BeadsMutationKind::Claim { .. } => Self::Claim,
            BeadsMutationKind::StatusUpdate { status, .. } => Self::StatusUpdate {
                status: status.clone(),
            },
            BeadsMutationKind::LabelAdd { label, .. } => Self::LabelAdd {
                label: label.clone(),
            },
            BeadsMutationKind::LabelRemove { label, .. } => Self::LabelRemove {
                label: label.clone(),
            },
            BeadsMutationKind::DependencyAdd {
                dependency_type, ..
            } => Self::DependencyAdd {
                dependency_type: *dependency_type,
            },
            BeadsMutationKind::DependencyRemove {
                dependency_type, ..
            } => Self::DependencyRemove {
                dependency_type: *dependency_type,
            },
            BeadsMutationKind::Close { .. } => Self::Close,
        }
    }

    pub(super) fn valid(&self) -> bool {
        match self {
            Self::StatusUpdate { status } => status_valid(status),
            Self::LabelAdd { label } | Self::LabelRemove { label } => label_valid(label),
            _ => true,
        }
    }

    /// Role policy only narrows an explicit grant.
    #[must_use]
    pub fn permitted_role(&self, role: BeadsRole) -> bool {
        if role == BeadsRole::Coordinator {
            return true;
        }
        match self {
            Self::Close
            | Self::DependencyRemove { .. }
            | Self::DependencyAdd {
                dependency_type: BeadsDependencyKind::Blocks | BeadsDependencyKind::ParentChild,
            } => false,
            Self::LabelAdd { label } | Self::LabelRemove { label } => {
                label != "integration_verified"
            }
            _ => true,
        }
    }
}

/// Narrow requested expansion, containing identifiers and effect type, never mutation text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BeadsCapability {
    /// Canonical project fixed by broker configuration.
    pub project_digest: String,
    /// Sorted exact target IDs; an edge requires both endpoints.
    pub issue_ids: Vec<String>,
    /// One exact required effect, including a status or label value where applicable.
    pub effect: BeadsEffect,
    /// Minimum role permitted to perform this effect.
    pub role: BeadsRole,
}

impl BeadsCapability {
    /// Describes one effect, without granting it or copying comment/closure text.
    #[must_use]
    pub fn for_mutation(project_digest: String, kind: &BeadsMutationKind) -> Self {
        let effect = BeadsEffect::for_mutation(kind);
        let role = if effect.permitted_role(BeadsRole::Worker) {
            BeadsRole::Worker
        } else {
            BeadsRole::Coordinator
        };
        let mut issue_ids = vec![kind.issue_id().into()];
        if let BeadsMutationKind::DependencyAdd { depends_on_id, .. }
        | BeadsMutationKind::DependencyRemove { depends_on_id, .. } = kind
        {
            issue_ids.push(depends_on_id.clone());
            issue_ids.sort();
        }
        Self {
            project_digest,
            issue_ids,
            effect,
            role,
        }
    }

    /// Rejects malformed, oversized or contradictory proposed scope.
    #[must_use]
    pub fn valid(&self) -> bool {
        crate::Digest::parse(&self.project_digest)
            .is_ok_and(|digest| digest.to_string() == self.project_digest)
            && (1..=2).contains(&self.issue_ids.len())
            && self.issue_ids.iter().all(|id| issue_identifier(id))
            && self.issue_ids.windows(2).all(|pair| pair[0] < pair[1])
            && self.effect.valid()
            && self.effect.permitted_role(self.role)
            && (self.issue_ids.len() == 2)
                == matches!(
                    self.effect,
                    BeadsEffect::DependencyAdd { .. } | BeadsEffect::DependencyRemove { .. }
                )
    }
}

/// Explicit controller-issued capability for one Session and canonical project.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedBeadsMutations {
    /// Digest of the canonical absolute project path, fixed by the operator.
    pub project_digest: String,
    /// Trusted role; no request may override it.
    pub role: BeadsRole,
    /// Sorted unique exact issue IDs, including both endpoints of dependency edits.
    pub issue_ids: Vec<String>,
    /// Sorted unique exact effects; no inferred authority from another effect.
    pub effects: Vec<BeadsEffect>,
    /// Non-refundable total distinct attempts across all effects (1..=64).
    pub max_mutations: u32,
    /// Exclusive absolute expiry; retries never renew permission.
    pub expires_at_ms: u64,
}

impl ApprovedBeadsMutations {
    /// Checks bounded scope, budget, role and lifetime without external effects.
    #[must_use]
    pub fn valid(&self, now_ms: u64) -> bool {
        crate::Digest::parse(&self.project_digest)
            .is_ok_and(|digest| digest.to_string() == self.project_digest)
            && !self.issue_ids.is_empty()
            && self.issue_ids.len() <= 64
            && self.issue_ids.iter().all(|id| issue_identifier(id))
            && self.issue_ids.windows(2).all(|pair| pair[0] < pair[1])
            && !self.effects.is_empty()
            && self.effects.len() <= 64
            && self
                .effects
                .iter()
                .all(|effect| effect.valid() && effect.permitted_role(self.role))
            && self.effects.windows(2).all(|pair| pair[0] < pair[1])
            && (1..=64).contains(&self.max_mutations)
            && now_ms < self.expires_at_ms
    }

    /// Checks exact effect and every target against the controller's envelope.
    #[must_use]
    pub fn permits(&self, request: &BeadsMutationRequest, now_ms: u64) -> bool {
        let effect = BeadsEffect::for_mutation(&request.kind);
        self.valid(now_ms)
            && request.valid()
            && effect.permitted_role(self.role)
            && self.effects.contains(&effect)
            && self
                .issue_ids
                .iter()
                .any(|id| id == request.kind.issue_id())
            && match &request.kind {
                BeadsMutationKind::DependencyAdd { depends_on_id, .. }
                | BeadsMutationKind::DependencyRemove { depends_on_id, .. } => {
                    self.issue_ids.contains(depends_on_id)
                }
                _ => true,
            }
    }
}
