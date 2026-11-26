//! Durable owner admission versus no-effect fencing.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use std::{
    fs,
    io::{Read, Write},
    os::unix::{
        fs::symlink,
        net::{UnixListener, UnixStream},
    },
    process::{Child, Command, Stdio},
    sync::{Arc, Barrier},
    thread,
    time::{Duration, Instant},
};

use louiselm_capture::ActionIdentity;

#[path = "support/phone_evidence.rs"]
#[allow(
    dead_code,
    reason = "other evidence fixtures share this support module"
)]
mod phone_evidence;

fn initialize(root: &std::path::Path) -> String {
    let tls = root.join("louiselm/capture/tls");
    let identity = louiselm_capture::TlsIdentity::load_or_create(&tls).unwrap();
    let receiver = identity.public_key_sha256().to_owned();
    phone_evidence::initialize(
        &root.join("louiselm/workflow/phone-action-owner"),
        &tls.join("phone-action-owner.json"),
        &receiver,
        louiselm_capture::ActionEvidenceKind::Owner,
    );
    phone_evidence::initialize_barrier(&tls, &receiver);
    phone_evidence::bind_barrier_to_owner(&tls);
    receiver
}

fn identity(project: &std::path::Path, id: u8) -> ActionIdentity {
    ActionIdentity {
        operation_id: format!("{id:02x}").repeat(16),
        receiver_identity: louiselm_capture::TlsIdentity::load_or_create(
            project.join("louiselm/capture/tls"),
        )
        .unwrap()
        .public_key_sha256()
        .to_owned(),
        controller_id: "03".repeat(16),
        subject_id: "04".repeat(16),
        action: 3,
        payload_hash: "05".repeat(32),
        grant_id: "06".repeat(16),
        grant_revision: 1,
        period_id: "07".repeat(16),
        expected_revision: 2,
        project: fs::canonicalize(project).unwrap().to_str().unwrap().into(),
        agent: "codex".into(),
        recorded_at_ms: 1_000,
    }
}

fn spawn(root: &std::path::Path) -> Child {
    Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
        .args(["phone-control", "owner-journal"])
        .env_clear()
        .env("LOUISELM_CAPTURE_CONFIG_DIR", root)
        .env("LOUISELM_CAPTURE_DATA_DIR", root)
        .env("LOUISELM_CAPTURE_STATE_DIR", root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap()
}

fn send(mut child: Child, message: &serde_json::Value) -> serde_json::Value {
    child
        .stdin
        .take()
        .unwrap()
        .write_all(message.to_string().as_bytes())
        .unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success());
    serde_json::from_slice(&output.stdout).unwrap()
}

fn invoke(root: &std::path::Path, message: &serde_json::Value) -> serde_json::Value {
    send(spawn(root), message)
}

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn start_owner(root: &std::path::Path, message: &serde_json::Value) -> OwnedChild {
    let mut child = OwnedChild(spawn(root));
    child
        .0
        .stdin
        .take()
        .unwrap()
        .write_all(message.to_string().as_bytes())
        .unwrap();
    child
}

fn finish_owner(child: &mut OwnedChild) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            assert!(status.success());
            break;
        }
        assert!(Instant::now() < deadline, "owner journal did not return");
        thread::sleep(Duration::from_millis(10));
    }
    let mut output = Vec::new();
    child
        .0
        .stdout
        .take()
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    serde_json::from_slice(&output).unwrap()
}

fn invoke_bounded(root: &std::path::Path, message: &serde_json::Value) -> serde_json::Value {
    finish_owner(&mut start_owner(root, message))
}

fn accept_child(listener: &UnixListener, child: &mut OwnedChild) -> UnixStream {
    listener.set_nonblocking(true).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let peer = loop {
        match listener.accept() {
            Ok((peer, _)) => break peer,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "test owner did not connect");
                assert!(child.0.try_wait().unwrap().is_none(), "test owner exited");
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!("test socket accept failed: {error}"),
        }
    };
    peer.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    peer
}

#[test]
fn losing_journal_and_lock_never_reopens_the_same_action() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let request = identity(temp.path(), 9);
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    assert_eq!(invoke(temp.path(), &begin)["type"], "begun");

    let owner = temp.path().join("louiselm/workflow/phone-action-owner");
    fs::remove_dir_all(&owner).unwrap();
    fs::remove_file(owner.with_extension("lock")).unwrap();

    let retry = invoke(temp.path(), &begin);
    assert_eq!(retry["type"], "error");
    assert_eq!(retry["code"], "unavailable");
}

#[test]
fn established_receiver_without_a_registration_barrier_refuses_begin() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let tls = temp.path().join("louiselm/capture/tls");
    fs::remove_file(tls.join("phone-admission-barrier.json")).unwrap();
    fs::remove_file(tls.join("phone-admission-barrier.lock")).unwrap();
    let request = identity(temp.path(), 16);
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});

    let response = invoke(temp.path(), &begin);
    assert_eq!(response["type"], "error");
    assert_eq!(response["code"], "unavailable");
}

#[test]
fn concurrent_ambiguous_first_starts_never_create_a_fresh_journal() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let request = identity(temp.path(), 26);
    let message =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    let tls = temp.path().join("louiselm/capture/tls");
    let journal = temp.path().join("louiselm/workflow/phone-action-owner");
    fs::remove_file(tls.join("phone-admission-barrier.json")).unwrap();
    fs::remove_file(tls.join("phone-admission-barrier.lock")).unwrap();
    fs::remove_dir_all(&journal).unwrap();
    fs::remove_file(journal.with_extension("lock")).unwrap();
    let start = Arc::new(Barrier::new(3));
    let children = [spawn(temp.path()), spawn(temp.path())];
    let joins: Vec<_> = children
        .into_iter()
        .map(|child| {
            let start = start.clone();
            let message = message.clone();
            thread::spawn(move || {
                start.wait();
                let mut child = OwnedChild(child);
                child
                    .0
                    .stdin
                    .take()
                    .unwrap()
                    .write_all(message.to_string().as_bytes())
                    .unwrap();
                finish_owner(&mut child)
            })
        })
        .collect();
    start.wait();
    for join in joins {
        assert_eq!(join.join().unwrap()["code"], "unavailable");
    }
    assert!(!journal.exists());
    assert!(!journal.with_extension("lock").exists());
    assert!(!tls.join("phone-admission-barrier.json").exists());
    assert!(!tls.join("phone-admission-barrier.lock").exists());
}

#[test]
fn owner_cli_rejects_a_self_consistent_replacement_barrier() {
    let temp = tempfile::tempdir().unwrap();
    let receiver = initialize(temp.path());
    let tls = temp.path().join("louiselm/capture/tls");
    let snapshot = temp
        .path()
        .join("louiselm/workflow/phone-action-owner/actions.json");
    let before = fs::read(&snapshot).unwrap();
    fs::rename(
        tls.join("phone-admission-barrier.json"),
        tls.join("old-anchor"),
    )
    .unwrap();
    fs::rename(
        tls.join("phone-admission-barrier.lock"),
        tls.join("old-lock"),
    )
    .unwrap();
    // Rebuilding a syntactically correct record is not proof of cutoff.
    phone_evidence::initialize_barrier(&tls, &receiver);
    let request = identity(temp.path(), 20);
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    assert_eq!(invoke(temp.path(), &begin)["code"], "unavailable");
    assert_eq!(fs::read(snapshot).unwrap(), before);
}

#[test]
fn owner_cli_rejects_rebound_lock_or_nonce_in_the_original_anchor() {
    for fault in ["lock", "nonce"] {
        let temp = tempfile::tempdir().unwrap();
        let receiver = initialize(temp.path());
        let tls = temp.path().join("louiselm/capture/tls");
        let snapshot = temp
            .path()
            .join("louiselm/workflow/phone-action-owner/actions.json");
        let before = fs::read(&snapshot).unwrap();
        if fault == "lock" {
            fs::rename(
                tls.join("phone-admission-barrier.lock"),
                tls.join("old-lock"),
            )
            .unwrap();
            phone_evidence::initialize_barrier(&tls, &receiver);
        } else {
            phone_evidence::initialize_barrier_with_nonce(&tls, &receiver, &"cd".repeat(16));
        }
        let request = identity(temp.path(), 21);
        let begin = serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
        assert_eq!(
            invoke(temp.path(), &begin)["code"],
            "unavailable",
            "{fault}"
        );
        assert_eq!(fs::read(snapshot).unwrap(), before, "{fault}");
    }
}

#[test]
fn owner_cli_refuses_a_guard_without_an_independent_barrier_binding() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let guard = temp
        .path()
        .join("louiselm/capture/tls/phone-action-owner.json");
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&guard).unwrap()).unwrap();
    record.as_object_mut().unwrap().remove("barrier");
    fs::write(guard, serde_json::to_vec(&record).unwrap()).unwrap();
    let request = identity(temp.path(), 22);
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    assert_eq!(invoke(temp.path(), &begin)["code"], "unavailable");
}

#[test]
fn owner_cli_refuses_an_evidence_lock_alias_to_the_barrier_lock() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let tls = temp.path().join("louiselm/capture/tls");
    let guard_lock = tls.join("phone-action-owner.json.lock");
    fs::remove_file(&guard_lock).unwrap();
    symlink("phone-admission-barrier.lock", &guard_lock).unwrap();
    let request = identity(temp.path(), 23);
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    assert_eq!(invoke(temp.path(), &begin)["code"], "unavailable");
}

#[cfg(target_os = "linux")]
#[test]
fn barrier_owner_death_releases_lock_with_a_surviving_duplicate() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let tls = temp.path().join("louiselm/capture/tls");
    let duplicate = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(tls.join("phone-admission-barrier.lock"))
        .unwrap();
    let socket_path = temp.path().join("lock-owner.sock");
    let listener = UnixListener::bind(&socket_path).unwrap();
    let mut holder = OwnedChild(
        Command::new("python3")
            .args([
                "-c",
                "import fcntl, os, signal, socket, sys\nfcntl.lockf(0, fcntl.LOCK_EX | fcntl.LOCK_NB)\ns = socket.socket(socket.AF_UNIX)\ns.connect(sys.argv[1])\ns.sendall(b'R')\nos.kill(os.getpid(), signal.SIGSTOP)",
            ])
            .arg(&socket_path)
            .stdin(Stdio::from(duplicate.try_clone().unwrap()))
            .stdout(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut peer = accept_child(&listener, &mut holder);
    let mut ready = [0];
    peer.read_exact(&mut ready).unwrap();
    assert_eq!(ready, *b"R");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let status = fs::read_to_string(format!("/proc/{}/status", holder.0.id())).unwrap();
        if status.lines().any(|line| line.starts_with("State:\tT")) {
            break;
        }
        assert!(Instant::now() < deadline, "test owner did not stop");
        thread::sleep(Duration::from_millis(10));
    }
    let request = identity(temp.path(), 24);
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    let snapshot = temp
        .path()
        .join("louiselm/workflow/phone-action-owner/actions.json");
    let before = fs::read(&snapshot).unwrap();
    let started = Instant::now();
    assert_eq!(invoke_bounded(temp.path(), &begin)["code"], "unavailable");
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(fs::read(&snapshot).unwrap(), before);
    holder.0.kill().unwrap();
    holder.0.wait().unwrap();
    assert!(duplicate.metadata().is_ok());
    assert_eq!(invoke_bounded(temp.path(), &begin)["type"], "begun");
    drop(duplicate);
}

#[test]
fn owner_report_waits_for_the_generation_barrier_before_writing() {
    const HOLD: &str = r"
import fcntl, socket, sys
peer = socket.socket(socket.AF_UNIX)
peer.connect(sys.argv[1])
with open(sys.argv[2], 'r+') as lock:
    fcntl.lockf(lock, fcntl.LOCK_EX)
    peer.sendall(b'R')
    assert peer.recv(1) == b'S'
";
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let request = identity(temp.path(), 26);
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    assert_eq!(invoke_bounded(temp.path(), &begin)["type"], "begun");
    let report = serde_json::json!({
        "v":1,"type":"report","identity":request,
        "outcome":{"accepted":{"evidence":"0a".repeat(32)}},"at_ms":2000
    });
    let snapshot = temp
        .path()
        .join("louiselm/workflow/phone-action-owner/actions.json");
    let before = fs::read(&snapshot).unwrap();
    let socket = temp.path().join("hold.sock");
    let listener = UnixListener::bind(&socket).unwrap();
    let mut holder = OwnedChild(
        Command::new("python3")
            .args(["-c", HOLD])
            .arg(&socket)
            .arg(
                temp.path()
                    .join("louiselm/capture/tls/phone-admission-barrier.lock"),
            )
            .stdin(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let mut peer = accept_child(&listener, &mut holder);
    let mut ready = [0];
    peer.read_exact(&mut ready).unwrap();
    assert_eq!(ready, *b"R");
    assert_eq!(invoke_bounded(temp.path(), &report)["code"], "unavailable");
    assert_eq!(fs::read(&snapshot).unwrap(), before);
    peer.write_all(b"S").unwrap();
    assert!(holder.0.wait().unwrap().success());
    assert_eq!(invoke_bounded(temp.path(), &report)["type"], "recorded");
}

#[test]
fn real_generation_change_and_durable_begin_are_exclusive() {
    const RESET: &str = r"
import fcntl, hashlib, json, os, socket, sys
peer = socket.socket(socket.AF_UNIX)
peer.connect(sys.argv[1])
peer.sendall(b'R')
assert peer.recv(1) == b'S'
with open(sys.argv[2], 'r+') as lock:
    fcntl.lockf(lock, fcntl.LOCK_EX)
    with open(sys.argv[3]) as source:
        seen = sys.argv[5] in json.load(source)['actions']
    with open(sys.argv[4]) as source:
        anchor = json.load(source)
    anchor['body']['generation'] = '0a' * 16
    encoded = json.dumps(anchor['body'], separators=(',', ':')).encode()
    anchor['sha256'] = hashlib.sha256(encoded).hexdigest()
    with open(sys.argv[4], 'w') as target:
        json.dump(anchor, target, separators=(',', ':'))
        target.flush()
        os.fsync(target.fileno())
    peer.sendall(b'1' if seen else b'0')
";
    for order in ["begin_first", "reset_first", "race", "race", "race", "race"] {
        let temp = tempfile::tempdir().unwrap();
        initialize(temp.path());
        let request = identity(temp.path(), 25);
        let begin = serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
        let tls = temp.path().join("louiselm/capture/tls");
        let socket = temp.path().join("reset.sock");
        let listener = UnixListener::bind(&socket).unwrap();
        let mut resetter = OwnedChild(
            Command::new("python3")
                .args(["-c", RESET])
                .arg(&socket)
                .arg(tls.join("phone-admission-barrier.lock"))
                .arg(
                    temp.path()
                        .join("louiselm/workflow/phone-action-owner/actions.json"),
                )
                .arg(tls.join("phone-admission-barrier.json"))
                .arg(&request.operation_id)
                .stdin(Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut peer = accept_child(&listener, &mut resetter);
        let mut ready = [0];
        peer.read_exact(&mut ready).unwrap();
        assert_eq!(ready, *b"R");
        let first = (order == "begin_first").then(|| invoke_bounded(temp.path(), &begin));
        let mut racing = (order == "race").then(|| start_owner(temp.path(), &begin));
        peer.write_all(b"S").unwrap();
        let mut seen = [0];
        peer.read_exact(&mut seen).unwrap();
        assert!(resetter.0.wait().unwrap().success());
        let result = if let Some(result) = first {
            result
        } else if let Some(ref mut child) = racing {
            finish_owner(child)
        } else {
            invoke_bounded(temp.path(), &begin)
        };
        match result["type"].as_str() {
            Some("begun") => assert_eq!(seen, *b"1", "{order}: {result}"),
            Some("error") => {
                assert_eq!(seen, *b"0", "{order}: {result}");
                assert_eq!(result["code"], "unavailable");
            }
            _ => panic!("{order}: {result}"),
        }
        if order == "begin_first" {
            assert_eq!(seen, *b"1");
        } else if order == "reset_first" {
            assert_eq!(seen, *b"0");
        }
    }
}

#[test]
fn owner_cli_refuses_lost_replaced_or_corrupt_barrier_files() {
    for fault in [
        "missing_anchor",
        "missing_lock",
        "replaced_anchor",
        "replaced_lock",
        "corrupt_anchor",
        "corrupt_lock",
    ] {
        let temp = tempfile::tempdir().unwrap();
        initialize(temp.path());
        let tls = temp.path().join("louiselm/capture/tls");
        let anchor = tls.join("phone-admission-barrier.json");
        let lock = tls.join("phone-admission-barrier.lock");
        let snapshot = temp
            .path()
            .join("louiselm/workflow/phone-action-owner/actions.json");
        let before = fs::read(&snapshot).unwrap();
        match fault {
            "missing_anchor" => fs::remove_file(&anchor).unwrap(),
            "missing_lock" => fs::remove_file(&lock).unwrap(),
            "replaced_anchor" => {
                let bytes = fs::read(&anchor).unwrap();
                fs::rename(&anchor, tls.join("old-anchor")).unwrap();
                fs::write(&anchor, bytes).unwrap();
            }
            "replaced_lock" => {
                let bytes = fs::read(&lock).unwrap();
                fs::rename(&lock, tls.join("old-lock")).unwrap();
                fs::write(&lock, bytes).unwrap();
            }
            "corrupt_anchor" => fs::write(&anchor, b"{}").unwrap(),
            "corrupt_lock" => fs::write(&lock, b"corrupt").unwrap(),
            _ => unreachable!(),
        }
        let request = identity(temp.path(), 17);
        let begin = serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
        let response = invoke(temp.path(), &begin);
        assert_eq!(response["code"], "unavailable", "fault: {fault}");
        assert_eq!(fs::read(snapshot).unwrap(), before, "fault: {fault}");
    }
}

#[test]
fn owner_cli_rejects_stale_and_mismatched_generations_before_begin() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let request = identity(temp.path(), 18);
    let stale =
        serde_json::json!({"v":2,"type":"begin","generation":"08".repeat(16),"identity":request});
    assert_eq!(invoke(temp.path(), &stale)["code"], "unavailable");
    let valid =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    assert_eq!(invoke(temp.path(), &valid)["type"], "begun");

    let guard = temp
        .path()
        .join("louiselm/capture/tls/phone-action-owner.json");
    let mut record: serde_json::Value = serde_json::from_slice(&fs::read(&guard).unwrap()).unwrap();
    record["generation"] = serde_json::json!("0a".repeat(16));
    fs::write(&guard, serde_json::to_vec(&record).unwrap()).unwrap();
    let other = identity(temp.path(), 19);
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":other});
    assert_eq!(invoke(temp.path(), &begin)["code"], "unavailable");
}

#[test]
fn owner_commands_refuse_a_missing_or_corrupt_receiver_guard() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let request = identity(temp.path(), 10);
    let guard = temp
        .path()
        .join("louiselm/capture/tls/phone-action-owner.json");
    let messages = [
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request}),
        serde_json::json!({"v":1,"type":"inspect","identity":request}),
        serde_json::json!({"v":2,"type":"fence","generation":"09".repeat(16),"identity":request,"at_ms":2000}),
        serde_json::json!({"v":1,"type":"report","identity":request,"outcome":"unknown","at_ms":2000}),
    ];
    fs::remove_file(&guard).unwrap();
    for message in &messages {
        assert_eq!(invoke(temp.path(), message)["code"], "unavailable");
    }
    fs::write(&guard, b"{}").unwrap();
    for message in &messages {
        assert_eq!(invoke(temp.path(), message)["code"], "unavailable");
    }
}

#[test]
fn owner_cli_recovers_a_proven_interrupted_update_and_rejects_rollback() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let original = identity(temp.path(), 11);
    let second = identity(temp.path(), 12);
    let command = |request: &ActionIdentity| serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    assert_eq!(invoke(temp.path(), &command(&original))["type"], "begun");
    let path = temp.path().join("louiselm/workflow/phone-action-owner");
    let guard_path = temp
        .path()
        .join("louiselm/capture/tls/phone-action-owner.json");
    let previous = fs::read(path.join("actions.json")).unwrap();
    let previous_seal = fs::read(path.join("seal.json")).unwrap();
    let mut previous_guard: serde_json::Value =
        serde_json::from_slice(&fs::read(&guard_path).unwrap()).unwrap();
    assert_eq!(invoke(temp.path(), &command(&second))["type"], "begun");
    let next = fs::read(path.join("actions.json")).unwrap();
    let next_guard: serde_json::Value =
        serde_json::from_slice(&fs::read(&guard_path).unwrap()).unwrap();

    fs::write(path.join("actions.json"), &next).unwrap();
    fs::write(path.join("seal.json"), &previous_seal).unwrap();
    fs::write(path.join("pending-before.json"), &previous).unwrap();
    fs::write(path.join("pending-after.json"), &next).unwrap();
    previous_guard["pending"] =
        serde_json::json!({"bytes":next_guard["bytes"],"sha256":next_guard["sha256"]});
    fs::write(&guard_path, serde_json::to_vec(&previous_guard).unwrap()).unwrap();
    let pending_guard = fs::read(&guard_path).unwrap();
    let cutoff = temp
        .path()
        .join("louiselm/capture/tls/phone-admission-cutoff.json");
    let retained_cutoff = temp.path().join("retained-cutoff");
    fs::rename(&cutoff, &retained_cutoff).unwrap();
    let inspect = serde_json::json!({"v":1,"type":"inspect","identity":original});
    assert_eq!(invoke(temp.path(), &inspect)["code"], "unavailable");
    assert_eq!(fs::read(&guard_path).unwrap(), pending_guard);
    assert_eq!(fs::read(path.join("seal.json")).unwrap(), previous_seal);
    fs::rename(retained_cutoff, cutoff).unwrap();
    assert_eq!(invoke(temp.path(), &command(&original))["type"], "existing");
    assert_eq!(invoke(temp.path(), &command(&second))["type"], "existing");

    fs::write(path.join("actions.json"), previous).unwrap();
    fs::write(path.join("seal.json"), previous_seal).unwrap();
    assert_eq!(
        invoke(temp.path(), &command(&original))["code"],
        "unavailable"
    );
    assert_eq!(
        invoke(temp.path(), &command(&second))["code"],
        "unavailable"
    );
}

#[test]
fn owner_can_report_proven_evidence_when_capture_archive_is_lost() {
    let temp = tempfile::tempdir().unwrap();
    let receiver = initialize(temp.path());
    let archive = temp
        .path()
        .join("louiselm/capture/phone-control/action-evidence");
    phone_evidence::initialize(
        &archive,
        &temp
            .path()
            .join("louiselm/capture/tls/phone-action-evidence.json"),
        &receiver,
        louiselm_capture::ActionEvidenceKind::Archive,
    );
    let request = identity(temp.path(), 13);
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    assert_eq!(invoke(temp.path(), &begin)["type"], "begun");
    fs::remove_dir_all(&archive).unwrap();
    fs::remove_file(archive.with_extension("lock")).unwrap();

    let outcome = serde_json::json!({"accepted":{"evidence":"aa".repeat(32)}});
    let report = serde_json::json!({"v":1,"type":"report","identity":request,"outcome":outcome,"at_ms":2000});
    assert_eq!(invoke(temp.path(), &report)["type"], "recorded");
    let inspect = serde_json::json!({"v":1,"type":"inspect","identity":request});
    assert_eq!(invoke(temp.path(), &inspect)["outcome"], outcome);
}

#[test]
fn caller_cannot_choose_a_different_receiver_identity() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let request = identity(temp.path(), 14);
    let mut wrong = request.clone();
    let replacement = if wrong.receiver_identity.starts_with("00") {
        "01"
    } else {
        "00"
    };
    wrong.receiver_identity.replace_range(..2, replacement);
    let attempt =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":wrong});
    assert_eq!(invoke(temp.path(), &attempt)["code"], "unavailable");
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    assert_eq!(invoke(temp.path(), &begin)["type"], "begun");
}

#[test]
fn capture_guard_cannot_initialize_the_native_owner_journal() {
    let temp = tempfile::tempdir().unwrap();
    let receiver = initialize(temp.path());
    let tls = temp.path().join("louiselm/capture/tls");
    let archive = temp
        .path()
        .join("louiselm/capture/phone-control/action-evidence");
    let capture_guard = tls.join("phone-action-evidence.json");
    phone_evidence::initialize(
        &archive,
        &capture_guard,
        &receiver,
        louiselm_capture::ActionEvidenceKind::Archive,
    );
    fs::copy(&capture_guard, tls.join("phone-action-owner.json")).unwrap();
    let request = identity(temp.path(), 15);
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    assert_eq!(invoke(temp.path(), &begin)["code"], "unavailable");
}

#[test]
fn local_owner_command_persists_a_fence_across_processes() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let request = identity(temp.path(), 5);
    let fenced = invoke(
        temp.path(),
        &serde_json::json!({"v":2,"type":"fence","generation":"09".repeat(16),"identity":request,"at_ms":2000}),
    );
    assert_eq!(fenced["type"], "fenced");
    let late = invoke(
        temp.path(),
        &serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request}),
    );
    assert_eq!(late["type"], "existing");
    assert_eq!(
        late["outcome"]["fenced_no_effect"]["evidence"],
        fenced["evidence"]
    );
}

#[test]
fn inspection_preserves_missing_and_owner_proved_outcomes_across_processes() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let request = identity(temp.path(), 6);
    let inspect = |request: &ActionIdentity| {
        invoke(
            temp.path(),
            &serde_json::json!({"v":1,"type":"inspect","identity":request}),
        )
    };
    assert_eq!(inspect(&request)["code"], "unavailable");
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    assert_eq!(invoke(temp.path(), &begin)["type"], "begun");
    assert_eq!(inspect(&request)["outcome"], "unknown");

    let mut altered = request.clone();
    altered.controller_id = "0a".repeat(16);
    assert_eq!(inspect(&altered)["code"], "conflict");
    let fence = serde_json::json!({"fenced_no_effect":{"evidence":"0b".repeat(32)}});
    let report =
        serde_json::json!({"v":1,"type":"report","identity":request,"outcome":fence,"at_ms":2000});
    assert_eq!(invoke(temp.path(), &report)["type"], "recorded");
    assert_eq!(inspect(&request)["outcome"], fence);
    assert_eq!(invoke(temp.path(), &begin)["outcome"], fence);
}

#[test]
fn separate_helpers_cannot_both_begin_and_fence() {
    let temp = tempfile::tempdir().unwrap();
    initialize(temp.path());
    let seed = identity(temp.path(), 7);
    assert_eq!(
        invoke(
            temp.path(),
            &serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":seed})
        )["type"],
        "begun"
    );
    let request = identity(temp.path(), 8);
    let begin =
        serde_json::json!({"v":2,"type":"begin","generation":"09".repeat(16),"identity":request});
    let fence = serde_json::json!({"v":2,"type":"fence","generation":"09".repeat(16),"identity":request,"at_ms":2000});
    let barrier = Arc::new(Barrier::new(2));
    let children = [spawn(temp.path()), spawn(temp.path())];
    let handles: Vec<_> = children
        .into_iter()
        .zip([begin.clone(), fence])
        .map(|(child, message)| {
            let barrier = Arc::clone(&barrier);
            thread::spawn(move || {
                barrier.wait();
                send(child, &message)
            })
        })
        .collect();
    let results: Vec<_> = handles
        .into_iter()
        .map(|handle| handle.join().unwrap())
        .collect();
    let won_begin = results[0]["type"] == "begun";
    let fenced = results[1]["type"] == "fenced";
    assert_ne!(
        won_begin, fenced,
        "exactly one helper must win: {results:?}"
    );
    let retained = invoke(temp.path(), &begin);
    assert_eq!(retained["type"], "existing");
    if fenced {
        assert_eq!(
            retained["outcome"]["fenced_no_effect"]["evidence"],
            results[1]["evidence"]
        );
    } else {
        assert_eq!(retained["outcome"], "unknown");
        assert_eq!(
            invoke(
                temp.path(),
                &serde_json::json!({"v":2,"type":"fence","generation":"09".repeat(16),"identity":request,"at_ms":3000})
            )["type"],
            "uncertain"
        );
    }
}
