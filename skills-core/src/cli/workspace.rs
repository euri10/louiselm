//! Closed command parser for local snapshots and untrusted byte bundles.

use std::{collections::BTreeMap, path::Path};

use super::CliError;
use crate::{Digest, robot, workspace};

mod launch_inputs;
mod verification;

pub(super) fn command() -> clap::Command {
    let mut root = clap::Command::new("workspace")
        .about("Freeze source and prepare local bytes; no launch or promotion authority")
        .subcommand_required(true);
    for (name, flags) in [
        ("prepare", &["--repository", "--output"][..]),
        ("materialize", &["--snapshot", "--digest", "--output"]),
        (
            "export",
            &["--snapshot", "--digest", "--workspace", "--output"],
        ),
        (
            "apply",
            &[
                "--snapshot",
                "--digest",
                "--bundle",
                "--bundle-digest",
                "--output",
            ],
        ),
    ] {
        let mut command = clap::Command::new(name)
            .args(flags.iter().map(|&flag| value(flag)))
            .arg(robot_flag());
        if name == "prepare" {
            command = command.arg(
                value("--include")
                    .required(false)
                    .action(clap::ArgAction::Append),
            );
        }
        root = root.subcommand(command);
    }
    root.subcommand(launch_inputs::command())
        .subcommand(verification::command())
}

pub(super) fn value(name: &'static str) -> clap::Arg {
    clap::Arg::new(name)
        .long(&name[2..])
        .required(true)
        .value_parser(clap::builder::NonEmptyStringValueParser::new())
}

pub(super) fn robot_flag() -> clap::Arg {
    clap::Arg::new("robot")
        .long("robot-json")
        .action(clap::ArgAction::SetTrue)
}

pub(super) fn run(args: &clap::ArgMatches) -> Result<i32, CliError> {
    let (operation, args) = args
        .subcommand()
        .ok_or_else(|| invalid("workspace requires a subcommand"))?;
    match operation {
        "launch-inputs" => return launch_inputs::run(args),
        "verification" => return verification::run(args),
        _ => (),
    }
    let robot = args.get_flag("robot");
    let included = if operation == "prepare" {
        args.get_many::<String>("--include")
            .map(|values| values.cloned().collect())
            .unwrap_or_default()
    } else {
        Vec::new()
    };
    let values: BTreeMap<_, _> = args
        .ids()
        .filter(|id| !matches!(id.as_str(), "robot" | "--include"))
        .filter_map(|id| {
            args.get_one::<String>(id.as_str())
                .map(|value| (id.as_str(), value.as_str()))
        })
        .collect();
    if operation == "export" || operation == "apply" {
        return run_bundle(operation, &values, robot);
    }
    let required = |flag| {
        values
            .get(flag)
            .copied()
            .ok_or_else(|| invalid("missing required workspace option"))
    };
    let output = Path::new(required("--output")?);
    let preview = if operation == "prepare" {
        workspace::prepare(Path::new(required("--repository")?), &included, output)
    } else {
        let digest = Digest::parse(required("--digest")?)
            .map_err(|_| invalid("invalid workspace digest"))?;
        workspace::materialize(Path::new(required("--snapshot")?), &digest, output)
    }
    .map_err(|error| invalid(&error.to_string()))?;
    if robot {
        println!("{}", robot::payload(&preview)?);
    } else {
        println!(
            "Source snapshot: {}\nBase commit: {}\nBase digest: {}\nFiles: {} ({} bytes)\nLocal source bytes; no Verified launch authority.",
            preview.snapshot_digest,
            preview.base_commit,
            preview.base_digest,
            preview.file_count,
            preview.total_bytes
        );
        for change in preview.changes {
            let decision = if change.included {
                "include"
            } else {
                "exclude"
            };
            println!("{decision}: {:?} {}", change.kind, change.path);
        }
    }
    Ok(0)
}

fn run_bundle(
    operation: &str,
    values: &BTreeMap<&str, &str>,
    robot: bool,
) -> Result<i32, CliError> {
    let required = |flag| {
        values
            .get(flag)
            .copied()
            .ok_or_else(|| invalid("missing required workspace option"))
    };
    let snapshot = Path::new(required("--snapshot")?);
    let digest =
        Digest::parse(required("--digest")?).map_err(|_| invalid("invalid workspace digest"))?;
    let output = Path::new(required("--output")?);
    let preview = if operation == "export" {
        workspace::bundle::export(
            snapshot,
            &digest,
            Path::new(required("--workspace")?),
            output,
        )
    } else {
        let bundle_digest = Digest::parse(required("--bundle-digest")?)
            .map_err(|_| invalid("invalid bundle digest"))?;
        workspace::bundle::apply(
            snapshot,
            &digest,
            Path::new(required("--bundle")?),
            &bundle_digest,
            output,
        )
    }
    .map_err(|error| invalid(&error.to_string()))?;
    if robot {
        println!("{}", robot::payload(&preview)?);
    } else {
        println!(
            "Bundle: {}\nBase digest: {}\nResult digest: {}\nFiles: {} ({} bytes)\nUntrusted source bytes; separate verification and promotion required.",
            preview.bundle_digest,
            preview.base_digest,
            preview.result_digest,
            preview.file_count,
            preview.total_bytes
        );
        for (kind, paths) in [
            ("added", preview.added),
            ("modified", preview.modified),
            ("deleted", preview.deleted),
        ] {
            for path in paths {
                println!("{kind}: {path}");
            }
        }
    }
    Ok(0)
}

fn invalid(message: &str) -> CliError {
    CliError::Invalid(message.to_owned())
}
