//! Command presentation for the owner-measured inactive runtime proposal.
use super::{CliError, Path, invalid, robot};

pub(super) fn command() -> clap::Command {
    clap::Command::new("qa-runtime")
        .about(
            "Produce inactive exact deterministic runtime metadata; no registration or signature",
        )
        .subcommand_required(true)
        .subcommand(clap::Command::new("inspect-installation").arg(super::robot_flag()))
        .subcommand(
            clap::Command::new("measure-cache")
                .arg(super::value("--cache"))
                .arg(super::robot_flag()),
        )
        .subcommand(
            clap::Command::new("prepare")
                .args(["--bundle", "--output"].map(super::value))
                .arg(super::robot_flag()),
        )
}

pub(super) fn run(args: &clap::ArgMatches) -> Result<i32, CliError> {
    let (verb, args) = args
        .subcommand()
        .ok_or_else(|| invalid("QA runtime requires prepare"))?;
    if verb == "inspect-installation" {
        let config = crate::launcher_install::public_runtime_config(
            &crate::launcher_install::LauncherPaths::system(),
        )
        .map_err(|_| invalid("public installed authority unavailable"))?;
        let value = serde_json::json!({
            "release_id": config.release_id,
            "conformance": config.conformance,
            "operator_uid": config.operator_uid,
            "policy_digest": crate::launcher_install::qa_policy::policy_digest(&config)?.to_string(),
        });
        println!("{}", robot::payload(&value)?);
        return Ok(0);
    }
    if verb == "measure-cache" {
        let path = args
            .get_one::<String>("--cache")
            .ok_or_else(|| invalid("QA cache measurement requires --cache"))?;
        let cache = crate::cache::CacheBase::capture(Path::new(path))
            .map_err(|_| invalid("QA cache measurement refused"))?;
        println!(
            "{}",
            robot::payload(&serde_json::json!({"cache_digest": cache.digest().to_string()}))?
        );
        return Ok(0);
    }
    let required = |flag| {
        args.get_one::<String>(flag)
            .ok_or_else(|| invalid("missing QA preparation option"))
    };
    let proposal = crate::workspace::qa_runtime::prepare(
        Path::new(required("--bundle")?),
        Path::new(required("--output")?),
    )
    .map_err(|error| invalid(&error.to_string()))?;
    if args.get_flag("robot") {
        println!("{}", robot::payload(&proposal)?);
    } else {
        println!(
            "Inactive QA runtime {} for release {}; review and explicitly publish its configuration before resolving inputs.",
            proposal.runtime.id, proposal.release_id
        );
    }
    Ok(0)
}
