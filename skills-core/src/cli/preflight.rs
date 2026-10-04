//! Closed, payload-safe artifact preflight command; no authority mutations.

use std::{
    fs::File,
    io::Read as _,
    path::{Path, PathBuf},
};

use clap::{CommandFactory, FromArgMatches, Parser};
use serde::Serialize;

use super::{CliError, EXIT_NOT_ADMISSIBLE, default_store_root};
use crate::{
    Policy, Store,
    launch::{LaunchRequest, MAX_REQUEST_BYTES},
    preflight::{self, DIRECT_LAUNCH_NOTICE},
    registry::Registry,
    robot,
    session_manifest::{MAX_INPUT_MANIFEST_BYTES, SessionInputManifest},
};

#[derive(Parser)]
#[command(
    name = "preflight",
    about = "Inspect prospective artifacts without launch or approval authority",
    after_help = "Inputs must be canonical launch request/2 and Session input-manifest/2 bytes. Exit 2: snapshot produced, enforcement unproven. Exit 1: invalid command/input."
)]
struct Options {
    #[arg(long, required_unless_present = "direct")]
    request: Option<PathBuf>,
    #[arg(long)]
    manifest: Option<PathBuf>,
    #[arg(long, requires = "previous_manifest")]
    previous_request: Option<PathBuf>,
    #[arg(long, requires = "previous_request")]
    previous_manifest: Option<PathBuf>,
    #[arg(long)]
    registry: Option<PathBuf>,
    #[arg(long)]
    store: Option<PathBuf>,
    #[arg(long = "robot-json")]
    robot: bool,
    #[arg(long, conflicts_with_all = ["request", "manifest", "previous_request", "previous_manifest", "registry", "store"])]
    direct: bool,
}

pub(super) fn command() -> clap::Command {
    Options::command()
}

fn invalid(message: &str) -> CliError {
    CliError::Invalid(message.to_owned())
}

pub(super) fn run(matches: &clap::ArgMatches) -> Result<i32, CliError> {
    let options =
        Options::from_arg_matches(matches).map_err(|_| invalid("invalid preflight options"))?;
    if options.direct {
        #[derive(Serialize)]
        struct Direct {
            schema: &'static str,
            notice: &'static str,
        }
        if options.robot {
            println!(
                "{}",
                robot::payload(&Direct {
                    schema: "louiselm.launch.direct/1",
                    notice: DIRECT_LAUNCH_NOTICE
                })?
            );
        } else {
            println!("{DIRECT_LAUNCH_NOTICE}");
        }
        return Ok(EXIT_NOT_ADMISSIBLE);
    }
    let request = read_request(
        options
            .request
            .as_deref()
            .ok_or_else(|| invalid("missing preflight request"))?,
    )?;
    let manifest = options.manifest.as_deref().map(read_manifest).transpose()?;
    let prior_request = options
        .previous_request
        .as_deref()
        .map(read_request)
        .transpose()?;
    let prior_manifest = options
        .previous_manifest
        .as_deref()
        .map(read_manifest)
        .transpose()?;
    let registry_root = options.registry.or_else(default_registry_root);
    // Unavailable or untrusted readers become missing trusted evidence in the
    // normalized record. Raw filesystem diagnostics must not reach presentation.
    let registry = registry_root
        .as_deref()
        .and_then(|path| Registry::open_trusted(path).ok());
    let store_root = options.store.or_else(|| default_store_root().ok());
    let store = store_root
        .as_deref()
        .filter(|path| path.is_dir())
        .and_then(|path| Store::open(path).ok());
    let preview = preflight::inspect(
        &request,
        manifest.as_ref(),
        store.as_ref(),
        &Policy::embedded(),
        registry.as_ref(),
        prior_request.as_ref().zip(prior_manifest.as_ref()),
    )
    .map_err(|error| invalid(&error.to_string()))?;
    if options.robot {
        println!("{}", robot::payload(&preview)?);
    } else {
        print!("{}", preflight::render(&preview));
    }
    Ok(EXIT_NOT_ADMISSIBLE)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "Non-Linux builds have no installed launcher registry default."
)]
fn default_registry_root() -> Option<PathBuf> {
    #[cfg(target_os = "linux")]
    {
        Some(PathBuf::from(
            crate::launch_supervisor::SYSTEM_REGISTRY_ROOT,
        ))
    }
    #[cfg(not(target_os = "linux"))]
    {
        None
    }
}

fn read_bounded(path: &Path, limit: usize) -> Result<Vec<u8>, CliError> {
    if !path.is_file() {
        return Err(invalid("preflight input must be a readable regular file"));
    }
    let file = File::open(path).map_err(|_| invalid("cannot read preflight input"))?;
    let mut bytes = Vec::new();
    file.take(limit as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| invalid("cannot read preflight input"))?;
    if bytes.len() > limit {
        return Err(invalid("preflight input exceeds size limit"));
    }
    Ok(bytes)
}

fn read_request(path: &Path) -> Result<LaunchRequest, CliError> {
    LaunchRequest::parse_canonical(&read_bounded(path, MAX_REQUEST_BYTES)?)
        .map_err(|_| invalid("invalid canonical preflight request"))
}

fn read_manifest(path: &Path) -> Result<SessionInputManifest, CliError> {
    SessionInputManifest::parse(&read_bounded(path, MAX_INPUT_MANIFEST_BYTES)?)
        .map_err(|_| invalid("invalid canonical preflight input manifest"))
}
