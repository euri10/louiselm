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
fn privileged_entrypoint_accepts_only_fixed_operator_verbs() {
    // Only exact run/certify are operator verbs; no caller-selected commands.
    let binary = env!("CARGO_BIN_EXE_louiselm-launch");
    for arguments in [
        vec![],
        vec!["status"],
        vec!["run", "--broker", "/tmp/socket"],
        vec!["run", "__sandbox_bootstrap"],
        vec!["certify", "--probe", "private-command"],
        vec!["cleanup", "--root", "/tmp/private-storage"],
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
                "louiselm-launch: expected exactly 'run', 'certify' or root-only 'cleanup'\n"
            }
        );
    }
}

#[test]
fn development_certifier_cannot_acquire_installed_authority() {
    for verb in ["certify", "__conformance-worker", "cleanup"] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-launch"))
            .arg(verb)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
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
