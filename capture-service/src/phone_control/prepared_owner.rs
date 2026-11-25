//! Owner publication of an exact private Run selection; never an admission.

use std::{
    collections::BTreeSet,
    fs::{self, File, Metadata},
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
};

use serde::Deserialize;
use thiserror::Error;

use super::{
    source::{
        PhoneSourceError, PhoneSourceStore, require_installed_skills, valid_commit, valid_digest,
    },
    source_checkout::{CheckoutProof, verify_checkout},
};
use crate::{
    PreparedRunError, PreparedRunRegistration, PreparedRunStore, PreparedRunSummary,
    RunProjectBinding, RunStore, RunStoreError,
};

const MAX_SELECTION: u64 = 65_536;

#[derive(Debug, Error)]
pub(super) enum OwnerPreparedError {
    #[error("prepared Run publication refused: {0}")]
    Invalid(&'static str),
    #[error("prepared Run source unavailable")]
    Source(#[from] PhoneSourceError),
    #[error("prepared Run identity unavailable")]
    Run(#[from] RunStoreError),
    #[error("prepared Run storage unavailable")]
    Prepared(#[from] PreparedRunError),
    #[error("prepared Run selection unavailable")]
    Io(#[from] io::Error),
    #[error("prepared Run selection is invalid")]
    Json(#[from] serde_json::Error),
}

pub(super) struct Publication<'a> {
    pub(super) run_id: &'a str,
    pub(super) source_id: &'a str,
    pub(super) checkout_id: &'a str,
    pub(super) selection: &'a Path,
    pub(super) previous: Option<(&'a str, u64)>,
}

/// Local owner operations share the service's actual Run ledger and state root.
#[derive(Clone)]
pub(super) struct PreparedOwner {
    root: PathBuf,
    runs: RunStore,
}

impl PreparedOwner {
    pub(super) fn new(root: PathBuf, runs: RunStore) -> Self {
        Self { root, runs }
    }

    fn store(&self) -> Result<PreparedRunStore, OwnerPreparedError> {
        Ok(PreparedRunStore::open(self.root.join("prepared-runs"))?)
    }

    pub(super) fn publish(
        &self,
        request: &Publication<'_>,
    ) -> Result<PreparedRunSummary, OwnerPreparedError> {
        let sources = PhoneSourceStore::open(self.root.join("run-sources"))?;
        publish(&sources, &self.store()?, &self.runs, request)
    }

    pub(super) fn list(&self) -> Result<Vec<PreparedRunSummary>, OwnerPreparedError> {
        require_installed_skills()?;
        Ok(self.store()?.list()?)
    }

    pub(super) fn inspect(&self, id: &str) -> Result<PreparedRunRegistration, OwnerPreparedError> {
        require_installed_skills()?;
        Ok(self.store()?.inspect(id)?)
    }

    pub(super) fn unpublish(&self, id: &str, revision: u64) -> Result<(), OwnerPreparedError> {
        // Removal must remain possible when the optional integration is absent.
        Ok(self.store()?.unpublish(id, revision)?)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Selection {
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct BeadScope {
    project_digest: String,
    role: String,
    issue_ids: Vec<String>,
    effects: Vec<Effect>,
    max_mutations: u64,
    expires_at_ms: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Effect {
    kind: String,
}

#[derive(Deserialize)]
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

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SkillScope {
    agents: Vec<String>,
    #[serde(rename = "allow_run")]
    _allow_run: bool,
    #[serde(default, rename = "allow_supply")]
    _allow_supply: bool,
    expires_at_ms: u64,
}

#[derive(Clone, Copy, Deserialize)]
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

#[derive(Deserialize)]
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

#[derive(Deserialize)]
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

pub(super) fn publish(
    sources: &PhoneSourceStore,
    prepared: &PreparedRunStore,
    runs: &RunStore,
    request: &Publication<'_>,
) -> Result<PreparedRunSummary, OwnerPreparedError> {
    require_installed_skills()?;
    publish_with(sources, prepared, runs, request, verify_checkout)
}

pub(super) fn publish_with(
    sources: &PhoneSourceStore,
    prepared: &PreparedRunStore,
    runs: &RunStore,
    request: &Publication<'_>,
    verify: impl FnOnce(&CheckoutProof) -> Result<(), PhoneSourceError>,
) -> Result<PreparedRunSummary, OwnerPreparedError> {
    let (bytes, selection) = read_selection(request.selection, fs::metadata(&sources.root)?.uid())?;
    if selection.envelope.run_id != request.run_id {
        return Err(OwnerPreparedError::Invalid(
            "selection Run identity differs",
        ));
    }
    sources.with_current_checkout_by(
        request.run_id,
        request.source_id,
        request.checkout_id,
        verify,
        |source, checkout| {
            if selection.worktree.path != checkout.checkout.path
                || selection.worktree.head != checkout.head
            {
                return Err(OwnerPreparedError::Invalid("selection checkout differs"));
            }
            let binding = RunProjectBinding {
                run_id: request.run_id.into(),
                project_id: source.project_id.clone(),
                source_id: source.source_id.clone(),
                checkout_id: checkout.checkout_id.clone(),
                agents: vec![selection.manifest.agent.id],
            };
            // Lock order: source/checkout, Run, then prepared registration.
            runs.with_unused_run(request.run_id, || {
                prepared.publish(request.previous, &binding, &bytes)
            })?
            .map_err(OwnerPreparedError::from)
        },
    )?
}

fn read_selection(path: &Path, owner: u32) -> Result<(String, Selection), OwnerPreparedError> {
    if !path.is_absolute() {
        return Err(OwnerPreparedError::Invalid(
            "selection path must be absolute",
        ));
    }
    let before = fs::symlink_metadata(path)?;
    if !before.file_type().is_file()
        || before.uid() != owner
        || before.mode() & 0o7777 != 0o600
        || before.nlink() != 1
        || before.len() == 0
        || before.len() > MAX_SELECTION
    {
        return Err(OwnerPreparedError::Invalid(
            "selection must be a private owner file",
        ));
    }
    let mut file = File::open(path)?;
    if !same_file(&before, &file.metadata()?) {
        return Err(OwnerPreparedError::Invalid(
            "selection changed while opening",
        ));
    }
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_SELECTION + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SELECTION
        || !same_file(&before, &file.metadata()?)
        || !same_file(&before, &fs::symlink_metadata(path)?)
    {
        return Err(OwnerPreparedError::Invalid(
            "selection changed while reading",
        ));
    }
    let bytes = String::from_utf8(bytes)
        .map_err(|_| OwnerPreparedError::Invalid("selection is not UTF-8"))?;
    let selection: Selection = serde_json::from_str(&bytes)?;
    validate_selection(&selection, owner)?;
    Ok((bytes, selection))
}

fn validate_selection(selection: &Selection, owner: u32) -> Result<(), OwnerPreparedError> {
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
        || !valid_digest(&provider.disclosure_profile)
        || provider.provider.is_empty()
        || provider.upstream.is_empty()
        || provider.addresses.is_empty()
        || provider.models != ["gpt-5.6-luna"]
        || !matches!(
            provider.max_effort.as_str(),
            "none" | "minimal" | "low" | "medium" | "high"
        )
        || !(1..=10_000).contains(&provider.max_run_requests)
        || provider.expires_at_ms != 0
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
        return Err(OwnerPreparedError::Invalid(
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
