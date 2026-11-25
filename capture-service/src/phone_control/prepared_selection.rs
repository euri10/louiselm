//! Private selection validation and bounded prepared-Run projections.

use std::{
    collections::BTreeSet,
    fs::{self, File, Metadata},
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::Path,
};

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::source::{valid_commit, valid_digest};
use crate::PreparedRunRevision;

const MAX_SELECTION: u64 = 65_536;

#[derive(Debug, Error)]
pub(super) enum SelectionError {
    #[error("prepared Run selection refused: {0}")]
    Invalid(&'static str),
    #[error("prepared Run selection unavailable")]
    Io(#[from] io::Error),
    #[error("prepared Run selection is invalid")]
    Json(#[from] serde_json::Error),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Selection {
    schema: String,
    envelope: DraftEnvelope,
    #[serde(default)]
    execution: Execution,
    beads: Vec<Bead>,
    manifest: Manifest,
    cache: String,
    plan: String,
    snapshot_parent: String,
    input_group: u64,
    worktree: Worktree,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DraftEnvelope {
    schema: String,
    run_id: String,
    envelope_id: String,
    envelope_revision: u64,
    controller_uid: u32,
    bead_scope: BeadScope,
    provider_requests: ProviderScope,
    commands: Option<serde::de::IgnoredAny>,
    skill_requests: Option<SkillScope>,
    verification_plan_digest: String,
    max_sessions: u64,
    expires_at_ms: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BeadScope {
    project_digest: String,
    role: String,
    issue_ids: Vec<String>,
    effects: Vec<Effect>,
    max_mutations: u64,
    expires_at_ms: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Effect {
    kind: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ProviderScope {
    disclosure_profile: String,
    provider: String,
    upstream: String,
    addresses: Vec<std::net::IpAddr>,
    models: Vec<String>,
    max_effort: String,
    max_run_requests: u64,
    expires_at_ms: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SkillScope {
    agents: Vec<String>,
    allow_run: bool,
    #[serde(default)]
    allow_supply: bool,
    expires_at_ms: u64,
}

#[derive(Clone, Copy, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Execution {
    Duration { duration_ms: u64 },
    StopBy { expires_at_ms: u64 },
}

impl Default for Execution {
    fn default() -> Self {
        Self::Duration {
            duration_ms: 1_800_000,
        }
    }
}

#[derive(Deserialize)]
struct Manifest {
    agent: Agent,
    envelope: ManifestEnvelope,
    skills: Skills,
    cache_base_digest: String,
    isolation: Isolation,
}

#[derive(Deserialize)]
struct Agent {
    id: String,
}

#[derive(Deserialize)]
struct ManifestEnvelope {
    id: String,
    revision: u64,
}

#[derive(Deserialize, Serialize)]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
enum Skills {
    Off {
        view_digest: String,
    },
    Enabled {
        generation_digest: String,
        view_digest: String,
    },
}

#[derive(Deserialize)]
struct Isolation {
    network: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Bead {
    id: String,
    prompt: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Worktree {
    path: String,
    journal_parent: String,
    head: String,
}

/// Builds protected content only; the caller must hold current phone authority,
/// pairing, the unused-Run guard, and the exact publication revision lock.
pub(super) fn project_detail(
    saved: &PreparedRunRevision,
    owner: u32,
) -> Result<serde_json::Value, SelectionError> {
    let selection = parse_saved(saved, owner)?;
    let skill_requests = selection.envelope.skill_requests.as_ref().map_or_else(
        || serde_json::json!({"enabled":false,"agents":[],"allow_run":false,"allow_supply":false}),
        |scope| serde_json::json!({"enabled":true,"agents":scope.agents,"allow_run":scope.allow_run,"allow_supply":scope.allow_supply}),
    );
    Ok(serde_json::json!({
        "state": "prepared",
        "agent": selection.manifest.agent.id,
        "beads": selection.beads,
        "execution": selection.execution,
        "max_sessions": selection.envelope.max_sessions,
        "verification_plan_digest": selection.envelope.verification_plan_digest,
        "bead_scope": selection.envelope.bead_scope,
        "provider_requests": selection.envelope.provider_requests,
        "skill_requests": skill_requests,
        "skills": selection.manifest.skills,
        "network": selection.manifest.isolation.network,
    }))
}

pub(super) fn parse_saved(
    saved: &PreparedRunRevision,
    owner: u32,
) -> Result<Selection, SelectionError> {
    if saved.selection.len() as u64 > MAX_SELECTION {
        return Err(SelectionError::Invalid("selection is too large"));
    }
    let selection: Selection = serde_json::from_str(&saved.selection)?;
    validate_selection(&selection, owner)?;
    if selection.envelope.run_id != saved.binding.run_id
        || saved.binding.agents.as_slice() != [selection.manifest.agent.id.as_str()]
    {
        return Err(SelectionError::Invalid("selection binding differs"));
    }
    Ok(selection)
}

impl Selection {
    pub(super) fn run_id(&self) -> &str {
        &self.envelope.run_id
    }

    pub(super) fn matches_checkout(&self, path: &str, head: &str) -> bool {
        self.worktree.path == path && self.worktree.head == head
    }

    pub(super) fn agent_id(&self) -> &str {
        &self.manifest.agent.id
    }

    pub(super) fn bead_count(&self) -> usize {
        self.beads.len()
    }
}

pub(super) fn read_selection(
    path: &Path,
    owner: u32,
) -> Result<(String, Selection), SelectionError> {
    if !path.is_absolute() {
        return Err(SelectionError::Invalid("selection path must be absolute"));
    }
    let before = fs::symlink_metadata(path)?;
    if !before.file_type().is_file()
        || before.uid() != owner
        || before.mode() & 0o7777 != 0o600
        || before.nlink() != 1
        || before.len() == 0
        || before.len() > MAX_SELECTION
    {
        return Err(SelectionError::Invalid(
            "selection must be a private owner file",
        ));
    }
    let mut file = File::open(path)?;
    if !same_file(&before, &file.metadata()?) {
        return Err(SelectionError::Invalid("selection changed while opening"));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_SELECTION + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SELECTION
        || !same_file(&before, &file.metadata()?)
        || !same_file(&before, &fs::symlink_metadata(path)?)
    {
        return Err(SelectionError::Invalid("selection changed while reading"));
    }
    let bytes =
        String::from_utf8(bytes).map_err(|_| SelectionError::Invalid("selection is not UTF-8"))?;
    let selection: Selection = serde_json::from_str(&bytes)?;
    validate_selection(&selection, owner)?;
    Ok((bytes, selection))
}

fn validate_selection(selection: &Selection, owner: u32) -> Result<(), SelectionError> {
    let valid_path = |value: &str| Path::new(value).is_absolute() && !value.contains('\0');
    let mut beads = BTreeSet::new();
    let envelope = &selection.envelope;
    let scope = &envelope.bead_scope;
    let provider = &envelope.provider_requests;
    let manifest = &selection.manifest;
    let limit = match selection.execution {
        Execution::Duration { duration_ms } => duration_ms,
        Execution::StopBy { expires_at_ms } => expires_at_ms,
    };
    let execution = (1..=9_007_199_254_740_991).contains(&limit);
    let skills = match &manifest.skills {
        Skills::Off { view_digest } => valid_digest(view_digest),
        Skills::Enabled {
            generation_digest,
            view_digest,
        } => valid_digest(generation_digest) && valid_digest(view_digest),
    };
    if selection.schema != "louiselm.operator.bead-run/2"
        || !(3..=5).contains(&selection.beads.len())
        || selection.beads.iter().any(|bead| {
            bead.id.is_empty()
                || bead.id.len() > 512
                || bead.id.contains('\0')
                || bead.prompt.is_empty()
                || bead.prompt.contains('\0')
                || !beads.insert(&bead.id)
        })
        || manifest.agent.id.is_empty()
        || manifest.agent.id.len() > 512
        || manifest.agent.id.contains('\0')
        || envelope.schema != "louiselm.broker.run-envelope/2"
        || envelope.envelope_id.is_empty()
        || envelope.envelope_id.len() > 128
        || !envelope
            .envelope_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        || envelope.envelope_revision != 1
        || envelope.controller_uid == 0
        || envelope.controller_uid != owner
        || envelope.expires_at_ms != 0
        || envelope.commands.is_some()
        || !valid_digest(&envelope.verification_plan_digest)
        || envelope.max_sessions != 2 * selection.beads.len() as u64
        || scope.role != "coordinator"
        || !valid_digest(&scope.project_digest)
        || scope
            .issue_ids
            .iter()
            .map(String::as_str)
            .ne(beads.iter().map(|value| value.as_str()))
        || scope.effects.len() != 1
        || scope.effects[0].kind != "comment_add"
        || scope.max_mutations != selection.beads.len() as u64
        || scope.expires_at_ms != 0
        || !valid_provider(provider)
        || !valid_skill_scope(envelope.skill_requests.as_ref())
        || !execution
        || manifest.envelope.id != envelope.envelope_id
        || manifest.envelope.revision != envelope.envelope_revision
        || !skills
        || !valid_digest(&manifest.cache_base_digest)
        || !matches!(manifest.isolation.network.as_str(), "denied" | "brokered")
        || !valid_commit(&selection.worktree.head)
        || selection.input_group == 0
        || selection.input_group > u64::from(u32::MAX)
        || !valid_path(&selection.cache)
        || !valid_path(&selection.plan)
        || !valid_path(&selection.snapshot_parent)
        || !valid_path(&selection.worktree.path)
        || !valid_path(&selection.worktree.journal_parent)
    {
        return Err(SelectionError::Invalid(
            "selection identity or instructions are invalid",
        ));
    }
    Ok(())
}

fn valid_skill_scope(scope: Option<&SkillScope>) -> bool {
    scope.is_none_or(|scope| {
        scope.expires_at_ms == 0
            && !scope.agents.is_empty()
            && scope.agents.len() <= 32
            && scope.agents.windows(2).all(|pair| pair[0] < pair[1])
            && scope.agents.iter().all(|agent| {
                !agent.is_empty()
                    && agent.len() <= 128
                    && agent
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
            })
    })
}

fn valid_provider(scope: &ProviderScope) -> bool {
    let upstream = url::Url::parse(&scope.upstream).is_ok_and(|url| {
        url.scheme() == "https"
            && url.host_str().is_some()
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none()
            && url.as_str() == scope.upstream
            && url.path() == "/v1/responses"
    });
    valid_digest(&scope.disclosure_profile)
        && !scope.provider.is_empty()
        && scope.provider.len() <= 64
        && scope
            .provider
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        && scope.upstream.len() <= 2048
        && upstream
        && (1..=16).contains(&scope.addresses.len())
        && scope.addresses.iter().collect::<BTreeSet<_>>().len() == scope.addresses.len()
        && scope
            .addresses
            .iter()
            .all(|address| !address.is_unspecified() && !address.is_multicast())
        && scope.models == ["gpt-5.6-luna"]
        && matches!(
            scope.max_effort.as_str(),
            "none" | "minimal" | "low" | "medium" | "high"
        )
        && (1..=10_000).contains(&scope.max_run_requests)
        && scope.expires_at_ms == 0
}

fn same_file(left: &Metadata, right: &Metadata) -> bool {
    left.file_type().is_file()
        && right.file_type().is_file()
        && left.dev() == right.dev()
        && left.ino() == right.ino()
        && left.uid() == right.uid()
        && left.mode() == right.mode()
        && left.nlink() == right.nlink()
        && left.len() == right.len()
        && left.mtime() == right.mtime()
        && left.mtime_nsec() == right.mtime_nsec()
        && left.ctime() == right.ctime()
        && left.ctime_nsec() == right.ctime_nsec()
}
