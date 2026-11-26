//! Consume the manager's fixed first-registration operation.
//!
//! One `R` byte carries exactly one open capture-root directory. The closed
//! v1 reply is `committed` with `receiver_sha256`, `unavailable`, or
//! `root_mismatch`. A committed reply only selects an existing matching TLS
//! identity. No reply is a Send, first-enrollment, or host-authority permit.

use std::{
    fs::File,
    io::{self, Read},
    os::unix::net::UnixStream,
    path::Path,
    time::Duration,
};

use rustix::fs::{Mode, OFlags, open};
use serde::Deserialize;

use crate::{IdentityError, TlsIdentity};

const SOCKET: &str = "/run/louiselm-phone-host/registration.sock";
const MAX_REPLY: u64 = 1024;

#[derive(Deserialize)]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
enum Reply {
    Committed { v: u8, receiver_sha256: String },
    Unavailable { v: u8 },
    RootMismatch { v: u8 },
}

/// A committed result selects an existing key; it does not grant authority.
pub(super) fn query(root: &Path) -> io::Result<Option<String>> {
    query_at(root, Path::new(SOCKET))
}

fn query_at(root: &Path, socket_path: &Path) -> io::Result<Option<String>> {
    let root = match open(
        root,
        OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC,
        Mode::empty(),
    ) {
        Ok(root) => File::from(root),
        Err(rustix::io::Errno::NOENT) => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let socket = match UnixStream::connect(socket_path) {
        Ok(socket) => socket,
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    socket.set_read_timeout(Some(Duration::from_secs(10)))?;
    socket.set_write_timeout(Some(Duration::from_secs(10)))?;
    query_connection(&socket, &root)
}

fn query_connection(socket: &UnixStream, root: &File) -> io::Result<Option<String>> {
    super::managed_pairings::send_root(socket, root, b'R')?;
    let mut response = Vec::new();
    socket.take(MAX_REPLY + 1).read_to_end(&mut response)?;
    if response.len() as u64 > MAX_REPLY {
        return Err(io::Error::other("registration response too large"));
    }
    let reply: Reply = serde_json::from_slice(&response)
        .map_err(|_| io::Error::other("registration response is invalid"))?;
    match reply {
        Reply::Committed {
            v: 1,
            receiver_sha256,
        } if receiver_sha256.len() == 64
            && receiver_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)) =>
        {
            Ok(Some(receiver_sha256))
        }
        Reply::Unavailable { v: 1 } | Reply::RootMismatch { v: 1 } => Ok(None),
        _ => Err(io::Error::other("registration response is invalid")),
    }
}

pub(super) fn select_identity(
    tls: &Path,
    committed: Option<&str>,
) -> Result<TlsIdentity, IdentityError> {
    let Some(expected) = committed else {
        return TlsIdentity::load_or_create(tls);
    };
    let identity = TlsIdentity::load_existing(tls)?;
    if identity.public_key_sha256() != expected {
        return Err(io::Error::other("registered receiver identity differs").into());
    }
    Ok(identity)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "fixture setup and assertions")]
mod tests {
    use std::{
        io::{IoSliceMut, Write},
        mem::MaybeUninit,
        os::unix::fs::{MetadataExt, PermissionsExt},
        thread,
    };

    use rustix::net::{self, RecvAncillaryBuffer, RecvAncillaryMessage, RecvFlags};

    use super::*;

    fn response(root: &File, json: &str) -> io::Result<Option<String>> {
        let (client, mut server) = UnixStream::pair().unwrap();
        let expected = root.metadata().unwrap();
        let json = json.to_owned();
        let worker = thread::spawn(move || {
            let mut byte = [0];
            let mut space = [MaybeUninit::uninit(); rustix::cmsg_space!(ScmRights(2))];
            let mut ancillary = RecvAncillaryBuffer::new(&mut space);
            let message = net::recvmsg(
                &server,
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
            server.write_all(json.as_bytes()).unwrap();
        });
        let result = query_connection(&client, root);
        worker.join().unwrap();
        result
    }

    #[test]
    fn registration_uses_one_held_root_and_closed_versioned_replies() {
        let temporary = tempfile::tempdir().unwrap();
        let root = File::open(temporary.path()).unwrap();
        let fingerprint = "a".repeat(64);
        let committed =
            format!("{{\"v\":1,\"result\":\"committed\",\"receiver_sha256\":\"{fingerprint}\"}}");
        assert_eq!(response(&root, &committed).unwrap(), Some(fingerprint));
        for json in [
            r#"{"v":1,"result":"unavailable"}"#,
            r#"{"v":1,"result":"root_mismatch"}"#,
        ] {
            assert!(response(&root, json).unwrap().is_none());
        }
        for json in [
            r#"{"v":2,"result":"unavailable"}"#,
            r#"{"v":1,"result":"unavailable","extra":true}"#,
            r#"{"v":1,"v":1,"result":"unavailable"}"#,
            r#"{"v":1,"result":"committed","receiver_sha256":"AA"}"#,
            r#"{"v":1,"result":"committed"}"#,
        ] {
            assert!(response(&root, json).is_err());
        }
        assert!(response(&root, &" ".repeat(1025)).is_err());
    }

    #[test]
    fn missing_root_or_service_keeps_capture_only_available() {
        let temporary = tempfile::tempdir().unwrap();
        let socket = temporary.path().join("missing.sock");
        assert!(
            query_at(&temporary.path().join("missing-root"), &socket)
                .unwrap()
                .is_none()
        );
        assert!(query_at(temporary.path(), &socket).unwrap().is_none());
        let file = temporary.path().join("not-a-directory");
        std::fs::write(&file, b"fixture").unwrap();
        assert!(query_at(&file, &socket).is_err());
    }

    #[test]
    fn committed_identity_never_creates_or_accepts_a_different_key() {
        let temporary = tempfile::tempdir().unwrap();
        let existing = temporary.path().join("existing");
        let identity = TlsIdentity::load_or_create(&existing).unwrap();
        assert_eq!(
            select_identity(&existing, Some(identity.public_key_sha256()))
                .unwrap()
                .public_key_sha256(),
            identity.public_key_sha256()
        );
        let wrong = if identity.public_key_sha256() == "a".repeat(64) {
            "b".repeat(64)
        } else {
            "a".repeat(64)
        };
        assert!(select_identity(&existing, Some(&wrong)).is_err());

        let key = existing.join("receiver-key.pem");
        let original_key = std::fs::read(&key).unwrap();
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(select_identity(&existing, Some(identity.public_key_sha256())).is_err());
        assert_eq!(std::fs::read(&key).unwrap(), original_key);
        std::fs::set_permissions(&key, std::fs::Permissions::from_mode(0o600)).unwrap();

        let certificate = existing.join("receiver-cert.pem");
        let original_certificate = std::fs::read(&certificate).unwrap();
        std::fs::write(&certificate, vec![b'x'; 65_537]).unwrap();
        assert!(select_identity(&existing, Some(identity.public_key_sha256())).is_err());
        std::fs::write(&certificate, original_certificate).unwrap();

        let missing = temporary.path().join("missing");
        assert!(select_identity(&missing, Some(&wrong)).is_err());
        assert!(!missing.exists());

        let capture_only = temporary.path().join("capture-only");
        assert!(select_identity(&capture_only, None).is_ok());
    }
}
