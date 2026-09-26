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
                "expected 'serve', 'adopt-state --confirm', 'session inspect|conformance ID --json', 'beads inspect OPERATION_UUID --json', 'skill-request inspect|reject|cancel ID --json', 'dependencies inspect|approve SESSION [CANDIDATE...] --json', 'waiver inspect|plan|apply|result|revoke SESSION [DIGEST] --json', or 'provider-extend SESSION REQUEST_ID REQUESTS [EXPIRES_AT_MS] --json'"
            }),
            "{error}"
        );
    }
}

#[test]
fn beads_inspection_cli_refuses_mutating_forms_before_broker_exchange() {
    use louiselm_skills::broker::operator::InspectError;
    for arguments in [
        vec![
            "beads",
            "reconcile",
            "12345678-1234-4234-8234-123456789abc",
            "--json",
        ],
        vec![
            "beads",
            "inspect",
            "12345678-1234-4234-8234-123456789abc",
            "--json",
            "apply",
        ],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-control"))
            .args(arguments)
            .env_clear()
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(i32::from(InspectError::InvalidRequest.exit_code()))
        );
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            InspectError::InvalidRequest.canonical_bytes()
        );
    }
}

#[test]
fn waiver_refusals_are_typed_and_do_not_echo_private_input() {
    for arguments in [
        vec!["waiver"],
        vec!["waiver", "inspect", "../private-input", "--json"],
        vec!["waiver", "plan", "session", "private-rationale", "--json"],
        vec!["waiver", "apply", "session", "--json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-control"))
            .args(arguments)
            .env_clear()
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(
            error,
            serde_json::json!({
                "schema": "louiselm.conformance-waiver-error/1",
                "error": "invalid_request", "next_action": "check_request"
            })
        );
    }
}

#[test]
fn provider_extension_refusals_are_typed_before_any_broker_exchange() {
    for arguments in [
        vec!["provider-extend"],
        vec![
            "provider-extend",
            "../private-input",
            "ext-1",
            "2",
            "--json",
        ],
        vec!["provider-extend", "session", "ext-1", "many", "--json"],
        vec!["provider-extend", "session", "ext-1", "2", "soon", "--json"],
        vec!["provider-extend", "session", "ext-1", "2"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-control"))
            .args(arguments)
            .env_clear()
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        let error: serde_json::Value = serde_json::from_slice(&output.stderr).unwrap();
        assert_eq!(
            error,
            serde_json::json!({
                "schema": "louiselm.provider-extension-error/1",
                "error": "invalid_request", "next_action": "check_request"
            })
        );
    }
}

#[test]
fn inspection_refusals_are_typed_and_stdout_stays_empty() {
    use louiselm_skills::broker::operator::InspectError;
    for (verb, id, error) in [
        ("inspect", "../secret", InspectError::InvalidRequest),
        ("inspect", "session", InspectError::BrokerUnavailable),
        ("conformance", "../secret", InspectError::InvalidRequest),
        ("conformance", "session", InspectError::BrokerUnavailable),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-control"))
            .args(["session", verb, id, "--json"])
            .env_clear()
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(i32::from(error.exit_code())));
        assert!(output.stdout.is_empty());
        assert_eq!(output.stderr, error.canonical_bytes());
    }
}

#[test]
fn inspection_argument_errors_use_the_same_typed_contract() {
    use louiselm_skills::broker::operator::InspectError;
    for arguments in [
        vec!["session"],
        vec!["session", "inspect", "session"],
        vec!["session", "inspect", "session", "--json", "extra"],
        vec!["session", "mutate", "session", "--json"],
        vec!["session", "inspect", "session", "--text"],
        vec!["session", "conformance", "session", "--text"],
        vec!["session", "conformance", "session", "--json", "extra"],
        vec!["dependencies", "approve", "session", "--json"],
        vec!["dependencies", "approve", "session", "*", "--json"],
        vec!["dependencies", "inspect", "../foreign", "--json"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-control"))
            .args(arguments)
            .env_clear()
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        assert!(output.stdout.is_empty());
        assert_eq!(
            output.stderr,
            InspectError::InvalidRequest.canonical_bytes()
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
