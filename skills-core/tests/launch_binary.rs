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
fn privileged_entrypoint_accepts_only_the_fixed_run_verb() {
    // `run` remains the sole privileged operator verb. The internal bootstrap
    // has no launcher authority and instead requires inherited capabilities.
    let binary = env!("CARGO_BIN_EXE_louiselm-launch");
    for arguments in [
        vec![],
        vec!["status"],
        vec!["run", "--broker", "/tmp/socket"],
        vec!["run", "__sandbox_bootstrap"],
    ] {
        let output = Command::new(binary)
            .args(&arguments)
            .output()
            .expect("launcher binary executes");
        assert!(!output.status.success(), "{arguments:?} must be rejected");
        assert!(output.stdout.is_empty(), "stdout is reserved for ACP bytes");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            "louiselm-launch: expected exactly 'run'\n"
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
