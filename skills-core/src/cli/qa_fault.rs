//! Foreground administrator review/apply/restore; no automatic fault injection.
use super::{CliError, Digest, LauncherPaths, release, robot};
use crate::launcher_install::qa_fault;
use std::time::{SystemTime, UNIX_EPOCH};

pub(super) fn command() -> clap::Command {
    let mut command = clap::Command::new("qa-fault")
        .about("Review, invalidate or explicitly restore only a new retired QA release pin")
        .subcommand_required(true);
    for operation in ["plan", "apply", "restore"] {
        let mut leaf = clap::Command::new(operation)
            .arg(super::workspace::robot_flag())
            .arg(
                clap::Arg::new("exclusive")
                    .long("exclusive-qa-window")
                    .action(clap::ArgAction::SetTrue)
                    .required(true),
            );
        if operation != "restore" {
            leaf = leaf.args(
                ["--baseline", "--retired", "--run", "--session"].map(super::workspace::value),
            );
        }
        if operation != "plan" {
            leaf = leaf.arg(super::workspace::value("--reviewed"));
        }
        command = command.subcommand(leaf);
    }
    command
}

pub(super) fn run(args: &clap::ArgMatches) -> Result<i32, CliError> {
    let invalid = || {
        CliError::Invalid("QA fault requires exact review and the installed operator's explicit private sudo command; retain recovery records".into())
    };
    let uid = std::env::var("SUDO_UID").map_err(|_| invalid())?;
    let operator: u32 = uid.parse().map_err(|_| invalid())?;
    if !rustix::process::geteuid().is_root() || operator == 0 || operator.to_string() != uid {
        return Err(invalid());
    }
    let running = release::running_identity();
    let release_id = running
        .release_id
        .filter(|_| running.verified)
        .ok_or_else(invalid)?;
    let (operation, leaf) = args.subcommand().ok_or_else(invalid)?;
    let value = |key| {
        leaf.get_one::<String>(key)
            .map(String::as_str)
            .ok_or_else(invalid)
    };
    let paths = LauncherPaths::system();
    if operation == "restore" {
        let reviewed = Digest::parse(value("--reviewed")?)?;
        qa_fault::restore(&paths, operator, &release_id, &reviewed)?;
        println!(
            "{}",
            robot::payload(
                &serde_json::json!({"state":"restored_not_resumed", "reviewed":reviewed.to_string()})
            )?
        );
    } else {
        let now = u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_| invalid())?
                .as_millis(),
        )
        .map_err(|_| invalid())?;
        let plan = qa_fault::plan(
            &paths,
            operator,
            &release_id,
            [value("--baseline")?, value("--retired")?],
            value("--run")?,
            value("--session")?,
            now,
        )?;
        let reviewed = qa_fault::digest(&plan)?;
        if operation == "apply" {
            let selected = Digest::parse(value("--reviewed")?)?;
            qa_fault::apply(&paths, &plan, &selected, &release_id, now)?;
        }
        println!(
            "{}",
            robot::payload(
                &serde_json::json!({"state":if operation == "plan" {"proposed_no_effect"} else {"fault_recorded_no_resume"}, "reviewed":reviewed.to_string(), "plan":plan})
            )?
        );
    }
    Ok(0)
}
