//! Real-process coverage for offline phone revocation.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use louiselm_capture::PairingRegistry;

#[test]
#[ignore = "offline cutoff proof is not yet implemented"]
fn offline_forget_without_a_provable_cutoff_is_unconfirmed() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let pairing = state.join("louiselm/capture/pairing");
    let registry = PairingRegistry::open(&pairing).unwrap();
    let offer = registry
        .issue("https://192.0.2.1:7391", &"a".repeat(64), 1_000, 5_000)
        .unwrap();
    let phone = registry.consume(&offer.token, "phone", 2_000).unwrap();
    drop(registry);

    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
        .env("LOUISELM_CAPTURE_DATA_DIR", root.path().join("data"))
        .env("LOUISELM_CAPTURE_STATE_DIR", &state)
        .env("LOUISELM_CAPTURE_CONFIG_DIR", &state)
        .args(["revoke-device", &phone.device_id])
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unconfirmed"));
    assert_eq!(
        PairingRegistry::open(pairing)
            .unwrap()
            .authenticate_device(&phone.credential)
            .unwrap(),
        Some(phone.device_id)
    );
}

#[test]
fn offline_forget_refuses_a_live_lock_within_a_bound() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let pairing = state.join("louiselm/capture/pairing");
    let registry = PairingRegistry::open(&pairing).unwrap();
    let offer = registry
        .issue("https://192.0.2.1:7391", &"a".repeat(64), 1_000, 5_000)
        .unwrap();
    let phone = registry.consume(&offer.token, "phone", 2_000).unwrap();
    let held = OpenOptions::new()
        .read(true)
        .write(true)
        .open(pairing.join("pairing.lock"))
        .unwrap();
    held.lock_exclusive().unwrap();
    let mut revoke = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
        .env("LOUISELM_CAPTURE_DATA_DIR", root.path().join("data"))
        .env("LOUISELM_CAPTURE_STATE_DIR", &state)
        .env("LOUISELM_CAPTURE_CONFIG_DIR", &state)
        .args(["revoke-device", &phone.device_id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let exited = loop {
        if revoke.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            revoke.kill().unwrap();
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let output = revoke.wait_with_output().unwrap();
    FileExt::unlock(&held).unwrap();
    assert!(
        exited,
        "offline revoke hung while another process held the lock"
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unconfirmed"));
    assert_eq!(
        registry.authenticate_device(&phone.credential).unwrap(),
        Some(phone.device_id)
    );
}

#[test]
fn offline_forget_is_unconfirmed_after_lock_owner_death_with_a_duplicate_open() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let pairing = state.join("louiselm/capture/pairing");
    let registry = PairingRegistry::open(&pairing).unwrap();
    let offer = registry
        .issue("https://192.0.2.1:7391", &"a".repeat(64), 1_000, 5_000)
        .unwrap();
    let phone = registry.consume(&offer.token, "phone", 2_000).unwrap();
    drop(registry);

    // The holder acquires both Linux lock types, so the same fixture covers the
    // old flock and the proposed process-owned record lock. The coordinator's
    // inherited descriptor stays open after the holder is killed and reaped.
    let duplicate = OpenOptions::new()
        .read(true)
        .write(true)
        .open(pairing.join("pairing.lock"))
        .unwrap();
    let mut holder = Command::new("python3")
        .args([
            "-c",
            "import fcntl, signal\nfcntl.flock(0, fcntl.LOCK_EX)\nfcntl.lockf(0, fcntl.LOCK_EX)\nsignal.alarm(30)\nprint('locked', flush=True)\nsignal.pause()",
        ])
        .stdin(Stdio::from(duplicate.try_clone().unwrap()))
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut ready = String::new();
    BufReader::new(holder.stdout.take().unwrap())
        .read_line(&mut ready)
        .unwrap();
    assert_eq!(ready, "locked\n");
    holder.kill().unwrap();
    holder.wait().unwrap();

    let mut revoke = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
        .env("LOUISELM_CAPTURE_DATA_DIR", root.path().join("data"))
        .env("LOUISELM_CAPTURE_STATE_DIR", &state)
        .env("LOUISELM_CAPTURE_CONFIG_DIR", &state)
        .args(["revoke-device", &phone.device_id])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    let exited = loop {
        if revoke.try_wait().unwrap().is_some() {
            break true;
        }
        if Instant::now() >= deadline {
            revoke.kill().unwrap();
            break false;
        }
        thread::sleep(Duration::from_millis(10));
    };
    let output = revoke.wait_with_output().unwrap();
    assert!(exited, "offline revoke hung while the duplicate survived");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unconfirmed"));
    drop(duplicate);
    let registry = PairingRegistry::open(pairing).unwrap();
    assert_eq!(
        registry.authenticate_device(&phone.credential).unwrap(),
        Some(phone.device_id)
    );
}
