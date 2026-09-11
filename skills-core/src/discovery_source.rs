//! Shared discovery-source wire records; no verification or confinement authority.

use serde::{Deserialize, Serialize};

/// Logical discovery namespace; paths are relative to its confined root.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceRoot {
    /// Agent private home.
    Home,
    /// Editable project workspace.
    Workspace,
    /// Registered immutable runtime.
    Runtime,
    /// Declared system roots.
    System,
}

/// Fixed inventory categories. Every category requires an explicit account.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    /// Global instruction-file discovery.
    GlobalInstructions,
    /// Project instruction files, frozen per Session despite workspace edits.
    ProjectInstructions,
    /// Repository-local native skill discovery.
    RepositorySkills,
    /// Global/native skill discovery outside the approved view.
    NativeSkills,
    /// Runtime-managed system skills outside the approved view.
    SystemSkills,
    /// Plugin discovery outside measured schemas.
    Plugins,
    /// Native MCP configuration; always masked in v1.
    NativeMcp,
    /// Measured tool schemas.
    ToolSchemas,
    /// Measured plugin schemas.
    PluginSchemas,
    /// The Agent-scoped admitted view, including the empty mask.
    ManagedSkills,
}

impl SourceKind {
    /// Every required category, in canonical order.
    pub const ALL: [Self; 10] = [
        Self::GlobalInstructions,
        Self::ProjectInstructions,
        Self::RepositorySkills,
        Self::NativeSkills,
        Self::SystemSkills,
        Self::Plugins,
        Self::NativeMcp,
        Self::ToolSchemas,
        Self::PluginSchemas,
        Self::ManagedSkills,
    ];

    /// Stable source category name.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::GlobalInstructions => "global_instructions",
            Self::ProjectInstructions => "project_instructions",
            Self::RepositorySkills => "repository_skills",
            Self::NativeSkills => "native_skills",
            Self::SystemSkills => "system_skills",
            Self::Plugins => "plugins",
            Self::NativeMcp => "native_mcp",
            Self::ToolSchemas => "tool_schemas",
            Self::PluginSchemas => "plugin_schemas",
            Self::ManagedSkills => "managed_skills",
        }
    }
}

/// One complete discovery root or instruction file known to a measured adapter.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    /// Stable inventory-local identity.
    pub id: String,
    /// Role of this source.
    pub kind: SourceKind,
    /// Confined namespace containing the source.
    pub root: SourceRoot,
    /// Canonical relative path; directory sources cover their entire subtree.
    pub path: String,
}

/// Backend-established source treatment. These wire claims require a signature.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SourceControl {
    /// Automatic loading consumes only the immutable manifest-bound snapshot.
    FrozenSnapshot {
        /// Canonical snapshot or Instruction view digest.
        digest: String,
    },
    /// The discovery source is inaccessible or disabled for the Session lifetime.
    Masked {
        /// Exact source-control observation reference, bound in the manifest.
        evidence_id: String,
    },
}

/// One launcher-observed source; duplicate observations are contradictory.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceObservation {
    /// Exact source observed, including namespace and path.
    pub source: Source,
    /// Observed control, never an Agent-authored assertion.
    pub control: SourceControl,
}

/// Observations included in isolation evidence before the launcher signs it.
///
/// Booleans describe independent backend observations, not optional requests.
/// A verifier checks the launcher signature before any value grants authority.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceEvidence {
    /// Evidence contract version.
    pub schema: String,
    /// Digest of the runtime-registered source inventory.
    pub inventory_digest: String,
    /// Source-control observation identity bound by the input manifest.
    pub evidence_id: String,
    /// No live PATH resolution, launcher shim, or executable refresh.
    pub fixed_executable: bool,
    /// Runtime self-update is disabled and cannot add instruction sources.
    pub self_update_disabled: bool,
    /// Automatic discovery/reload cannot consume mutable workspace instructions.
    pub workspace_rediscovery_disabled: bool,
    /// Complete observed set, exactly one entry per declared source.
    pub sources: Vec<SourceObservation>,
}
