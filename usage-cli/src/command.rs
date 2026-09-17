//! Conservative signatures; unparsed shell programs retain exact private equality.

use crate::model::{Call, digest};
mod prefix;

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
    "awk",
    "chmod",
    "cp",
    "curl",
    "diff",
    "echo",
    "lua",
    "luarocks",
    "make",
    "mkdir",
    "mv",
    "rm",
    "rustup",
    "sort",
    "sqlite3",
    "ssh",
    "systemd-run",
    "tr",
    "uniq",
    "which",
    "bvr",
    "cd",
    "pwd",
    "printf",
    "date",
    "sleep",
    "timeout",
    "gh",
    "stylua",
    "lua-language-server",
    "sha256sum",
    "udisksctl",
    "lsusb",
    "true",
    "false",
    "exit",
    "set",
    "export",
    "test",
    "[",
    "env",
    "sudo",
    "command",
    "exec",
    "adb",
    "launcher-vm",
    "agent-liveness-snapshot",
    "generate-api-appendix",
    "generate-luacats",
    "generate-vimdoc",
    "test-skills-core",
    "playwright-cli",
    "ps",
    "tar",
    "tofu",
    "glab",
    "systemctl",
    "python3.13",
    "louiselm-capture",
    "print",
    "ss",
    "mktemp",
    "kitty",
    "generate-plugin-version",
    "printenv",
    "check-agent-instructions",
    "grep",
    "sync",
    "dig",
    "rustc",
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
    "pr",
    "issue",
    "api",
];

pub(crate) fn extract(call: &mut Call, command: &str) {
    call.argument_bytes = Some(command.len() as u64);
    call.command_key = Some(digest(command.as_bytes()));
    // A known unquoted leading executable is useful even when its arguments are
    // opaque. This identifies the program prefix, never additional shell children.
    let opaque = command.chars().any(|c| "'\"`$|;&<>\\\n(){}#".contains(c));
    let Some((executable, mut rest)) = prefix::leading(command, &mut call.wrapper) else {
        call.family = Some("<opaque>".to_owned());
        call.normalization = Some("opaque".to_owned());
        return;
    };
    let executable = executable.as_str();
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
        "gh",
    ]
    .contains(&executable)
    {
        let mut after = rest;
        if let Some(part) =
            prefix::subcommand(&mut after).filter(|p| SUBCOMMANDS.contains(&p.as_str()))
        {
            family.push(' ');
            family.push_str(&part);
            rest = after;
        }
    }
    let mut signature = family.clone();
    if opaque {
        call.family = Some(family);
        call.signature = Some(format!("{signature} <opaque arguments>"));
        call.normalization = Some("prefix_only".into());
        return;
    }
    let mut options = true;
    for part in rest.split_whitespace() {
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

    #[test]
    fn recognizes_common_executables_and_wrappers_without_persisting_values() {
        let mut call = Call::default();
        extract(
            &mut call,
            "env SECRET=PRIVATE_CREDENTIAL awk 'BEGIN { print 1 }'",
        );
        assert_eq!(call.family.as_deref(), Some("awk"));
        assert_eq!(call.wrapper.as_deref(), Some("env"));
        assert_eq!(call.normalization.as_deref(), Some("prefix_only"));
        assert!(!call.signature.as_deref().unwrap_or("").contains("SECRET"));

        let mut call = Call::default();
        extract(&mut call, "sudo -n git status");
        assert_eq!(call.family.as_deref(), Some("git status"));
        assert_eq!(call.wrapper.as_deref(), Some("sudo"));

        let mut call = Call::default();
        extract(&mut call, "command git status");
        assert_eq!(call.family.as_deref(), Some("git status"));
        assert_eq!(call.wrapper.as_deref(), Some("command"));
    }

    #[test]
    fn recognizes_observed_builtins_and_development_tools() {
        // Prefixes sampled from indexed histories on 2026-09-17; operands synthetic.
        for (command, family) in [
            ("pwd", "pwd"),
            ("cd /PRIVATE && git status", "cd"),
            ("printf '%s' PRIVATE", "printf"),
            ("bvr --robot-triage", "bvr"),
            ("gh pr list", "gh pr"),
            ("stylua --check .", "stylua"),
            ("date -u", "date"),
            ("sleep 1", "sleep"),
            ("sha256sum PRIVATE", "sha256sum"),
            ("./scripts/launcher-vm status", "launcher-vm"),
            (
                "./scripts/agent-liveness-snapshot --status",
                "agent-liveness-snapshot",
            ),
            (
                "./scripts/generate-api-appendix --check",
                "generate-api-appendix",
            ),
        ] {
            let mut call = Call::default();
            extract(&mut call, command);
            assert_eq!(call.family.as_deref(), Some(family));
        }
    }

    #[test]
    fn literal_prefixes_respect_quotes_assignments_and_shell_boundaries() {
        for command in [
            "FOO=PRIVATE /usr/bin/git status",
            "FOO='PRIVATE VALUE' 'git' status",
            "# PRIVATE\ngit status; echo PRIVATE",
            "git status|cat",
            "git status&&echo PRIVATE",
        ] {
            let mut call = Call::default();
            extract(&mut call, command);
            assert_eq!(call.family.as_deref(), Some("git status"));
            assert!(!format!("{call:?}").contains("PRIVATE"));
        }
    }

    #[test]
    fn wrappers_consume_their_own_operands_and_compose() {
        for command in [
            "sudo -u git -- rg PRIVATE",
            "sudo --user=git -n rg PRIVATE",
            "env -u PRIVATE FOO='PRIVATE VALUE' sudo -n rtk proxy rg PRIVATE",
            "timeout -k 1s 10s rg PRIVATE",
            "command -p exec -a PRIVATE rg PRIVATE",
        ] {
            let mut call = Call::default();
            extract(&mut call, command);
            assert_eq!(call.family.as_deref(), Some("rg"));
            assert!(!format!("{call:?}").contains("PRIVATE"));
        }
    }

    #[test]
    fn lookup_modes_and_unknown_wrapper_options_keep_wrapper_identity() {
        for (command, family) in [
            ("command -v git", "command"),
            ("sudo -l git", "sudo"),
            ("sudo --invented git status", "sudo"),
            ("env --split-string='git status'", "env"),
            ("timeout --help", "timeout"),
            ("rtk --help", "rtk"),
            ("timeout 1ss git status", "timeout"),
        ] {
            let mut call = Call::default();
            extract(&mut call, command);
            assert_eq!(call.family.as_deref(), Some(family));
        }
    }

    #[test]
    fn dynamic_prefixes_never_promote_a_trailing_known_name() {
        for command in [
            "$PRIVATE/git status",
            "$(echo PRIVATE)/git status",
            "`echo PRIVATE`/git status",
            "FOO=$(echo PRIVATE) git status",
            "sudo -u 'PRIVATE git status",
            "env FOO=PRIVATE; git status",
            "rtk proxy FOO=PRIVATE git status",
            "exec FOO=PRIVATE git status",
            "[ab]/git status PRIVATE",
        ] {
            let mut call = Call::default();
            extract(&mut call, command);
            assert_ne!(call.family.as_deref(), Some("git status"));
            assert!(!format!("{call:?}").contains("PRIVATE"));
        }
    }
}
