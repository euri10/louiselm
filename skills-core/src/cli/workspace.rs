//! Closed command parser for explicit local source capture and materialization.

use std::{collections::BTreeMap, path::Path};

use super::CliError;
use crate::{Digest, robot, workspace};

pub(super) fn run(args: &[String]) -> Result<i32, CliError> {
    if args == ["--help"] {
        println!(
            "louiselm-skills workspace prepare --repository DIR --output NEW_DIR\n  [--include RELATIVE_FILE ...] [--robot-json]\nlouiselm-skills workspace materialize --snapshot DIR --digest SHA256\n  --output NEW_DIR [--robot-json]\nPrepare freezes HEAD plus explicitly selected working-copy files.\nInspect the preview digest before materialization. Local bytes only; no launch authority.\nExit 0: complete. Exit 1: refused or failed; inspect output after persistence errors."
        );
        return Ok(0);
    }
    let Some(operation @ ("prepare" | "materialize")) = args.first().map(String::as_str) else {
        return Err(invalid("workspace requires prepare or materialize"));
    };
    let mut values = BTreeMap::new();
    let mut included = Vec::new();
    let mut robot = false;
    let mut args = args[1..].iter();
    while let Some(flag) = args.next() {
        if flag == "--robot-json" && !robot {
            robot = true;
            continue;
        }
        let allowed = match operation {
            "prepare" => ["--repository", "--output", "--include"].contains(&flag.as_str()),
            _ => ["--snapshot", "--digest", "--output"].contains(&flag.as_str()),
        };
        if !allowed {
            return Err(invalid("invalid or duplicate workspace option"));
        }
        let value = args
            .next()
            .filter(|value| !value.is_empty() && !value.starts_with("--"))
            .ok_or_else(|| invalid("workspace option requires a value"))?;
        if flag == "--include" {
            included.push(value.clone());
        } else if values.insert(flag.as_str(), value.as_str()).is_some() {
            return Err(invalid("duplicate workspace option"));
        }
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

fn invalid(message: &str) -> CliError {
    CliError::Invalid(message.to_owned())
}
