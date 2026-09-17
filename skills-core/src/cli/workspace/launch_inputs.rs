//! Local staging/preview; broker launch authorization stays separate.

use super::{CliError, Digest, Path, invalid, robot, workspace};
use crate::session_manifest::{MAX_INPUT_MANIFEST_BYTES, SessionInputManifest};
use std::{collections::BTreeMap, io::Read as _};

pub(super) fn run(args: &[String]) -> Result<i32, CliError> {
    if args == ["--help"] {
        println!(
            "louiselm-skills workspace launch-inputs stage --manifest FILE --snapshot DIR\n  --cache DIR --output NEW_DIR [--robot-json]\nlouiselm-skills workspace launch-inputs inspect --input DIR --digest SHA256 [--robot-json]\nThe manifest must bind exact source snapshot/base and cache digests.\nInspect included/excluded paths before authorizing launch. Local staging grants no authority."
        );
        return Ok(0);
    }
    let Some(operation @ ("stage" | "inspect")) = args.first().map(String::as_str) else {
        return Err(invalid("launch-inputs requires stage or inspect"));
    };
    let mut values = BTreeMap::new();
    let mut robot = false;
    let mut args = args[1..].iter();
    while let Some(flag) = args.next() {
        if flag == "--robot-json" && !robot {
            robot = true;
            continue;
        }
        let allowed = if operation == "stage" {
            ["--manifest", "--snapshot", "--cache", "--output"].contains(&flag.as_str())
        } else {
            ["--input", "--digest"].contains(&flag.as_str())
        };
        if !allowed {
            return Err(invalid("invalid or duplicate launch-inputs option"));
        }
        let value = args
            .next()
            .filter(|v| !v.is_empty() && !v.starts_with("--"))
            .ok_or_else(|| invalid("launch-inputs option requires a value"))?;
        if values.insert(flag.as_str(), value.as_str()).is_some() {
            return Err(invalid("duplicate launch-inputs option"));
        }
    }
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
