#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "Fixtures assert real subprocess outcomes."
)]
use super::{SystemTrackerRunner, TrackerInvocation, TrackerRunner};
use std::{ffi::OsString, path::PathBuf, time::Duration};

fn invocation(program: &str, arguments: &[&str]) -> TrackerInvocation {
    TrackerInvocation {
        program: PathBuf::from(program),
        program_digest: crate::Digest::of(&std::fs::read(program).unwrap_or_default()),
        arguments: arguments
            .iter()
            .map(|value| OsString::from(*value))
            .collect(),
        environment: Vec::new(),
        current_dir: std::env::temp_dir(),
    }
}

#[test]
fn reports_zero_exit() {
    let runner = SystemTrackerRunner::new(Duration::from_secs(5));
    let output = runner.run(&invocation("/bin/true", &[])).unwrap();
    assert_eq!(output.exit_code, Some(0));
}

#[test]
fn reports_nonzero_exit() {
    let runner = SystemTrackerRunner::new(Duration::from_secs(5));
    let output = runner.run(&invocation("/bin/false", &[])).unwrap();
    assert_eq!(output.exit_code, Some(1));
}

#[test]
fn kills_and_reports_timeout_past_deadline() {
    let runner = SystemTrackerRunner::new(Duration::from_millis(50));
    let error = runner
        .run(&invocation("/bin/sleep", &["5"]))
        .expect_err("a hung process must be reported, not silently awaited");
    assert!(error.to_string().contains("tracker"));
}

#[test]
fn refuses_a_program_that_does_not_exist() {
    let runner = SystemTrackerRunner::new(Duration::from_secs(5));
    assert!(runner.run(&invocation("/nonexistent/br", &[])).is_err());
}

#[test]
fn rejects_executable_bytes_that_do_not_match_the_pin() {
    let mut request = invocation("/bin/true", &[]);
    request.program_digest = crate::Digest::of(b"wrong");
    assert!(
        SystemTrackerRunner::new(Duration::from_secs(1))
            .run(&request)
            .is_err()
    );
}

#[test]
fn environment_is_cleared_and_only_explicit_values_are_passed() {
    let mut request = invocation(
        "/bin/sh",
        &[
            "-c",
            "test -z \"${HOME+x}\" && test \"$TRACKER_MARKER\" = exact",
        ],
    );
    request
        .environment
        .push(("TRACKER_MARKER".into(), "exact".into()));
    assert_eq!(
        SystemTrackerRunner::new(Duration::from_secs(1))
            .run(&request)
            .unwrap()
            .exit_code,
        Some(0)
    );
}

#[test]
fn deadline_reaps_the_leader_before_returning() {
    let root = tempfile::tempdir().unwrap();
    let pid_file = root.path().join("pid");
    let mut request = invocation(
        "/bin/sh",
        &["-c", "echo $$ > \"$1\"; exec /bin/sleep 5", "tracker-test"],
    );
    request.arguments.push(pid_file.clone().into_os_string());
    assert!(
        SystemTrackerRunner::new(Duration::from_millis(100))
            .run(&request)
            .is_err()
    );
    let pid = rustix::process::Pid::from_raw(
        std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(
        rustix::process::test_kill_process(pid),
        Err(rustix::io::Errno::SRCH)
    );
}
