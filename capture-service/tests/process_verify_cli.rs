//! The editor passes its existing receiver descriptor to the capture executable.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]

use std::{
    fs::File,
    io::Seek,
    os::unix::fs::FileExt,
    process::{Command, Stdio},
};

#[test]
fn verifies_the_inherited_process_without_consuming_the_editors_handle() {
    let pid = std::process::id();
    let namespace = std::fs::read_link("/proc/self/ns/pid").unwrap();
    let mut process = File::open(format!("/proc/{pid}/stat")).unwrap();
    let mut status = File::open("/proc/self/status").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
        .args([
            "phone-control",
            "verify-process",
            "--pid",
            &pid.to_string(),
            "--editor-pid",
            &pid.to_string(),
            "--namespace",
        ])
        .arg(namespace)
        .stdin(process.try_clone().unwrap())
        .stderr(status.try_clone().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"{\"v\":1,\"type\":\"process_verified\"}\n");
    assert_eq!(process.stream_position().unwrap(), 0);
    assert_eq!(status.stream_position().unwrap(), 0);
    let mut bytes = [0; 4096];
    assert!(process.read_at(&mut bytes, 0).unwrap() > 0);
}

#[test]
fn refuses_unavailable_descriptors_and_a_normally_exited_process() {
    let editor_pid = std::process::id();
    let namespace = std::fs::read_link("/proc/self/ns/pid").unwrap();
    let run = |pid: u32, process: File, status: File, namespace: &std::path::Path| {
        Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
            .args([
                "phone-control",
                "verify-process",
                "--pid",
                &pid.to_string(),
                "--editor-pid",
                &editor_pid.to_string(),
                "--namespace",
            ])
            .arg(namespace)
            .stdin(process)
            .stderr(status)
            .output()
            .unwrap()
    };
    let status = || File::open("/proc/self/status").unwrap();
    let process = || File::open("/proc/self/stat").unwrap();
    for output in [
        run(
            editor_pid,
            tempfile::tempfile().unwrap(),
            status(),
            &namespace,
        ),
        run(
            editor_pid,
            process(),
            tempfile::tempfile().unwrap(),
            &namespace,
        ),
        run(
            editor_pid,
            process(),
            status(),
            std::path::Path::new("pid:[0]"),
        ),
    ] {
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"");
    }
    let mut child = Command::new("cat")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let pid = child.id();
    let retained = File::open(format!("/proc/{pid}/stat")).unwrap();
    drop(child.stdin.take());
    assert!(child.wait().unwrap().success());
    let output = run(pid, retained, status(), &namespace);
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
}
