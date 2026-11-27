//! Real receiver and offline cutoff in a disposable network namespace.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::{
    io,
    net::{SocketAddr, TcpStream},
    os::{
        linux::net::SocketAddrExt,
        unix::net::{SocketAddr as UnixSocketAddr, UnixListener, UnixStream},
    },
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use louiselm_capture::{NetworkProfile, NetworkProfileKind, PairingRegistry, TlsIdentity};

struct Receiver(Child);

impl Drop for Receiver {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            // Only the child owned by this disposable fixture is stopped.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn namespace_socket(name: &[u8]) -> io::Result<UnixListener> {
    UnixListener::bind_addr(&UnixSocketAddr::from_abstract_name(name)?)
}

fn fixture(root: &Path) -> (PathBuf, String) {
    let state = root.join("state");
    let capture = state.join("louiselm/capture");
    let tls = capture.join("tls");
    // Establish both bound stores through the production startup. The previous
    // archive-only fixture could not exercise a healthy write registrar.
    let receiver_process = start(root, &state);
    let identity = TlsIdentity::load_or_create(&tls).unwrap();
    let receiver = identity.public_key_sha256();
    let pairing = PairingRegistry::open(capture.join("pairing")).unwrap();
    let offer = pairing
        .issue("https://192.0.2.1:7391", receiver, 1_000, 5_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "fixture", 2_000).unwrap();
    stop(receiver_process);
    (state, phone.device_id)
}

fn command(root: &Path, state: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"));
    command
        .env("LOUISELM_CAPTURE_STATE_DIR", state)
        .env("LOUISELM_CAPTURE_DATA_DIR", root.join("data"))
        .env("LOUISELM_CAPTURE_CONFIG_DIR", root.join("config"));
    command
}

fn start(root: &Path, state: &Path) -> Receiver {
    let mut child = Receiver(
        command(root, state)
            .env("LOUISELM_RECEIVER_ENABLED", "true")
            .env("LOUISELM_PHONE_CONTROL_ENABLED", "true")
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let socket = state.join("louiselm/workflow/phone-control-admin.sock");
    let deadline = Instant::now() + Duration::from_secs(10);
    while UnixStream::connect(&socket).is_err() {
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "receiver exited at startup"
        );
        assert!(Instant::now() < deadline, "receiver did not start");
        thread::sleep(Duration::from_millis(10));
    }
    child
}

fn wait_for_receiver(receiver: &mut Receiver, address: SocketAddr) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_err() {
        assert!(
            receiver.0.try_wait().unwrap().is_none(),
            "receiver exited before listening"
        );
        assert!(Instant::now() < deadline, "receiver did not listen");
        thread::sleep(Duration::from_millis(10));
    }
}

fn stop(mut receiver: Receiver) {
    assert!(
        Command::new("kill")
            .args(["-TERM", &receiver.0.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while receiver.0.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "receiver did not stop");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
#[ignore = "requires a disposable network namespace"]
fn real_receiver_and_offline_cutoff_share_exclusion() {
    const STARTUP: &[u8] = b"louiselm.phone.startup.v1";
    const REGISTRAR: &[u8] = b"louiselm.phone.registrar.v1";
    let root = tempfile::tempdir().unwrap();
    let first = root.path().join("first");
    let second = root.path().join("second");
    let (first_state, _) = fixture(&first);
    let (second_state, device) = fixture(&second);
    let second_address: SocketAddr = "10.254.0.1:7392".parse().unwrap();
    NetworkProfile::new(
        NetworkProfileKind::Private,
        second_address,
        "https://10.254.0.1:7392",
    )
    .unwrap()
    .save(&second.join("config/louiselm/capture-network.json"))
    .unwrap();

    // Contended startup still permits the unrelated read-only service.
    let startup = namespace_socket(STARTUP).unwrap();
    let receiver = start(&first, &first_state);
    assert!(namespace_socket(REGISTRAR).is_ok());
    stop(receiver);
    drop(startup);

    let receiver = start(&first, &first_state);
    assert_eq!(
        namespace_socket(REGISTRAR).unwrap_err().kind(),
        io::ErrorKind::AddrInUse
    );
    let blocked = command(&second, &second_state)
        .args(["revoke-device", &device])
        .output()
        .unwrap();
    assert!(!blocked.status.success());
    assert!(String::from_utf8_lossy(&blocked.stderr).contains("admission cutoff is unavailable"));

    // A second live receiver remains available for reads but cannot mutate
    // bound authority, including after the first receiver has stopped.
    let mut competing = start(&second, &second_state);
    wait_for_receiver(&mut competing, second_address);
    let read = command(&second, &second_state)
        .args(["phone-control", "project-roots"])
        .output()
        .unwrap();
    assert!(
        read.status.success(),
        "{}",
        String::from_utf8_lossy(&read.stderr)
    );
    let revoke = || {
        command(&second, &second_state)
            .args(["revoke-device", &device])
            .output()
            .unwrap()
    };
    let refused = revoke();
    assert!(!refused.status.success());
    assert!(String::from_utf8_lossy(&refused.stderr).contains("authority did not confirm"));
    stop(receiver);
    assert!(!revoke().status.success());
    stop(competing);

    let completed = command(&second, &second_state)
        .args(["revoke-device", &device])
        .output()
        .unwrap();
    assert!(
        completed.status.success(),
        "{}",
        String::from_utf8_lossy(&completed.stderr)
    );
    let mut restarted = start(&second, &second_state);
    wait_for_receiver(&mut restarted, second_address);
    let registered = command(&second, &second_state)
        .args(["phone-control", "register-root", "--path"])
        .arg(&second)
        .output()
        .unwrap();
    assert!(
        registered.status.success(),
        "{}",
        String::from_utf8_lossy(&registered.stderr)
    );
    stop(restarted);
    let receiver = start(&first, &first_state);
    assert_eq!(
        namespace_socket(REGISTRAR).unwrap_err().kind(),
        io::ErrorKind::AddrInUse
    );
    stop(receiver);
    assert!(namespace_socket(REGISTRAR).is_ok());
}
