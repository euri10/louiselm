//! Process death with real evidence locks and transferred descriptors.

#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use std::{
    io::{BufRead, BufReader, IoSlice, Read, Write},
    mem::MaybeUninit,
    os::{fd::AsFd, unix::net::UnixStream},
    path::Path,
    process::{Child, Command, Stdio},
};

use super::{ActionEvidenceKind, ActionEvidenceStore, initialize_receiver_fixture};
use rustix::net::{SendAncillaryBuffer, SendAncillaryMessage, SendFlags, sendmsg};

const RECEIVER: &str = "0202020202020202020202020202020202020202020202020202020202020202";

const KEEP_DESCRIPTOR: &str = r"
import array, os, socket, sys
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(sys.argv[1])
server.listen(1)
server.settimeout(10)
print('LISTENING', flush=True)
peer, _ = server.accept()
data, ancillary, flags, _ = peer.recvmsg(1, socket.CMSG_SPACE(array.array('i').itemsize))
assert data == b'L' and not flags
fds = array.array('i')
for level, kind, payload in ancillary:
    if level == socket.SOL_SOCKET and kind == socket.SCM_RIGHTS:
        fds.frombytes(payload[:array.array('i').itemsize])
assert len(fds) == 1
print('RETAINED', flush=True)
sys.stdin.buffer.read(1)
os.close(fds[0])
";

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            // Only test-owned processes are stopped, including on assertion failure.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn read_until(reader: &mut impl BufRead, marker: &str) {
    let mut line = String::new();
    loop {
        line.clear();
        assert_ne!(reader.read_line(&mut line).unwrap(), 0, "missing {marker}");
        if line.contains(marker) {
            return;
        }
    }
}

#[test]
fn evidence_lock_holder() {
    let Ok(root) = std::env::var("LOUISELM_TEST_EVIDENCE_HOLDER") else {
        return;
    };
    let root = Path::new(&root);
    let store = ActionEvidenceStore::open_bound(
        root.join("actions"),
        root.join("receiver.json"),
        RECEIVER,
        ActionEvidenceKind::Archive,
    )
    .unwrap();
    let lock = if std::env::var("LOUISELM_TEST_EVIDENCE_LOCK").unwrap() == "store" {
        &store.lock
    } else {
        &store.guard_lock.as_ref().unwrap().1
    };
    let peer = UnixStream::connect(root.join("descriptor.sock")).unwrap();
    let rights = [lock.0.file().unwrap().as_fd()];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut control = SendAncillaryBuffer::new(&mut space);
    assert!(control.push(SendAncillaryMessage::ScmRights(&rights)));
    assert_eq!(
        sendmsg(
            &peer,
            &[IoSlice::new(b"L")],
            &mut control,
            SendFlags::empty()
        )
        .unwrap(),
        1
    );
    println!("HOLDER_READY");
    std::io::stdout().flush().unwrap();
    let mut input = Vec::new();
    std::io::stdin().read_to_end(&mut input).unwrap();
}

#[test]
#[ignore = "process-owned evidence lock is not yet implemented"]
fn store_lock_recovers_after_owner_death_with_transferred_descriptor() {
    recover_after_owner_death("store");
}

#[test]
#[ignore = "process-owned receiver-guard lock is not yet implemented"]
fn receiver_guard_lock_recovers_after_owner_death_with_transferred_descriptor() {
    recover_after_owner_death("guard");
}

fn recover_after_owner_death(lock: &str) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    initialize_receiver_fixture(&root.join("actions"), &root.join("receiver.json"), RECEIVER);
    let mut keeper = OwnedChild(
        Command::new("python3")
            .args(["-c", KEEP_DESCRIPTOR])
            .arg(root.join("descriptor.sock"))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    let mut keeper_output = BufReader::new(keeper.0.stdout.take().unwrap());
    read_until(&mut keeper_output, "LISTENING");
    let mut holder = OwnedChild(
        Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "phone_control::evidence::lock_tests::evidence_lock_holder",
                "--nocapture",
            ])
            .env("LOUISELM_TEST_EVIDENCE_HOLDER", root)
            .env("LOUISELM_TEST_EVIDENCE_LOCK", lock)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap(),
    );
    read_until(&mut keeper_output, "RETAINED");
    read_until(
        &mut BufReader::new(holder.0.stdout.take().unwrap()),
        "HOLDER_READY",
    );
    let lock_path = if lock == "store" {
        root.join("actions.lock")
    } else {
        root.join("receiver.json.lock")
    };
    let still_locked = Command::new("python3")
        .args([
            "-c",
            "import fcntl, sys\nf = open(sys.argv[1], 'r+')\ntry:\n fcntl.flock(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\n fcntl.lockf(f, fcntl.LOCK_EX | fcntl.LOCK_NB)\nexcept BlockingIOError:\n sys.exit(0)\nsys.exit(1)",
        ])
        .arg(lock_path)
        .status()
        .unwrap();
    assert!(
        still_locked.success(),
        "descriptor transfer released {lock}"
    );
    holder.0.kill().unwrap();
    holder.0.wait().unwrap();

    let reopened = ActionEvidenceStore::open_bound(
        root.join("actions"),
        root.join("receiver.json"),
        RECEIVER,
        ActionEvidenceKind::Archive,
    );
    keeper.0.stdin.take().unwrap().write_all(b"x").unwrap();
    assert!(keeper.0.wait().unwrap().success());
    assert!(reopened.is_ok(), "{lock} lock survived its owner process");
}
