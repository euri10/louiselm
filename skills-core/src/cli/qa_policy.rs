//! Explicit administrator presentation for the reviewed exclusive QA window.
use super::{CliError, Digest, LauncherPaths, release, robot};
use crate::{conformance::admission::Enforcement, launcher_install::qa_policy};
use std::time::{Duration, Instant};

pub(super) fn command() -> clap::Command {
    clap::Command::new("qa-policy")
        .about("Explicit conformance-only change with every identity idle; never certifies or auto-restores")
        .args(["--reviewed-policy", "--to"].map(super::workspace::value))
        .mut_arg("--to", |arg| arg.value_parser(["pre_cutover", "enforced"]))
        .arg(clap::Arg::new("exclusive").long("exclusive-no-contained-sessions").action(clap::ArgAction::SetTrue).required(true))
        .arg(super::workspace::robot_flag())
}

pub(super) fn run(args: &clap::ArgMatches) -> Result<i32, CliError> {
    let invalid = || {
        CliError::Invalid(
            "QA policy requires the installed operator's explicit private sudo command".into(),
        )
    };
    let uid = std::env::var("SUDO_UID").map_err(|_| invalid())?;
    let operator: u32 = uid.parse().map_err(|_| invalid())?;
    if !rustix::process::geteuid().is_root()
        || operator == 0
        || operator.to_string() != uid
        || !args.get_flag("exclusive")
    {
        return Err(invalid());
    }
    let running = release::running_identity();
    let release_id = running
        .release_id
        .filter(|_| running.verified)
        .ok_or_else(invalid)?;
    let expected = Digest::parse(
        args.get_one::<String>("--reviewed-policy")
            .ok_or_else(invalid)?,
    )?;
    let target = match args.get_one::<String>("--to").map(String::as_str) {
        Some("enforced") => Enforcement::Enforced,
        Some("pre_cutover") => Enforcement::PreCutover,
        _ => return Err(invalid()),
    };
    let outcome = qa_policy::apply(
        &LauncherPaths::system(),
        &expected,
        operator,
        &release_id,
        target,
        Instant::now() + Duration::from_secs(5),
    )?;
    println!("{}", robot::payload(&outcome)?);
    Ok(0)
}
