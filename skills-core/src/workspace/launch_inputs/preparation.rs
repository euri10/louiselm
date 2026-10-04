//! Assemble owner-measured inputs and an inactive proposal; never broker authority.

use super::{LoadedInputs, WorkspaceError, filesystem};
use crate::{
    Digest, Policy, Store,
    cache::CacheBase,
    registry::Registry,
    runtime_configuration,
    session_manifest::{
        EnvelopeInput, IsolationIntent, MeasuredInput, SessionInputManifest, SessionInputs,
    },
};
use serde::{Deserialize, Serialize};
use std::{fs, path::Path};

const PROPOSAL_SCHEMA: &str = "louiselm.run.input-proposal/1";

/// Explicit operator selection, separate from registry/store authority.
pub struct Preparation<'a> {
    /// Registered Agent whose runtime and fixed configuration are measured.
    pub agent_id: &'a str,
    /// Registered isolation profile; the prospective Run starts at revision one.
    pub envelope_id: &'a str,
    /// Already reviewed source snapshot, never an implicit working-tree selection.
    pub snapshot: &'a Path,
    /// Exact selected source snapshot identity.
    pub snapshot_digest: &'a Digest,
    /// Selected cache tree, including an explicitly empty cache.
    pub cache: &'a Path,
    /// Exact selected cache identity; changed bytes refuse.
    pub cache_digest: &'a Digest,
    /// Complete explicit instruction paths within the selected source snapshot.
    pub project_instructions: &'a [String],
}

/// Inactive proposed Run identity and exact inputs. Not a Run envelope or approval.
/// Bead scope, budgets, expiry and launch approval remain with the existing operator.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunProposal {
    /// Closed proposal schema.
    pub schema: String,
    /// Fresh UUID for the proposed Run, not an allocated Session or grant.
    pub run_id: String,
    /// Registered profile and proposed revision one, not an approved revision.
    pub envelope: EnvelopeInput,
    /// Complete canonical manifest stored beside this proposal.
    pub manifest_digest: String,
    /// Source commit already selected by the operator.
    pub base_commit: String,
}

/// Resolves real owners, measures selected bytes, and atomically publishes inputs.
///
/// Blocking local filesystem/crypto I/O. `registry` and `store` must be trusted
/// caller-owned authorities. The fresh private output contains `manifest.json`,
/// `proposal.json`, `snapshot/` and `cache/`. No broker, Agent or Provider is called.
/// The operator must separately approve a finite envelope for this proposed Run.
///
/// # Errors
/// Refuses unknown configuration, unresolved supply, missing instructions, changed
/// source/cache bytes, malformed inputs, unsafe/nested output or existing output.
pub fn prepare(
    store: &Store,
    policy: &Policy,
    registry: &Registry,
    selection: &Preparation<'_>,
    output: &Path,
) -> Result<RunProposal, WorkspaceError> {
    filesystem::validate_output(output, &fs::canonicalize(selection.snapshot)?)?;
    filesystem::validate_output(output, &fs::canonicalize(selection.cache)?)?;
    let configuration = runtime_configuration::resolve(&registry.agent(selection.agent_id)?)?;
    let envelope = registry.envelope(selection.envelope_id)?;
    let (record, files) =
        super::super::load_snapshot(selection.snapshot, selection.snapshot_digest)?;
    let source = record.preview()?;
    let cache = CacheBase::capture(selection.cache)?;
    if cache.digest() != selection.cache_digest {
        return Err(WorkspaceError::Invalid("selected cache digest mismatch"));
    }
    if selection.project_instructions.len() > super::super::MAX_FILES {
        return Err(WorkspaceError::Invalid("too many project instructions"));
    }
    let instructions = selection
        .project_instructions
        .iter()
        .map(|path| {
            let file = files.get(path).ok_or(WorkspaceError::Invalid(
                "selected project instruction missing",
            ))?;
            Ok(MeasuredInput::from_bytes(
                path,
                file.executable,
                &file.bytes,
            )?)
        })
        .collect::<Result<Vec<_>, WorkspaceError>>()?;
    let mut inputs = SessionInputs::resolve(store, policy, registry, selection.agent_id)?;
    inputs.runtime_configuration_digest = Some(configuration.to_string());
    inputs.project_instructions = Some(instructions);
    // resolve() above accepts only the two fixed, extension-free integrations.
    // Unknown integrations or overrides never get these explicit empty inputs.
    inputs.tool_schemas = Some(vec![]);
    inputs.plugin_schemas = Some(vec![]);
    inputs.acp_mcp_servers = Some(vec![]);
    inputs.source_snapshot_digest = Some(source.snapshot_digest);
    inputs.source_base_digest = Some(source.base_digest);
    inputs.cache_base_digest = Some(cache.digest().to_string());
    inputs.isolation = Some(IsolationIntent::new(envelope.network));
    inputs.envelope_id = Some(envelope.id);
    inputs.envelope_revision = Some(1);
    let manifest = SessionInputManifest::build(inputs)?;
    let proposal = RunProposal {
        schema: PROPOSAL_SCHEMA.into(),
        run_id: fresh_run_id()?,
        envelope: manifest.envelope.clone(),
        manifest_digest: manifest.digest().to_string(),
        base_commit: source.base_commit,
    };
    let inputs = LoadedInputs {
        manifest,
        cache,
        record,
        files,
    };
    filesystem::publish(output, |staging| {
        inputs.write(staging)?;
        filesystem::write_file(
            &staging.join("proposal.json"),
            &serde_json::to_vec(&proposal)?,
            0o400,
        )
    })?;
    Ok(proposal)
}

/// Checks a stored inactive proposal against its fully remeasured input tree.
/// Does not approve the proposed Run, establish freshness, or imply enforcement.
///
/// # Errors
/// Refuses malformed, noncanonical, substituted or missing proposal/input bytes.
pub fn inspect_proposal(input: &Path, expected: &Digest) -> Result<RunProposal, WorkspaceError> {
    let inputs = super::load(input, expected)?;
    let root = filesystem::open_directory(input)?;
    let bytes = filesystem::read_source(&root, "proposal.json", 4096)?
        .ok_or(WorkspaceError::Invalid("Run proposal missing"))?
        .bytes;
    let proposal: RunProposal = serde_json::from_slice(&bytes)?;
    if proposal.schema != PROPOSAL_SCHEMA
        || proposal.envelope != inputs.manifest.envelope
        || proposal.envelope.revision != 1
        || proposal.manifest_digest != expected.to_string()
        || proposal.base_commit != inputs.record.base_commit
        || !valid_run_id(&proposal.run_id)
        || serde_json::to_vec(&proposal)? != bytes
    {
        return Err(WorkspaceError::Invalid("Run proposal binding mismatch"));
    }
    Ok(proposal)
}

fn fresh_run_id() -> Result<String, WorkspaceError> {
    let mut bytes = [0_u8; 16];
    if rustix::rand::getrandom(&mut bytes, rustix::rand::GetRandomFlags::empty())
        .map_err(std::io::Error::from)?
        != bytes.len()
    {
        return Err(WorkspaceError::Invalid("Run identity unavailable"));
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex = bytes.map(|byte| format!("{byte:02x}")).join("");
    Ok(format!(
        "{}-{}-{}-{}-{}",
        &hex[..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..]
    ))
}

fn valid_run_id(id: &str) -> bool {
    id.len() == 36
        && id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_digit() || (b'a'..=b'f').contains(&b)
            }
        })
        && id.as_bytes()[14] == b'4'
        && matches!(id.as_bytes()[19], b'8' | b'9' | b'a' | b'b')
}
