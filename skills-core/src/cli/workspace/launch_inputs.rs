//! Local staging/preview; broker launch authorization stays separate.

use super::{CliError, Digest, Path, invalid, robot, workspace};
use crate::session_manifest::{MAX_INPUT_MANIFEST_BYTES, SessionInputManifest};
use std::{collections::BTreeMap, io::Read as _};

pub(super) fn command() -> clap::Command {
    clap::Command::new("launch-inputs")
        .about("Stage exact source/cache inputs; launch authorization stays separate")
        .subcommand_required(true)
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
}

pub(super) fn run(args: &clap::ArgMatches) -> Result<i32, CliError> {
    let (operation, args) = args
        .subcommand()
        .ok_or_else(|| invalid("launch-inputs requires stage or inspect"))?;
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
