//! Conservative signatures; unparsed shell programs retain exact private equality.

use crate::model::{Call, digest};

const EXECUTABLES: &[&str] = &[
    "git",
    "cargo",
    "br",
    "npm",
    "pnpm",
    "docker",
    "kubectl",
    "rg",
    "sed",
    "jq",
    "ls",
    "cat",
    "find",
    "head",
    "tail",
    "wc",
    "rustfmt",
    "nvim",
    "rtk",
    "sh",
    "bash",
    "zsh",
    "python3",
    "node",
    "go",
    "pytest",
    "louiselm-usage",
];
const SUBCOMMANDS: &[&str] = &[
    "status",
    "diff",
    "log",
    "show",
    "add",
    "commit",
    "test",
    "check",
    "build",
    "fmt",
    "clippy",
    "doc",
    "run",
    "list",
    "search",
    "ready",
    "update",
    "create",
    "close",
    "comments",
    "dep",
    "sync",
    "robot-docs",
    "stats",
    "index",
    "calls",
    "schema",
    "sources",
];

pub(crate) fn extract(call: &mut Call, command: &str) {
    call.argument_bytes = Some(command.len() as u64);
    call.command_key = Some(digest(command.as_bytes()));
    // A known unquoted leading executable is useful even when its arguments are
    // opaque. This identifies the program prefix, never additional shell children.
    let opaque = command.chars().any(|c| "'\"`$|;&<>\\\n(){}".contains(c));
    let mut parts = command.split_whitespace().peekable();
    if parts.peek() == Some(&"rtk") {
        parts.next();
        if parts.peek() == Some(&"proxy") {
            parts.next();
            call.wrapper = Some("rtk proxy".to_owned());
        } else {
            call.wrapper = Some("rtk".to_owned());
        }
    }
    let Some(executable) = parts.next() else {
        return;
    };
    if executable.contains('=') {
        call.family = Some("<opaque>".to_owned());
        call.normalization = Some("opaque".to_owned());
        return;
    }
    let executable = executable.rsplit('/').next().unwrap_or(executable);
    if !EXECUTABLES.contains(&executable) {
        call.family = Some("<unknown executable>".to_owned());
        call.normalization = Some("opaque".to_owned());
        return;
    }
    call.executable = Some(executable.to_owned());
    let mut family = executable.to_owned();
    if [
        "git",
        "cargo",
        "br",
        "npm",
        "pnpm",
        "docker",
        "kubectl",
        "go",
        "louiselm-usage",
    ]
    .contains(&executable)
        && let Some(part) = parts.peek().copied().filter(|p| SUBCOMMANDS.contains(p))
    {
        family.push(' ');
        family.push_str(part);
        parts.next();
    }
    let mut signature = family.clone();
    if opaque {
        call.family = Some(family);
        call.signature = Some(format!("{signature} <opaque arguments>"));
        call.normalization = Some("prefix_only".into());
        return;
    }
    let mut options = true;
    for part in parts {
        signature.push(' ');
        if part == "--" && options {
            options = false;
            signature.push_str("--");
            continue;
        }
        let flag =
            options && (part.starts_with("--") || (part.len() == 2 && part.starts_with('-')));
        if flag
            && part
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_=.:".contains(c))
        {
            signature.push_str(part.split('=').next().unwrap_or("<arg>"));
            if part.contains('=') {
                signature.push_str("=<value>");
            }
        } else {
            signature.push_str("<arg>");
        }
    }
    call.family = Some(family);
    call.signature = Some(signature);
    call.normalization = Some("simple".to_owned());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attached_short_option_values_are_never_persisted() {
        let mut call = Call::default();
        extract(
            &mut call,
            "git status -pPRIVATE_CREDENTIAL --token=PRIVATE_CREDENTIAL",
        );
        assert!(
            !call
                .signature
                .as_deref()
                .unwrap_or("")
                .contains("PRIVATE_CREDENTIAL")
        );
    }

    #[test]
    fn operands_after_terminator_are_redacted() {
        let mut call = Call::default();
        extract(&mut call, "git status -- --PRIVATE_CREDENTIAL");
        assert!(
            !call
                .signature
                .as_deref()
                .unwrap_or("")
                .contains("PRIVATE_CREDENTIAL")
        );
    }

    #[test]
    fn quoted_arguments_preserve_only_a_known_program_prefix() {
        let mut call = Call::default();
        extract(
            &mut call,
            "rtk proxy rg 'PRIVATE_CREDENTIAL' src && git status",
        );
        assert_eq!(call.family.as_deref(), Some("rg"));
        assert_eq!(call.normalization.as_deref(), Some("prefix_only"));
        assert_eq!(call.signature.as_deref(), Some("rg <opaque arguments>"));
    }
}
