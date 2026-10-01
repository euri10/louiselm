//! Closed operator CLI for preparation and remeasurement, never execution.

use std::{collections::BTreeMap, path::Path};

use super::{CliError, invalid};
use crate::{Digest, robot, workspace::verification};

pub(super) fn command() -> clap::Command {
    clap::Command::new("verification")
        .about("Prepare and inspect verification bytes; no command executes")
        .subcommand_required(true)
        .subcommand(
            clap::Command::new("prepare")
                .args(
                    [
                        "--snapshot",
                        "--digest",
                        "--bundle",
                        "--bundle-digest",
                        "--plan",
                        "--plan-digest",
                        "--output",
                    ]
                    .map(super::value),
                )
                .arg(super::robot_flag()),
        )
        .subcommand(
            clap::Command::new("inspect")
                .args(["--job", "--digest"].map(super::value))
                .arg(super::robot_flag()),
        )
}

pub(super) fn run(args: &clap::ArgMatches) -> Result<i32, CliError> {
    let (operation, args) = args
        .subcommand()
        .ok_or_else(|| invalid("verification requires prepare or inspect"))?;
    let robot = args.get_flag("robot");
    let options: BTreeMap<_, _> = args
        .ids()
        .filter(|id| id.as_str() != "robot")
        .filter_map(|id| {
            args.get_one::<String>(id.as_str())
                .map(|value| (id.as_str(), value.as_str()))
        })
        .collect();
    let required = |flag| {
        options
            .get(flag)
            .copied()
            .ok_or_else(|| invalid("missing required verification option"))
    };
    let digest =
        |flag| Digest::parse(required(flag)?).map_err(|_| invalid("invalid verification digest"));
    let preview = if operation == "prepare" {
        verification::prepare(
            Path::new(required("--snapshot")?),
            &digest("--digest")?,
            Path::new(required("--bundle")?),
            &digest("--bundle-digest")?,
            Path::new(required("--plan")?),
            &digest("--plan-digest")?,
            Path::new(required("--output")?),
        )
    } else {
        verification::inspect(Path::new(required("--job")?), &digest("--digest")?)
    }
    .map_err(|error| invalid(&error.to_string()))?;
    if robot {
        println!("{}", robot::payload(&preview)?);
    } else {
        println!(
            "Prepared job: {}\nSnapshot: {}\nBundle: {}\nBase: {}\nResult: {}\nPlan: {}\nCommands: {}\nPrepared bytes only; not verification or promotion authority.",
            preview.job_digest,
            preview.snapshot_digest,
            preview.bundle_digest,
            preview.base_digest,
            preview.result_digest,
            preview.plan_digest,
            preview.command_count
        );
    }
    Ok(0)
}
