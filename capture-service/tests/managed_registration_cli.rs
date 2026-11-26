//! Real CLI consumers against a disposable registration-service fixture.
#![cfg(target_os = "linux")]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::{
    fs::{self, File},
    io::{BufRead, BufReader, IoSliceMut, Read, Write},
    mem::MaybeUninit,
    net::{SocketAddr, TcpStream},
    os::unix::{
        fs::MetadataExt,
        net::{UnixListener, UnixStream},
    },
    path::Path,
    process::{Child, Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

use louiselm_capture::{NetworkProfile, NetworkProfileKind, TlsIdentity};
use rustix::net::{self, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags};

const SOCKET: &str = "/run/louiselm-phone-host/registration.sock";

struct OwnedChild(Option<Child>);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.0
            && child.try_wait().ok().flatten().is_none()
        {
            // Only the child owned by this disposable fixture is stopped.
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn with_writer<T>(root: &Path, reply: String, run: impl FnOnce() -> T) -> T {
    // This ignored gate is run only with a private /run mount.
    assert!(!Path::new(SOCKET).exists());
    let listener = UnixListener::bind(SOCKET).unwrap();
    listener.set_nonblocking(true).unwrap();
    let expected = fs::metadata(root).unwrap();
    let socket_identity = fs::symlink_metadata(SOCKET).unwrap();
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut connection = loop {
            match listener.accept() {
                Ok((connection, _)) => break connection,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(Instant::now() < deadline, "registration was not requested");
                    thread::sleep(Duration::from_millis(10));
                }
                Err(error) => panic!("registration accept failed: {error}"),
            }
        };
        connection
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut byte = [0];
        let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
        let mut ancillary = RecvAncillaryBuffer::new(&mut space);
        let message = net::recvmsg(
            &connection,
            &mut [IoSliceMut::new(&mut byte)],
            &mut ancillary,
            RecvFlags::CMSG_CLOEXEC,
        )
        .unwrap();
        assert_eq!(message.bytes, 1);
        assert_eq!(byte, *b"R");
        let descriptors: Vec<_> = ancillary
            .drain()
            .flat_map(|item| match item {
                RecvAncillaryMessage::ScmRights(rights) => rights.collect::<Vec<_>>(),
                _ => Vec::new(),
            })
            .collect();
        let [descriptor]: [_; 1] = descriptors.try_into().unwrap();
        let actual = File::from(descriptor).metadata().unwrap();
        assert_eq!(
            (actual.dev(), actual.ino()),
            (expected.dev(), expected.ino())
        );
        connection.write_all(reply.as_bytes()).unwrap();
    });
    let result = run();
    worker.join().unwrap();
    let current = fs::symlink_metadata(SOCKET).unwrap();
    assert_eq!(
        (current.dev(), current.ino()),
        (socket_identity.dev(), socket_identity.ino())
    );
    fs::remove_file(SOCKET).unwrap();
    result
}

fn command(root: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_louiselm-capture"));
    command
        .env("LOUISELM_CAPTURE_STATE_DIR", root.join("state"))
        .env("LOUISELM_CAPTURE_DATA_DIR", root.join("data"))
        .env("LOUISELM_CAPTURE_CONFIG_DIR", root.join("config"));
    command
}

fn configure_pairing(root: &Path) {
    NetworkProfile::new(
        NetworkProfileKind::Private,
        "192.168.10.1:7391".parse().unwrap(),
        "https://192.168.10.1:7391",
    )
    .unwrap()
    .save(&root.join("config/louiselm/capture-network.json"))
    .unwrap();
}

fn pair(root: &Path, output: &Path) -> Output {
    command(root)
        .arg("pair")
        .arg("--svg")
        .arg(output)
        .output()
        .unwrap()
}

#[test]
#[ignore = "requires disposable private mount and network namespaces"]
#[allow(
    clippy::too_many_lines,
    reason = "one real CLI lifecycle covers matching, refusal and capture-only behavior"
)]
fn pair_and_serve_consume_registration_without_granting_authority() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path().join("existing");
    let capture = root.join("state/louiselm/capture");
    let identity = TlsIdentity::load_or_create(capture.join("tls")).unwrap();
    configure_pairing(&root);
    let committed = format!(
        "{{\"v\":1,\"result\":\"committed\",\"receiver_sha256\":\"{}\"}}",
        identity.public_key_sha256()
    );
    let svg = root.join("pair.svg");
    let output = with_writer(&capture, committed, || pair(&root, &svg));
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(svg.is_file());

    let wrong = if identity.public_key_sha256() == "a".repeat(64) {
        "b".repeat(64)
    } else {
        "a".repeat(64)
    };
    let mismatch = format!("{{\"v\":1,\"result\":\"committed\",\"receiver_sha256\":\"{wrong}\"}}");
    let refused_svg = root.join("refused.svg");
    let output = with_writer(&capture, mismatch.clone(), || pair(&root, &refused_svg));
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("registered receiver identity differs")
    );
    assert!(!refused_svg.exists());

    let fresh = temporary.path().join("capture-only");
    let fresh_capture = fresh.join("state/louiselm/capture");
    fs::create_dir_all(&fresh_capture).unwrap();
    configure_pairing(&fresh);
    let svg = fresh.join("pair.svg");
    let output = with_writer(
        &fresh_capture,
        r#"{"v":1,"result":"unavailable"}"#.into(),
        || pair(&fresh, &svg),
    );
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(svg.is_file());
    assert!(fresh_capture.join("tls/receiver-key.pem").is_file());

    // A bad committed identity must preserve unrelated read attachment without
    // replacing the existing key or making the receiver a registrar.
    let service = temporary.path().join("service");
    let service_capture = service.join("state/louiselm/capture");
    let identity = TlsIdentity::load_or_create(service_capture.join("tls")).unwrap();
    let wrong = if identity.public_key_sha256() == "a".repeat(64) {
        "b".repeat(64)
    } else {
        "a".repeat(64)
    };
    let reply = format!("{{\"v\":1,\"result\":\"committed\",\"receiver_sha256\":\"{wrong}\"}}");
    let child = with_writer(&service_capture, reply, || {
        command(&service)
            .env("LOUISELM_RECEIVER_ENABLED", "true")
            .env("LOUISELM_PHONE_CONTROL_ENABLED", "true")
            .arg("serve")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap()
    });
    let mut child = OwnedChild(Some(child));
    let address: SocketAddr = "127.0.0.1:7391".parse().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while TcpStream::connect_timeout(&address, Duration::from_millis(50)).is_err() {
        assert!(child.0.as_mut().unwrap().try_wait().unwrap().is_none());
        assert!(Instant::now() < deadline, "capture receiver did not start");
        thread::sleep(Duration::from_millis(10));
    }
    let socket = service.join("state/louiselm/workflow/phone-control.sock");
    let capability =
        fs::read_to_string(service.join("state/louiselm/workflow/operator-capability")).unwrap();
    let mut controller = UnixStream::connect(socket).unwrap();
    controller
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(
        controller,
        "{}",
        serde_json::json!({"v": 1, "type": "attach", "capability": capability.trim()})
    )
    .unwrap();
    let mut reply = String::new();
    BufReader::new(&mut controller)
        .read_line(&mut reply)
        .unwrap();
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&reply).unwrap()["type"],
        "attached"
    );
    let process = child.0.as_mut().unwrap();
    assert!(
        Command::new("kill")
            .args(["-TERM", &process.id().to_string()])
            .status()
            .unwrap()
            .success()
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while process.try_wait().unwrap().is_none() {
        assert!(Instant::now() < deadline, "capture receiver did not stop");
        thread::sleep(Duration::from_millis(10));
    }
    let mut stderr = String::new();
    process
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut stderr)
        .unwrap();
    assert!(stderr.contains("starting without registrar"));
    assert_eq!(
        TlsIdentity::load_or_create(service_capture.join("tls"))
            .unwrap()
            .public_key_sha256(),
        identity.public_key_sha256()
    );
}
