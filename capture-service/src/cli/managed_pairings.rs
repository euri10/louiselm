//! Read existing pairings through an on-demand manager-started service.
//!
//! The output is a read result, not proof of authority to perform phone actions.

use std::{
    env,
    fs::{self, File},
    io::{self, IoSlice, IoSliceMut, Read, Write},
    mem::MaybeUninit,
    os::{
        fd::{AsFd, AsRawFd},
        unix::{fs::MetadataExt, net::UnixStream},
    },
    path::Path,
    time::Duration,
};

use rustix::{
    fs::{Mode, OFlags, openat},
    net::{
        self, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags, ReturnFlags,
        SendAncillaryBuffer, SendAncillaryMessage, SendFlags,
    },
};
use serde::{Deserialize, Serialize};

use crate::{PairingRegistry, PairingStatus};

const SOCKET: &str = "/run/louiselm-phone-host/pairing.sock";
const MAX_REPLY: u64 = 1024 * 1024;

#[derive(Deserialize, Serialize)]
#[serde(
    tag = "result",
    content = "value",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Reply {
    Pairings(PairingStatus),
    RootMismatch {},
    Unavailable {},
}

pub(super) fn query(root: &Path) -> io::Result<PairingStatus> {
    query_at(root, Path::new(SOCKET))
}

fn query_at(root: &Path, socket_path: &Path) -> io::Result<PairingStatus> {
    let root = File::open(root)?;
    let socket = UnixStream::connect(socket_path)
        .map_err(|_| io::Error::other("managed pairing service unavailable"))?;
    socket.set_read_timeout(Some(Duration::from_secs(10)))?;
    socket.set_write_timeout(Some(Duration::from_secs(10)))?;
    query_connection(&socket, &root)
}

fn query_connection(socket: &UnixStream, root: &File) -> io::Result<PairingStatus> {
    let rights = [root.as_fd()];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = SendAncillaryBuffer::new(&mut space);
    if !ancillary.push(SendAncillaryMessage::ScmRights(&rights))
        || net::sendmsg(
            socket,
            &[IoSlice::new(b"P")],
            &mut ancillary,
            SendFlags::empty(),
        )? != 1
    {
        return Err(io::Error::other("managed pairing request unavailable"));
    }
    let mut response = Vec::new();
    socket.take(MAX_REPLY + 1).read_to_end(&mut response)?;
    if response.len() as u64 > MAX_REPLY {
        return Err(io::Error::other("managed pairing response too large"));
    }
    match serde_json::from_slice(&response).map_err(io::Error::other)? {
        Reply::Pairings(status) => Ok(status),
        Reply::RootMismatch {} => Err(io::Error::other("registered root mismatch")),
        Reply::Unavailable {} => Err(io::Error::other("pairing unavailable")),
    }
}

// These descriptors have manager provenance only in the installed PID 1 unit.
// A caller running this command directly cannot turn its output into authority.
pub(super) fn serve() -> io::Result<()> {
    let files = manager_files()?;
    serve_connection(&files[0], &files[1], &files[2], &io::stdin(), io::stdout())
}

fn manager_files() -> io::Result<[File; 3]> {
    let invalid = || io::Error::other("manager descriptors unavailable");
    if env::var("LISTEN_PID").ok().as_deref() != Some(&std::process::id().to_string())
        || env::var("LISTEN_FDS").ok().as_deref() != Some("3")
    {
        return Err(invalid());
    }
    let names = env::var("LISTEN_FDNAMES").map_err(|_| invalid())?;
    let indexes = manager_descriptor_indexes(&names)?;
    let open = |index| File::open(format!("/proc/self/fd/{index}"));
    Ok([open(indexes[0])?, open(indexes[1])?, open(indexes[2])?])
}

fn manager_descriptor_indexes(names: &str) -> io::Result<[usize; 3]> {
    let invalid = || io::Error::other("manager descriptors unavailable");
    // With Accept=yes and StandardInput=socket, systemd 257 includes the
    // stdio connection in FDNAMES but not in FDS or the fd-3 sequence.
    let names = names.strip_prefix("connection:").unwrap_or(names);
    let names: Vec<_> = names.split(':').collect();
    if names.len() != 3 {
        return Err(invalid());
    }
    let index = |name| {
        let matches: Vec<_> = names
            .iter()
            .enumerate()
            .filter(|(_, value)| **value == name)
            .collect();
        if matches.len() != 1 {
            return Err(invalid());
        }
        Ok(matches[0].0 + 3)
    };
    Ok([index("root")?, index("hostproc")?, index("hostnet")?])
}

fn serve_connection(
    root: &File,
    proc: &File,
    host_net: &File,
    input: &impl AsFd,
    mut output: impl Write,
) -> io::Result<()> {
    let reply = match receive_root(input) {
        Ok(requested) if same_directory(root, &requested)? => {
            if host_context(proc, host_net).is_ok() {
                match read_pairings(root) {
                    Ok(status) => Reply::Pairings(status),
                    Err(_) => Reply::Unavailable {},
                }
            } else {
                Reply::Unavailable {}
            }
        }
        Ok(_) => Reply::RootMismatch {},
        Err(_) => Reply::Unavailable {},
    };
    serde_json::to_writer(&mut output, &reply).map_err(io::Error::other)?;
    output.flush()
}

fn receive_root(socket: &impl AsFd) -> io::Result<File> {
    let mut byte = [0];
    let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(1))];
    let mut ancillary = RecvAncillaryBuffer::new(&mut space);
    let message = net::recvmsg(
        socket,
        &mut [IoSliceMut::new(&mut byte)],
        &mut ancillary,
        RecvFlags::CMSG_CLOEXEC,
    )?;
    if message.bytes != 1
        || byte != *b"P"
        || message
            .flags
            .intersects(ReturnFlags::TRUNC | ReturnFlags::CTRUNC)
    {
        return Err(io::Error::other("invalid managed pairing request"));
    }
    let mut descriptors = Vec::new();
    for item in ancillary.drain() {
        match item {
            RecvAncillaryMessage::ScmRights(rights) => descriptors.extend(rights),
            _ => return Err(io::Error::other("invalid managed pairing request")),
        }
    }
    let [descriptor]: [_; 1] = descriptors
        .try_into()
        .map_err(|_| io::Error::other("invalid managed pairing request"))?;
    Ok(File::from(descriptor))
}

fn same_directory(left: &File, right: &File) -> io::Result<bool> {
    let left = left.metadata()?;
    let right = right.metadata()?;
    Ok(left.is_dir() && right.is_dir() && left.dev() == right.dev() && left.ino() == right.ino())
}

fn host_context(proc: &File, host_net: &File) -> io::Result<()> {
    let expected_proc = proc.metadata()?;
    let actual_proc = fs::metadata("/proc")?;
    let current = File::from(openat(
        proc,
        "self/ns/net",
        OFlags::RDONLY | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let current = current.metadata()?;
    let host = host_net.metadata()?;
    if !expected_proc.is_dir()
        || expected_proc.dev() != actual_proc.dev()
        || expected_proc.ino() != actual_proc.ino()
        || current.dev() != host.dev()
        || current.ino() != host.ino()
    {
        return Err(io::Error::other("host context unavailable"));
    }
    Ok(())
}

fn read_pairings(root: &File) -> io::Result<PairingStatus> {
    // Pin the child directory without following a substituted symlink. Every
    // existing pairing-store path is then resolved relative to that held fd.
    let pairing = File::from(openat(
        root,
        "pairing",
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::NOFOLLOW | OFlags::CLOEXEC,
        Mode::empty(),
    )?);
    let path = format!("/proc/self/fd/{}", pairing.as_raw_fd());
    PairingRegistry::open_existing(Path::new(&path))
        .and_then(|registry| registry.status())
        .map_err(io::Error::other)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]
mod tests {
    use super::*;
    use crate::PairingRegistry;
    use std::{fs, os::unix::net::UnixStream, thread};

    #[test]
    fn accepted_socket_name_does_not_shift_openfile_descriptors() {
        assert_eq!(
            manager_descriptor_indexes("connection:root:hostproc:hostnet").unwrap(),
            [3, 4, 5]
        );
        assert!(manager_descriptor_indexes("connection:root:root:hostnet").is_err());
    }

    #[test]
    fn real_pairing_read_uses_held_root_and_accepts_alias_but_rejects_another_root() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("registered");
        fs::create_dir(&root).unwrap();
        let registry = PairingRegistry::open(root.join("pairing")).unwrap();
        let offer = registry
            .issue("https://localhost", &"a".repeat(64), 1, 1000)
            .unwrap();
        registry.consume(&offer.token, "fixture phone", 2).unwrap();
        drop(registry);
        let held = File::open(&root).unwrap();
        let proc = File::open("/proc").unwrap();
        let network = File::open("/proc/self/ns/net").unwrap();
        let alias = temporary.path().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        let other = temporary.path().join("other");
        fs::create_dir(&other).unwrap();
        for path in [&root, &alias, &other] {
            let (client, server) = UnixStream::pair().unwrap();
            let held = held.try_clone().unwrap();
            let proc = proc.try_clone().unwrap();
            let network = network.try_clone().unwrap();
            let worker =
                thread::spawn(move || serve_connection(&held, &proc, &network, &server, &server));
            let result = query_connection(&client, &File::open(path).unwrap());
            worker.join().unwrap().unwrap();
            if path == &other {
                assert!(result.unwrap_err().to_string().contains("root mismatch"));
            } else {
                let status = result.unwrap();
                assert_eq!(status.devices.len(), 1);
                assert_eq!(status.devices[0].device_name, "fixture phone");
            }
        }
    }

    #[test]
    fn held_directory_survives_path_replacement_and_missing_registry_is_not_created() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("registered");
        fs::create_dir(&root).unwrap();
        let held = File::open(&root).unwrap();
        let renamed = temporary.path().join("retained");
        fs::rename(&root, &renamed).unwrap();
        fs::create_dir(&root).unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let requested = File::open(&renamed).unwrap();
        let worker = thread::spawn(move || {
            serve_connection(
                &held,
                &File::open("/proc").unwrap(),
                &File::open("/proc/self/ns/net").unwrap(),
                &server,
                &server,
            )
        });
        let error = query_connection(&client, &requested).unwrap_err();
        worker.join().unwrap().unwrap();
        assert!(error.to_string().contains("pairing unavailable"));
        assert!(!renamed.join("pairing").exists());
        assert!(!root.join("pairing").exists());
    }

    #[test]
    fn renamed_root_still_reads_existing_pairings() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("registered");
        fs::create_dir(&root).unwrap();
        let registry = PairingRegistry::open(root.join("pairing")).unwrap();
        let offer = registry
            .issue("https://localhost", &"a".repeat(64), 1, 1000)
            .unwrap();
        registry.consume(&offer.token, "retained phone", 2).unwrap();
        drop(registry);
        let held = File::open(&root).unwrap();
        let renamed = temporary.path().join("retained");
        fs::rename(&root, &renamed).unwrap();
        fs::create_dir(&root).unwrap();
        let requested = File::open(&renamed).unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            serve_connection(
                &held,
                &File::open("/proc").unwrap(),
                &File::open("/proc/self/ns/net").unwrap(),
                &server,
                &server,
            )
        });
        let status = query_connection(&client, &requested).unwrap();
        worker.join().unwrap().unwrap();
        assert_eq!(status.devices[0].device_name, "retained phone");
        assert!(!root.join("pairing").exists());
    }

    #[test]
    fn absent_service_and_wrong_network_reference_fail_closed() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        let error = query_at(root, &root.join("absent.sock")).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("managed pairing service unavailable")
        );
        assert!(!root.join("pairing").exists());
        let registry = PairingRegistry::open(root.join("pairing")).unwrap();
        let offer = registry
            .issue("https://localhost", &"a".repeat(64), 1, 1000)
            .unwrap();
        registry.consume(&offer.token, "fixture phone", 2).unwrap();
        drop(registry);
        let previous = fs::read(root.join("pairing/pairing.json")).unwrap();
        assert_eq!(
            read_pairings(&File::open(root).unwrap())
                .unwrap()
                .devices
                .len(),
            1
        );
        let held = File::open(root).unwrap();
        let requested = held.try_clone().unwrap();
        let (client, server) = UnixStream::pair().unwrap();
        let worker = thread::spawn(move || {
            serve_connection(
                &held,
                &File::open("/proc").unwrap(),
                &File::open("/proc/self/ns/mnt").unwrap(),
                &server,
                &server,
            )
        });
        let error = query_connection(&client, &requested).unwrap_err();
        worker.join().unwrap().unwrap();
        assert!(error.to_string().contains("pairing unavailable"));
        assert_eq!(
            fs::read(root.join("pairing/pairing.json")).unwrap(),
            previous
        );
    }
}
