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
    io::{self, BufRead, BufReader, Read, Write},
    os::unix::net::{UnixListener, UnixStream},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use fs2::FileExt;
use louiselm_capture::PairingRegistry;
use rustix::fs::{FlockOperation, fcntl_lock};

#[path = "support/phone_evidence.rs"]
#[allow(
    dead_code,
    reason = "this integration uses only the bound archive fixture"
)]
mod phone_evidence;

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

#[test]
fn offline_receiver_lock_child() {
    let Ok(tls) = std::env::var("LOUISELM_TEST_OFFLINE_RECEIVER_TLS") else {
        return;
    };
    let pairing = std::io::stdin();
    fcntl_lock(&pairing, FlockOperation::NonBlockingLockExclusive).unwrap();
    let registrar = OpenOptions::new()
        .read(true)
        .write(true)
        .open(std::path::Path::new(&tls).join("phone-admission-registrar.lock"))
        .unwrap();
    fcntl_lock(&registrar, FlockOperation::NonBlockingLockExclusive).unwrap();
    let mut peer =
        UnixStream::connect(std::path::Path::new(&tls).join("receiver-ready.sock")).unwrap();
    peer.write_all(b"R").unwrap();
    let mut finish = [0];
    peer.read_exact(&mut finish).unwrap();
}

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

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one real-process acceptance scenario"
)]
fn offline_forget_succeeds_after_receiver_death_with_duplicate_and_paused_neovim() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    let capture = state.join("louiselm/capture");
    let tls = capture.join("tls");
    let pairing = capture.join("pairing");
    let identity = louiselm_capture::TlsIdentity::load_or_create(&tls).unwrap();
    let receiver = identity.public_key_sha256().to_owned();
    let registry = PairingRegistry::open(&pairing).unwrap();
    let offer = registry
        .issue("https://192.0.2.1:7391", &receiver, 1_000, 5_000)
        .unwrap();
    let phone = registry.consume(&offer.token, "phone", 2_000).unwrap();
    drop(registry);
    phone_evidence::initialize(
        &capture.join("phone-control/action-evidence"),
        &tls.join("phone-action-evidence.json"),
        &receiver,
        louiselm_capture::ActionEvidenceKind::Archive,
    );
    phone_evidence::initialize_barrier(&tls, &receiver);
    phone_evidence::bind_barrier_to_archive(&tls);

    let workflow = state.join("louiselm/workflow");
    fs::create_dir_all(&workflow).unwrap();
    let orphaned_admin = UnixListener::bind(workflow.join("phone-control-admin.sock")).unwrap();
    drop(orphaned_admin);

    let ready = UnixListener::bind(tls.join("receiver-ready.sock")).unwrap();
    let duplicate = OpenOptions::new()
        .read(true)
        .write(true)
        .open(pairing.join("pairing.lock"))
        .unwrap();
    let mut receiver_process = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "offline_receiver_lock_child", "--nocapture"])
            .env("LOUISELM_TEST_OFFLINE_RECEIVER_TLS", &tls)
            .stdin(Stdio::from(duplicate.try_clone().unwrap()))
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let (mut ready_peer, _) = ready.accept().unwrap();
    let mut byte = [0];
    ready_peer.read_exact(&mut byte).unwrap();
    assert_eq!(byte, *b"R");
    receiver_process.0.kill().unwrap();
    receiver_process.0.wait().unwrap();

    let mut neovim = OwnedChild(
        Command::new("nvim")
            .args([
                "--headless",
                "--clean",
                "-u",
                "NONE",
                "-c",
                "lua vim.wait(30000)",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    Command::new("kill")
        .args(["-STOP", &neovim.0.id().to_string()])
        .status()
        .unwrap();

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
    while revoke.try_wait().unwrap().is_none() {
        if Instant::now() >= deadline {
            revoke.kill().unwrap();
            panic!("offline revoke did not finish within its bound");
        }
        thread::sleep(Duration::from_millis(10));
    }
    let output = revoke.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&output.stdout).trim(),
        phone.device_id
    );
    assert!(duplicate.metadata().is_ok());
    assert!(neovim.0.try_wait().unwrap().is_none());
    assert_eq!(
        PairingRegistry::open(pairing)
            .unwrap()
            .authenticate_device(&phone.credential)
            .unwrap(),
        None
    );
}
