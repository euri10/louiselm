#![allow(clippy::unwrap_used, reason = "test fixtures and assertions")]

use super::{ControllerEntry, ControllerRegistry, ControllerSocket, Registration};
use std::{io::Write, os::unix::net::UnixStream, sync::Arc};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

async fn attach(
    path: &std::path::Path,
    token: &str,
) -> (tokio::net::UnixStream, serde_json::Value) {
    let mut peer = tokio::net::UnixStream::connect(path).await.unwrap();
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
    (peer, serde_json::from_str(&response).unwrap())
}

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
            gate_capable: false,
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
                gate_capable: false,
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
        gate_capable: false,
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
            gate_capable: false,
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
            gate_capable: false,
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

#[tokio::test]
#[allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
async fn gate_capable_registration_and_cutoff_share_the_real_barrier() {
    let root = tempfile::tempdir().unwrap();
    let (_, generation, owner) = super::super::barrier::receiver_fixture(root.path());
    let path = root.path().join("controller.sock");
    let capability = root.path().join("operator-capability");
    let registry = Arc::new(ControllerRegistry::new(Some((generation, owner))));
    let server = ControllerSocket::bind(&path, &capability, Arc::clone(&registry))
        .await
        .unwrap();
    let token = std::fs::read_to_string(&capability).unwrap();
    let task = tokio::spawn(server.serve());
    let (mut first, response) = attach(&path, &token).await;
    let id = response["controller_id"].as_str().unwrap();
    assert!(
        registry
            .attached
            .lock()
            .unwrap()
            .get(id)
            .unwrap()
            .gate_capable
    );
    registry
        .cutoff(
            super::super::barrier_log::CutoffKind::Disable,
            "11111111-1111-4111-8111-111111111111",
            || {
                std::fs::write(root.path().join("durable-authority"), b"disabled")
                    .map_err(|_| super::super::authority::Refusal::Unavailable)
            },
        )
        .unwrap();
    assert_eq!(
        first.write_all(b"gate\n").await.unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        std::fs::read(root.path().join("durable-authority")).unwrap(),
        b"disabled"
    );
    let (_second, response) = attach(&path, &token).await;
    assert_eq!(response["type"], "attached");
    task.abort();
    let _ = task.await;
}

#[tokio::test]
#[allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
async fn gate_capable_registration_and_authority_invalidation_share_the_real_barrier() {
    let root = tempfile::tempdir().unwrap();
    let (_, generation, owner) = super::super::barrier::receiver_fixture(root.path());
    let path = root.path().join("controller.sock");
    let capability = root.path().join("operator-capability");
    let registry = Arc::new(ControllerRegistry::new(Some((generation, owner))));
    let server = ControllerSocket::bind(&path, &capability, Arc::clone(&registry))
        .await
        .unwrap();
    let token = std::fs::read_to_string(&capability).unwrap();
    let task = tokio::spawn(server.serve());
    let (mut old, response) = attach(&path, &token).await;
    assert_eq!(response["type"], "attached");
    registry
        .invalidate(|| {
            std::fs::write(root.path().join("authority-generation"), b"next")
                .map_err(|_| super::super::authority::Refusal::Unavailable)
        })
        .unwrap();
    assert_eq!(
        old.write_all(b"gate\n").await.unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    assert_eq!(
        std::fs::read(root.path().join("authority-generation")).unwrap(),
        b"next"
    );
    let (_fresh, response) = attach(&path, &token).await;
    assert_eq!(response["type"], "attached");
    task.abort();
    let _ = task.await;
}

#[test]
fn missing_barrier_refuses_authority_invalidation_without_running_it() {
    let registry = ControllerRegistry::new(None);
    let mut called = false;
    assert_eq!(
        registry.invalidate(|| {
            called = true;
            Ok(())
        }),
        Err(super::super::authority::Refusal::Unavailable)
    );
    assert!(!called);
}

#[tokio::test]
#[allow(clippy::unwrap_used, reason = "test fixtures and assertions")]
async fn failed_storage_keeps_new_gate_capable_registrations_closed() {
    let root = tempfile::tempdir().unwrap();
    let (_, generation, owner) = super::super::barrier::receiver_fixture(root.path());
    let path = root.path().join("controller.sock");
    let capability = root.path().join("operator-capability");
    let registry = Arc::new(ControllerRegistry::new(Some((generation, owner))));
    let server = ControllerSocket::bind(&path, &capability, Arc::clone(&registry))
        .await
        .unwrap();
    let token = std::fs::read_to_string(&capability).unwrap();
    let task = tokio::spawn(server.serve());
    assert_eq!(
        registry.cutoff(
            super::super::barrier_log::CutoffKind::Forget,
            "11111111-1111-4111-8111-111111111111",
            || Err::<(), _>(super::super::authority::Refusal::Unavailable),
        ),
        Err(super::super::authority::Refusal::Unavailable)
    );
    let (_peer, response) = attach(&path, &token).await;
    assert_eq!(response["type"], "error");
    assert_eq!(response["code"], "unavailable");
    assert!(registry.attached.lock().unwrap().is_empty());
    task.abort();
    let _ = task.await;
}
