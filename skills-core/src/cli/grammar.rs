//! Declarative command tree for the trusted skill tool.

use super::Options;
use clap::{Arg, Args, Command};

fn subject(command: Command, multiple: bool) -> Command {
    command.arg(Arg::new("subjects").required(true).num_args(if multiple {
        clap::builder::ValueRange::from(1..)
    } else {
        clap::builder::ValueRange::from(1)
    }))
}

fn leaf(name: &'static str) -> Command {
    Command::new(name)
}

fn operation(name: &str, verb: &'static str) -> Command {
    let mut command = Options::augment_args(leaf(verb));
    let required: &[&str] = match (name, verb) {
        ("trust", "bootstrap") => &["primary", "release_key"],
        ("trust", "reset") => &["confirm"],
        ("generation", "admit") => &["members", "key"],
        ("generation", "witness") => &["remote"],
        ("view", "materialize") => &["registry"],
        ("quarantine", "all" | "exclude") => &["reason"],
        ("release", "build") => &["output"],
        ("release", "sign") => &["bundle", "key"],
        ("release", "verify" | "install") => &["bundle"],
        ("launcher", "install") => &[
            "operator",
            "broker_uid",
            "broker_gid",
            "uid_start",
            "gid_start",
            "slots",
        ],
        ("launcher", "rotate-key") => &["rotation_id", "expected_key_id"],
        ("launcher", "revoke-key" | "cleanup-key") => &["expected_key_id"],
        _ => &[],
    };
    for &flag in required {
        command = command.mut_arg(flag, |argument| argument.required(true));
    }
    command
}

pub(super) fn command() -> Command {
    let mut root = Command::new("louiselm-skills")
        .about("Package, inspect and admit trusted skills; manage installed authority")
        .after_help("Exit 0: admissible success. Exit 1: command or operation failed. Exit 2: completed, subject not admissible.");
    for name in ["package", "verify", "inspect", "dossier"] {
        root = root.subcommand(subject(Options::augment_args(leaf(name)), false));
    }
    for name in ["list", "policy"] {
        root = root.subcommand(Options::augment_args(leaf(name)));
    }
    for (name, verbs) in [
        ("trust", &["bootstrap", "show", "reset"][..]),
        (
            "generation",
            &["admit", "witness", "activate", "status", "list"],
        ),
        ("view", &["materialize", "empty"]),
        ("quarantine", &["exclude", "all", "show", "clear"]),
        (
            "release",
            &["build", "sign", "verify", "install", "status", "identity"],
        ),
        (
            "launcher",
            &[
                "install",
                "rotate-key",
                "revoke-key",
                "cleanup-key",
                "status",
            ],
        ),
    ] {
        let mut group = leaf(name).subcommand_required(true);
        for &verb in verbs {
            let command = operation(name, verb);
            let command = if name == "generation" && matches!(verb, "witness" | "activate") {
                subject(command, false)
            } else if name == "quarantine" && verb == "exclude" {
                subject(command, true)
            } else {
                command
            };
            group = group.subcommand(command);
        }
        root = root.subcommand(group);
    }
    root.subcommand(super::preflight::command())
        .subcommand(super::workspace::command())
        .subcommand(super::recovery::command())
}
