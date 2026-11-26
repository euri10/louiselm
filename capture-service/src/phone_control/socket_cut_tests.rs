use super::{ControllerEntry, ControllerRegistry, ControllerSocket, Registration};
use std::{io::Write, os::unix::net::UnixStream, sync::Arc};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[test]
#[allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
fn detach_shuts_the_original_connection_before_discarding_it() {
    let (mut owner, receiver) = UnixStream::pair().unwrap();
    let duplicate = receiver.try_clone().unwrap();
    let registry = Arc::new(ControllerRegistry::default());
    let id = "01".repeat(16);
    let (sender, _responses) = tokio::sync::mpsc::channel(1);
    registry.attached.lock().unwrap().insert(
        id.clone(),
        ControllerEntry {
            sender: Some(sender),
            socket: Some(receiver),
        },
    );
    let registration = Registration {
        registry: Arc::clone(&registry),
        id: id.clone(),
    };
    drop(registration);
    assert!(!registry.contains(&id));
    assert_eq!(
        owner.write(b"gate").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    assert!(duplicate.peer_addr().is_ok());
}

#[test]
#[allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
fn invalidation_cuts_all_active_controller_connections() {
    let registry = ControllerRegistry::default();
    let mut peers = Vec::new();
    for byte in ["01", "02"] {
        let (owner, receiver) = UnixStream::pair().unwrap();
        let (sender, _responses) = tokio::sync::mpsc::channel(1);
        registry.attached.lock().unwrap().insert(
            byte.repeat(16),
            ControllerEntry {
                sender: Some(sender),
                socket: Some(receiver),
            },
        );
        peers.push(owner);
    }
    registry.cut_all().unwrap();
    for mut peer in peers {
        assert_eq!(
            peer.write(b"gate").unwrap_err().kind(),
            std::io::ErrorKind::BrokenPipe
        );
    }
}

#[test]
#[allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
fn failed_shutdown_refuses_dispatch_but_retains_the_handle_for_retry() {
    // A non-socket descriptor injects ENOTSOCK without unsafe code.
    let socket = UnixStream::from(std::os::fd::OwnedFd::from(tempfile::tempfile().unwrap()));
    let (sender, _responses) = tokio::sync::mpsc::channel(1);
    let mut entry = ControllerEntry {
        sender: Some(sender),
        socket: Some(socket),
    };
    assert!(entry.cut().is_err());
    assert!(entry.sender.is_none());
    assert!(entry.socket.is_some());
}

#[test]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    reason = "test poisoning and assertions"
)]
fn poisoned_registry_still_cuts_the_original_connection() {
    let (mut owner, receiver) = UnixStream::pair().unwrap();
    let registry = Arc::new(ControllerRegistry::default());
    let id = "01".repeat(16);
    let (sender, _responses) = tokio::sync::mpsc::channel(1);
    registry.attached.lock().unwrap().insert(
        id.clone(),
        ControllerEntry {
            sender: Some(sender),
            socket: Some(receiver),
        },
    );
    let registration = Registration {
        registry: Arc::clone(&registry),
        id,
    };
    let poisoning = Arc::clone(&registry);
    assert!(
        std::thread::spawn(move || {
            let _guard = poisoning.attached.lock().unwrap();
            panic!("test registry poison");
        })
        .join()
        .is_err()
    );
    assert!(registry.cut_all().is_err());
    drop(registration);
    assert_eq!(
        owner.write(b"gate").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
}

#[test]
#[allow(
    clippy::panic,
    clippy::unwrap_used,
    reason = "test poisoning and assertions"
)]
fn detach_still_cuts_a_connection_when_the_registry_is_poisoned() {
    let (mut owner, receiver) = UnixStream::pair().unwrap();
    let registry = Arc::new(ControllerRegistry::default());
    let id = "01".repeat(16);
    let (sender, _responses) = tokio::sync::mpsc::channel(1);
    registry.attached.lock().unwrap().insert(
        id.clone(),
        ControllerEntry {
            sender: Some(sender),
            socket: Some(receiver),
        },
    );
    let registration = Registration {
        registry: Arc::clone(&registry),
        id,
    };
    let poisoning = Arc::clone(&registry);
    assert!(
        std::thread::spawn(move || {
            let _guard = poisoning.attached.lock().unwrap();
            panic!("test registry poison");
        })
        .join()
        .is_err()
    );
    drop(registration);
    assert_eq!(
        owner.write(b"gate").unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
}

#[tokio::test]
#[allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
async fn invalidation_shuts_a_real_attached_controller_socket() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("controller.sock");
    let capability = root.path().join("operator-capability");
    let registry = Arc::new(ControllerRegistry::default());
    let server = ControllerSocket::bind(&path, &capability, Arc::clone(&registry))
        .await
        .unwrap();
    let token = std::fs::read_to_string(&capability).unwrap();
    let task = tokio::spawn(server.serve());
    let mut peer = tokio::net::UnixStream::connect(&path).await.unwrap();
    peer.write_all(
        format!(
            "{}\n",
            serde_json::json!({"v":1,"type":"attach","capability":token})
        )
        .as_bytes(),
    )
    .await
    .unwrap();
    let mut response = String::new();
    BufReader::new(&mut peer)
        .read_line(&mut response)
        .await
        .unwrap();
    let attached: serde_json::Value = serde_json::from_str(&response).unwrap();
    let id = attached["controller_id"].as_str().unwrap();
    assert!(registry.contains(id));
    registry.cut_all().unwrap();
    assert!(!registry.contains(id));
    assert_eq!(
        peer.write_all(b"gate\n").await.unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    task.abort();
    let _ = task.await;
}
