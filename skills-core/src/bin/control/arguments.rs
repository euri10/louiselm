//! Closed operator command tree; machine refusal presentation stays in handlers.

use clap::{Arg, ArgAction, Command};

fn json() -> Arg {
    Arg::new("json")
        .long("json")
        .action(ArgAction::SetTrue)
        .required(true)
}

fn subject() -> Arg {
    Arg::new("subject")
        .required(true)
        .value_parser(clap::builder::NonEmptyStringValueParser::new())
}

fn group(name: &'static str, verbs: &[&'static str], has_subject: bool) -> Command {
    let mut group = Command::new(name).subcommand_required(true);
    for &verb in verbs {
        let mut leaf = Command::new(verb).arg(json());
        if has_subject {
            leaf = leaf.arg(subject());
        }
        group = group.subcommand(leaf);
    }
    group
}

pub(super) fn command() -> Command {
    let mut dependencies = group("dependencies", &["inspect"], true);
    dependencies = dependencies.subcommand(
        Command::new("approve")
            .arg(subject())
            .arg(json())
            .arg(Arg::new("candidates").required(true).num_args(1..=32)),
    );
    let mut waiver = group("waiver", &["inspect", "plan"], true);
    for verb in ["apply", "result", "revoke"] {
        waiver = waiver.subcommand(
            Command::new(verb)
                .arg(subject())
                .arg(Arg::new("digest").required(true))
                .arg(json()),
        );
    }
    Command::new("louiselm-control")
        .about("Run the installed Control broker or request authenticated operator operations")
        .subcommand_required(true)
        .subcommand(Command::new("serve"))
        .subcommand(
            Command::new("adopt-state").arg(
                Arg::new("confirm")
                    .long("confirm")
                    .action(ArgAction::SetTrue)
                    .required(true),
            ),
        )
        .subcommand(group(
            "session",
            &["inspect", "conformance", "retention", "pin", "unpin"],
            true,
        ))
        .subcommand(group("beads", &["inspect"], true))
        .subcommand(group(
            "skill-request",
            &["inspect", "reject", "cancel"],
            true,
        ))
        .subcommand(group("run", &["authorize"], false))
        .subcommand(group("launch-inputs", &["stage"], false))
        .subcommand(Command::new("verification").arg(json()))
        .subcommand(Command::new("lifecycle").arg(json()))
        .subcommand(group("promotion", &["preview", "commit"], false))
        .subcommand(dependencies)
        .subcommand(waiver)
        .subcommand(
            Command::new("provider-extend")
                .arg(subject())
                .arg(json())
                .arg(Arg::new("request_id").required(true))
                .arg(
                    Arg::new("requests")
                        .required(true)
                        .value_parser(clap::value_parser!(u32)),
                )
                .arg(Arg::new("expires_at_ms").value_parser(clap::value_parser!(u64))),
        )
}

pub(super) fn operation(
    input: Result<&clap::ArgMatches, ()>,
) -> Result<(&str, &clap::ArgMatches), ()> {
    input?.subcommand().ok_or(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn lifecycle_accepts_only_the_json_entrypoint() {
        assert!(
            super::command()
                .try_get_matches_from(["control", "lifecycle", "--json"])
                .is_ok()
        );
        for args in [
            vec!["control", "lifecycle"],
            vec!["control", "lifecycle", "--json", "--uid", "0"],
            vec!["control", "lifecycle", "--json", "session"],
        ] {
            assert!(super::command().try_get_matches_from(args).is_err());
        }
    }
}
