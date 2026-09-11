//! Closed command parser for local snapshots and untrusted byte bundles.

use std::{collections::BTreeMap, path::Path};

use super::CliError;
use crate::{Digest, robot, workspace};

mod verification;

pub(super) fn run(args: &[String]) -> Result<i32, CliError> {
    if args.first().is_some_and(|arg| arg == "verification") {
        return verification::run(&args[1..]);
    }
    if args == ["--help"] {
        println!("Verification inputs: louiselm-skills workspace verification --help");
        println!(
            "louiselm-skills workspace prepare --repository DIR --output NEW_DIR\n  [--include RELATIVE_FILE ...] [--robot-json]\nlouiselm-skills workspace materialize --snapshot DIR --digest SHA256\n  --output NEW_DIR [--robot-json]\nlouiselm-skills workspace export --snapshot DIR --digest SHA256\n  --workspace DIR --output NEW_DIR [--robot-json]\nlouiselm-skills workspace apply --snapshot DIR --digest SHA256\n  --bundle DIR --bundle-digest SHA256 --output NEW_DIR [--robot-json]\nPrepare freezes HEAD plus explicitly selected working-copy files.\nExport compares actual workspace bytes; apply writes a fresh integration tree.\nInspect preview digests before use. Local bytes only; no launch or promotion authority.\nExit 0: complete. Exit 1: refused or failed; inspect output after persistence errors."
        );
        return Ok(0);
    }
    let Some(operation @ ("prepare" | "materialize" | "export" | "apply")) =
        args.first().map(String::as_str)
    else {
        return Err(invalid(
            "workspace requires prepare, materialize, export or apply",
        ));
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
            "export" => {
                ["--snapshot", "--digest", "--workspace", "--output"].contains(&flag.as_str())
            }
            "apply" => [
                "--snapshot",
                "--digest",
                "--bundle",
                "--bundle-digest",
                "--output",
            ]
            .contains(&flag.as_str()),
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
