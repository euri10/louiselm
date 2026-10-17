//! Behavioral coverage for launch binary.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]
#![cfg(target_os = "linux")]

use std::process::{Command, Stdio};

#[test]
fn generated_help_is_public_and_does_not_acquire_authority() {
    for args in [
        vec!["--help"],
        vec!["run", "--help"],
        vec!["recover-arm", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-launch"))
            .args(args)
            .env_clear()
            .output()
            .unwrap();
        assert!(output.status.success());
        let help = String::from_utf8(output.stdout).unwrap();
        assert!(help.contains("Usage:"));
        assert!(!help.contains("__sandbox_bootstrap"));
        assert!(!help.contains("__conformance"));
        assert_eq!(output.stderr, [] as [u8; 0]);
    }
}

#[test]
fn privileged_entrypoint_accepts_only_fixed_operator_verbs() {
    // Fixed verbs only; administrative recovery accepts no paths or commands.
    let binary = env!("CARGO_BIN_EXE_louiselm-launch");
    for arguments in [
        vec![],
        vec!["status"],
        vec!["run", "--broker", "/tmp/socket"],
        vec!["run", "__sandbox_bootstrap"],
        vec!["certify", "--probe", "private-command"],
        vec!["prepare", "--root", "/tmp/private-storage"],
        vec!["cleanup", "--root", "/tmp/private-storage"],
        vec!["recover-arm"],
        vec!["recover-complete"],
        vec!["recover-arm", "approval", "--root", "/tmp/private-storage"],
        vec!["recover-complete", "approval", "--broker", "/tmp/socket"],
        vec!["__conformance-worker", "private-command"],
    ] {
        let output = Command::new(binary)
            .args(&arguments)
            .output()
            .expect("launcher binary executes");
        assert!(!output.status.success(), "{arguments:?} must be rejected");
        assert!(output.stdout.is_empty(), "stdout is reserved for ACP bytes");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            if arguments.len() > 1 {
                "louiselm-launch: unexpected arguments\n"
            } else {
                "louiselm-launch: invalid launcher command; see --help\n"
            }
        );
    }
}

#[test]
fn development_recovery_cannot_acquire_installed_authority() {
    for verb in ["recover-arm", "recover-complete"] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-launch"))
            .args([verb, "approval"])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, [] as [u8; 0]);
        assert!(
            output.stderr == b"louiselm-launch: root launcher authority required\n"
                || output.stderr == b"louiselm-launch: running launcher release is untrusted\n"
        );
    }
}

#[test]
fn development_certifier_cannot_acquire_installed_authority() {
    for verb in ["prepare", "certify", "__conformance-worker", "cleanup"] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-launch"))
            .arg(verb)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, [] as [u8; 0]);
        assert!(
            output.stderr == b"louiselm-launch: root launcher authority required\n"
                || output.stderr == b"louiselm-launch: running launcher release is untrusted\n"
        );
    }
}

#[test]
fn bootstrap_without_inherited_capabilities_fails_without_disclosing_arguments() {
    for arguments in [
        vec!["__sandbox_bootstrap"],
        vec!["__sandbox_bootstrap", "/bin/echo", "private-argument"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-launch"))
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .expect("launcher binary executes");

        assert!(!output.status.success());
        assert!(output.stdout.is_empty(), "the workload must not execute");
        assert_eq!(
            output.stderr, b"louiselm-launch: sandbox bootstrap failed\n",
            "bootstrap failures must not disclose supplied paths or arguments",
        );
    }
}
