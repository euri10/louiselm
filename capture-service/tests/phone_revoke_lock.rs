//! Real-process coverage for offline phone revocation.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::{
    fs::{self, OpenOptions},
    io::{self, BufRead, BufReader},
    os::unix::net::UnixListener,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use louiselm_capture::PairingRegistry;

#[test]
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
fn unanswered_live_administration_is_bounded_and_unconfirmed() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let workflow = state.join("louiselm/workflow");
    fs::create_dir_all(&workflow).unwrap();
    fs::write(workflow.join("operator-capability"), "a".repeat(64)).unwrap();
    let listener = UnixListener::bind(workflow.join("phone-control-admin.sock")).unwrap();
    listener.set_nonblocking(true).unwrap();
    let mut revoke = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
        .env("LOUISELM_CAPTURE_DATA_DIR", root.path().join("data"))
        .env("LOUISELM_CAPTURE_STATE_DIR", &state)
        .env("LOUISELM_CAPTURE_CONFIG_DIR", &state)
        .args(["revoke-device", "22222222-2222-4222-8222-222222222222"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let peer = loop {
        match listener.accept() {
            Ok((peer, _)) => break peer,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    revoke.kill().unwrap();
                    revoke.wait().unwrap();
                    panic!("administration was not contacted");
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("administration accept failed: {error}"),
        }
    };
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut request = String::new();
    BufReader::new(&peer).read_line(&mut request).unwrap();
    assert!(request.contains("forget"));
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
    assert!(exited, "unanswered administration hung");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unconfirmed"));
    drop(peer);
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
