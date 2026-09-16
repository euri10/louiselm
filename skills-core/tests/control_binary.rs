//! Refuse invalid daemon invocations before opening installed authority.
#![allow(
    clippy::unwrap_used,
    reason = "Process fixtures assert setup and refusals."
)]

use std::process::Command;

#[test]
fn validates_verbs_confirmation_and_socket_activation() {
    for arguments in [
        vec![],
        vec!["inspect"],
        vec!["serve", "extra"],
        vec!["serve"],
        vec!["adopt-state"],
        vec!["adopt-state", "--confirm", "extra"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-control"))
            .args(&arguments)
            .env_clear()
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(output.stdout.is_empty());
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains(if arguments == ["serve"] {
                "socket activation"
            } else {
                "expected 'serve' or 'adopt-state --confirm'"
            }),
            "{error}"
        );
    }
}

#[test]
fn adoption_requires_the_installed_broker_and_sudo_operator() {
    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-control"))
        .args(["adopt-state", "--confirm"])
        .env_clear()
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("sudo operator")
    );
}

#[test]
fn forged_activation_without_fd_three_is_rejected_safely() {
    let output = Command::new("/bin/sh")
        .args([
            "-c",
            "export LISTEN_PID=$$ LISTEN_FDS=1; exec 3<&-; exec \"$1\" serve",
            "sh",
            env!("CARGO_BIN_EXE_louiselm-control"),
        ])
        .env_clear()
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("socket activation")
    );
}
