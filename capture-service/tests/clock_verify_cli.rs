//! The editor passes its existing clock descriptor to the capture executable.

#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test fixtures")]

use std::{fs::File, io::Seek, os::unix::fs::FileExt, process::Command};

#[test]
fn verifies_the_inherited_clock_without_consuming_the_editors_handle() {
    let mut clock = File::open("/proc/uptime").unwrap();
    assert_eq!(clock.stream_position().unwrap(), 0);
    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
        .args(["phone-control", "verify-clock"])
        .stdin(clock.try_clone().unwrap())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(output.stdout, b"{\"v\":1,\"type\":\"clock_verified\"}\n");
    assert_eq!(clock.stream_position().unwrap(), 0);
    let mut bytes = [0; 128];
    assert!(clock.read_at(&mut bytes, 0).unwrap() > 0);
}

#[test]
fn refuses_an_ordinary_file_and_missing_input() {
    let file = tempfile::tempfile().unwrap();
    for input in [std::process::Stdio::from(file), std::process::Stdio::null()] {
        let output = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
            .args(["phone-control", "verify-clock"])
            .stdin(input)
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert_eq!(output.stdout, b"");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("boot clock mount is unavailable")
        );
    }
}

#[test]
fn refuses_another_file_on_the_proc_mount() {
    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
        .args(["phone-control", "verify-clock"])
        .stdin(File::open("/proc/self/stat").unwrap())
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert_eq!(output.stdout, b"");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("boot clock descriptor is unavailable")
    );
}
