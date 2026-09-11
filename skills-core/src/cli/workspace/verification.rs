//! Closed operator CLI for preparation and remeasurement, never execution.

use std::{collections::BTreeMap, path::Path};

use super::{CliError, invalid};
use crate::{Digest, robot, workspace::verification};

pub(super) fn run(args: &[String]) -> Result<i32, CliError> {
    if args == ["--help"] {
        println!(
            "louiselm-skills workspace verification prepare\n  --snapshot DIR --digest SHA256 --bundle DIR --bundle-digest SHA256\n  --plan FILE --plan-digest SHA256 --output NEW_DIR [--robot-json]\nlouiselm-skills workspace verification inspect\n  --job DIR --digest SHA256 [--robot-json]\nPrepared bytes only; not verification or promotion authority. No command executes."
        );
        return Ok(0);
    }
    let Some(operation @ ("prepare" | "inspect")) = args.first().map(String::as_str) else {
        return Err(invalid("verification requires prepare or inspect"));
    };
    let mut options = BTreeMap::new();
    let mut robot = false;
    let mut args = args[1..].iter();
    while let Some(flag) = args.next() {
        if flag == "--robot-json" && !robot {
            robot = true;
            continue;
        }
        let allowed = if operation == "prepare" {
            [
                "--snapshot",
                "--digest",
                "--bundle",
                "--bundle-digest",
                "--plan",
                "--plan-digest",
                "--output",
            ]
            .contains(&flag.as_str())
        } else {
            ["--job", "--digest"].contains(&flag.as_str())
        };
        if !allowed {
            return Err(invalid("invalid or duplicate verification option"));
        }
        let value = args
            .next()
            .filter(|value| !value.is_empty() && !value.starts_with("--"))
            .ok_or_else(|| invalid("verification option requires a value"))?;
        if options.insert(flag.as_str(), value.as_str()).is_some() {
            return Err(invalid("duplicate verification option"));
        }
    }
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
