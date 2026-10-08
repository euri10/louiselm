//! Refuse invalid daemon invocations before opening installed authority.
#![allow(
    clippy::unwrap_used,
    reason = "Process fixtures assert setup and refusals."
)]

use std::{
    fs::File,
    os::fd::OwnedFd,
    path::Path,
    process::{Command, Stdio},
};

use rustix::net::{
    AddressFamily, SocketAddrUnix, SocketFlags, SocketType, bind, listen, socket_with, socketpair,
};

#[test]
fn generated_help_needs_no_installed_authority_or_stdin_payload() {
    for args in [
        vec!["--help"],
        vec!["session", "inspect", "--help"],
        vec!["run", "authorize", "--help"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-control"))
            .args(args)
            .env_clear()
            .output()
            .unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("Usage:"));
        assert_eq!(output.stderr, [] as [u8; 0]);
    }
}

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
        assert_eq!(output.stdout, [] as [u8; 0]);
        let error = String::from_utf8(output.stderr).unwrap();
        assert!(
            error.contains(if arguments == ["serve"] {
                "socket on standard input (StandardInput=socket)"
            } else {
                "expected 'serve', 'adopt-state --confirm', 'run authorize --json', 'launch-inputs stage --json', 'session inspect|conformance ID --json', 'beads inspect OPERATION_UUID --json', 'skill-request inspect|reject|cancel ID --json', 'dependencies inspect|approve SESSION [CANDIDATE...] --json', 'waiver inspect|plan|apply|result|revoke SESSION [DIGEST] --json', or 'provider-extend SESSION REQUEST_ID REQUESTS [EXPIRES_AT_MS] --json'"
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
        assert_eq!(output.stdout, [] as [u8; 0]);
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
        assert_eq!(output.stdout, [] as [u8; 0]);
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
        assert_eq!(output.stdout, [] as [u8; 0]);
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
        assert_eq!(output.stdout, [] as [u8; 0]);
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
        assert_eq!(output.stdout, [] as [u8; 0]);
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
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("sudo operator")
    );
}

const NOT_A_LISTENER: &str = "louiselm-control: serve requires a listening Unix SOCK_SEQPACKET socket on standard input (StandardInput=socket)\n";

fn serve_with_stdin(stdin: Stdio, script: &str) -> std::process::Output {
    Command::new("/bin/sh")
        .args(["-c", script, "sh", env!("CARGO_BIN_EXE_louiselm-control")])
        .env_clear()
        .stdin(stdin)
        .output()
        .unwrap()
}

fn listening_seqpacket(path: &Path) -> OwnedFd {
    let fd = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    bind(&fd, &SocketAddrUnix::new(path).unwrap()).unwrap();
    listen(&fd, 8).unwrap();
    fd
}

#[test]
fn serve_adopts_only_a_listening_seqpacket_socket_on_stdin() {
    let directory = tempfile::TempDir::new().unwrap();
    // A real listener passes adoption; this uninstalled build then fails the
    // release trust check instead of hanging in accept.
    let listener = listening_seqpacket(&directory.path().join("control.sock"));
    let output = serve_with_stdin(Stdio::from(listener), "exec \"$1\" serve");
    assert_eq!(output.status.code(), Some(1));
    assert_eq!(output.stdout, [] as [u8; 0]);
    assert_eq!(
        String::from_utf8(output.stderr).unwrap(),
        "louiselm-control: running broker release is untrusted\n"
    );

    let file = File::create(directory.path().join("regular")).unwrap();
    let (connected, _peer) = socketpair(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    // The former LISTEN_PID/LISTEN_FDS handover on fd 3 is no longer honoured.
    let legacy = listening_seqpacket(&directory.path().join("legacy.sock"));
    for (name, stdin, script) in [
        ("null", Stdio::null(), "exec \"$1\" serve"),
        // The Rust runtime reopens a closed fd 0 on /dev/null before main.
        ("closed", Stdio::null(), "exec 0<&-; exec \"$1\" serve"),
        ("regular file", Stdio::from(file), "exec \"$1\" serve"),
        (
            "connected socket",
            Stdio::from(connected),
            "exec \"$1\" serve",
        ),
        (
            "systemd fd 3",
            Stdio::from(legacy),
            "export LISTEN_PID=$$ LISTEN_FDS=1; exec 3<&0; exec 0</dev/null; exec \"$1\" serve",
        ),
    ] {
        let output = serve_with_stdin(stdin, script);
        assert_eq!(output.status.code(), Some(1), "{name}");
        assert_eq!(output.stdout, [] as [u8; 0], "{name}");
        assert_eq!(
            String::from_utf8(output.stderr).unwrap(),
            NOT_A_LISTENER,
            "{name}"
        );
    }
}
