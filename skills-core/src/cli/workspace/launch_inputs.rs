//! Local staging/preview; broker launch authorization stays separate.

use super::{CliError, Digest, Path, invalid, robot, workspace};
use crate::session_manifest::{MAX_INPUT_MANIFEST_BYTES, SessionInputManifest};
use std::{collections::BTreeMap, io::Read as _};

pub(super) fn command() -> clap::Command {
    clap::Command::new("launch-inputs")
        .about("Stage exact source/cache inputs; launch authorization stays separate")
        .subcommand_required(true)
        .subcommand(
            clap::Command::new("prepare")
                .about("Measure selected inputs and write an inactive fresh Run proposal")
                .args(
                    [
                        "--store",
                        "--registry",
                        "--agent",
                        "--envelope",
                        "--snapshot",
                        "--snapshot-digest",
                        "--cache",
                        "--cache-digest",
                        "--instructions",
                        "--output",
                    ]
                    .map(super::value),
                )
                .arg(super::robot_flag()),
        )
        .subcommand(
            clap::Command::new("stage")
                .args(["--manifest", "--snapshot", "--cache", "--output"].map(super::value))
                .arg(super::robot_flag()),
        )
        .subcommand(
            clap::Command::new("inspect")
                .args(["--input", "--digest"].map(super::value))
                .arg(super::robot_flag()),
        )
        .subcommand(
            clap::Command::new("inspect-proposal")
                .args(["--input", "--digest"].map(super::value))
                .arg(super::robot_flag()),
        )
}

pub(super) fn run(args: &clap::ArgMatches) -> Result<i32, CliError> {
    let (operation, args) = args.subcommand().ok_or_else(|| {
        invalid("launch-inputs requires prepare, stage, inspect or inspect-proposal")
    })?;
    let robot = args.get_flag("robot");
    let values: BTreeMap<_, _> = args
        .ids()
        .filter(|id| id.as_str() != "robot")
        .filter_map(|id| {
            args.get_one::<String>(id.as_str())
                .map(|value| (id.as_str(), value.as_str()))
        })
        .collect();
    let required = |flag| {
        values
            .get(flag)
            .copied()
            .ok_or_else(|| invalid("missing required launch-inputs option"))
    };
    if operation == "prepare" {
        return prepare(&values, robot);
    }
    if operation == "inspect-proposal" {
        let expected = Digest::parse(required("--digest")?)
            .map_err(|_| invalid("invalid launch input digest"))?;
        let proposal =
            workspace::launch_inputs::inspect_proposal(Path::new(required("--input")?), &expected)
                .map_err(|error| invalid(&error.to_string()))?;
        return print_proposal(&proposal, robot);
    }
    let preview = if operation == "stage" {
        let mut bytes = Vec::new();
        std::fs::File::open(required("--manifest")?)
            .and_then(|file| {
                file.take(MAX_INPUT_MANIFEST_BYTES as u64 + 1)
                    .read_to_end(&mut bytes)
            })
            .map_err(|_| invalid("Session input manifest unavailable"))?;
        let manifest = SessionInputManifest::parse(&bytes)
            .map_err(|_| invalid("invalid Session input manifest"))?;
        workspace::launch_inputs::stage(
            &manifest,
            Path::new(required("--snapshot")?),
            Path::new(required("--cache")?),
            Path::new(required("--output")?),
        )
    } else {
        let expected = Digest::parse(required("--digest")?)
            .map_err(|_| invalid("invalid launch input digest"))?;
        workspace::launch_inputs::inspect(Path::new(required("--input")?), &expected)
    }
    .map_err(|error| invalid(&error.to_string()))?;
    if robot {
        println!("{}", robot::payload(&preview)?);
    } else {
        println!(
            "Manifest: {}\nSource snapshot: {}\nSource base: {}\nCache base: {}\nStaged inputs; launch authorization required.",
            preview.manifest_digest,
            preview.source.snapshot_digest,
            preview.source.base_digest,
            preview.cache_base_digest
        );
        for change in preview.source.changes {
            println!(
                "{}: {:?} {}",
                if change.included {
                    "include"
                } else {
                    "exclude"
                },
                change.kind,
                change.path
            );
        }
    }
    Ok(0)
}

fn prepare(values: &BTreeMap<&str, &str>, robot: bool) -> Result<i32, CliError> {
    let required = |flag| {
        values
            .get(flag)
            .copied()
            .ok_or_else(|| invalid("missing required preparation option"))
    };
    let store_path = Path::new(required("--store")?);
    let store = crate::Store::open_existing(store_path)
        .map_err(|_| invalid("preparation requires an existing supply store"))?;
    let registry = crate::registry::Registry::open_trusted(Path::new(required("--registry")?))
        .map_err(|_| invalid("preparation requires a trusted registry"))?;
    let snapshot_digest = Digest::parse(required("--snapshot-digest")?)
        .map_err(|_| invalid("invalid selected snapshot digest"))?;
    let cache_digest = Digest::parse(required("--cache-digest")?)
        .map_err(|_| invalid("invalid selected cache digest"))?;
    let mut bytes = Vec::new();
    std::fs::File::open(required("--instructions")?)
        .and_then(|file| {
            file.take(MAX_INPUT_MANIFEST_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
        })
        .map_err(|_| invalid("project instruction selection unavailable"))?;
    if bytes.len() > MAX_INPUT_MANIFEST_BYTES {
        return Err(invalid("project instruction selection too large"));
    }
    let instructions: Vec<String> = serde_json::from_slice(&bytes).map_err(|_| {
        invalid("project instructions must be an explicit JSON array of snapshot paths")
    })?;
    let proposal = workspace::launch_inputs::prepare(
        &store,
        &crate::Policy::embedded(),
        &registry,
        &workspace::launch_inputs::Preparation {
            agent_id: required("--agent")?,
            envelope_id: required("--envelope")?,
            snapshot: Path::new(required("--snapshot")?),
            snapshot_digest: &snapshot_digest,
            cache: Path::new(required("--cache")?),
            cache_digest: &cache_digest,
            project_instructions: &instructions,
        },
        Path::new(required("--output")?),
    )
    .map_err(|error| invalid(&error.to_string()))?;
    print_proposal(&proposal, robot)
}

fn print_proposal(
    proposal: &workspace::launch_inputs::RunProposal,
    robot: bool,
) -> Result<i32, CliError> {
    if robot {
        println!("{}", robot::payload(proposal)?);
    } else {
        println!(
            "Inactive Run proposal: {}\nProposed envelope: {} revision {}\nManifest: {}\nBase commit: {}\nSeparate finite Run approval required; nothing launched.",
            proposal.run_id,
            proposal.envelope.id,
            proposal.envelope.revision,
            proposal.manifest_digest,
            proposal.base_commit
        );
    }
    Ok(0)
}
