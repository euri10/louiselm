#![allow(clippy::unwrap_used, reason = "test fixtures and assertions")]

use super::{ControllerEntry, ControllerRegistry, ControllerSocket, Registration};
use std::{io::Write, os::unix::net::UnixStream, sync::Arc};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

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

#[tokio::test]
async fn ordinary_service_reads_and_reconnects_after_shutdown() {
    let root = tempfile::tempdir().unwrap();
    let pairing = Arc::new(crate::PairingRegistry::open(root.path().join("pairing")).unwrap());
    // This capture-only fixture has no separately provisioned managed authority.
    let service = super::super::Service::open(
        &root.path().join("phone-control/grants"),
        &"02".repeat(32),
        pairing,
    )
    .unwrap();
    let admin = root.path().join("admin.sock");
    let path = root.path().join("controller.sock");
    let capability = root.path().join("operator-capability");
    let sockets = service.bind(&admin, &path, &capability).await.unwrap();
    let competing = service
        .bind(&admin, &path, &capability)
        .await
        .err()
        .unwrap();
    assert_eq!(competing.kind(), std::io::ErrorKind::AddrInUse);
    let token = std::fs::read_to_string(&capability).unwrap();
    let task = tokio::spawn(sockets.serve());
    let (first, second) = tokio::join!(attach(&path, &token), attach(&path, &token));
    let (mut first, first_reply) = first;
    let (mut second, second_reply) = second;
    let first_id = first_reply["controller_id"].as_str().unwrap();
    let second_id = second_reply["controller_id"].as_str().unwrap();
    assert_eq!(first_reply["type"], "attached");
    assert_eq!(second_reply["type"], "attached");
    assert_ne!(first_id, second_id);
    let read = tokio::spawn({
        let registry = Arc::clone(&service.controllers);
        let id = first_id.to_owned();
        async move {
            registry
                .dispatch(&id, br#"{"type":"read_session"}"#.to_vec())
                .await
        }
    });
    let mut frame = String::new();
    BufReader::new(&mut first)
        .read_line(&mut frame)
        .await
        .unwrap();
    assert_eq!(frame, "{\"type\":\"read_session\"}\n");
    first.write_all(b"read response\n").await.unwrap();
    assert_eq!(read.await.unwrap().unwrap(), b"read response");
    for id in [first_id, second_id] {
        assert!(service.controllers.contains(id));
        for kind in ["send_prompt", "action_commit"] {
            let frame = serde_json::to_vec(&serde_json::json!({"type":kind})).unwrap();
            assert_eq!(
                service.controllers.dispatch(id, frame).await,
                Err(super::super::authority::Refusal::Unavailable)
            );
        }
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    for peer in [&mut first, &mut second] {
        let mut byte = [0];
        let count = tokio::time::timeout(std::time::Duration::from_secs(2), peer.read(&mut byte))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(count, 0);
    }
    assert!(!service.controllers.contains(first_id));
    assert!(!service.controllers.contains(second_id));
    let sockets = service.bind(&admin, &path, &capability).await.unwrap();
    let task = tokio::spawn(sockets.serve());
    let (_peer, reply) = attach(&path, &token).await;
    assert_eq!(reply["type"], "attached");
    assert_ne!(reply["controller_id"], first_id);
    assert_ne!(reply["controller_id"], second_id);
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn service_without_registrar_retains_reads_and_refuses_send() {
    let root = tempfile::tempdir().unwrap();
    let pairing = Arc::new(crate::PairingRegistry::open(root.path().join("pairing")).unwrap());
    let service = super::super::Service::open_without_registrar(
        &root.path().join("phone-control/grants"),
        &"02".repeat(32),
        pairing,
    )
    .unwrap();
    assert!(service.controllers.barrier.is_none());
    let admin = root.path().join("admin.sock");
    let socket = root.path().join("controller.sock");
    let capability = root.path().join("operator-capability");
    let sockets = service.bind(&admin, &socket, &capability).await.unwrap();
    let token = std::fs::read_to_string(capability).unwrap();
    let task = tokio::spawn(sockets.serve());
    let (mut peer, reply) = attach(&socket, &token).await;
    let id = reply["controller_id"].as_str().unwrap().to_owned();
    let read = tokio::spawn({
        let controllers = Arc::clone(&service.controllers);
        let id = id.clone();
        async move {
            controllers
                .dispatch(&id, br#"{"type":"read_session"}"#.to_vec())
                .await
        }
    });
    let mut frame = String::new();
    BufReader::new(&mut peer)
        .read_line(&mut frame)
        .await
        .unwrap();
    assert_eq!(frame, "{\"type\":\"read_session\"}\n");
    peer.write_all(b"read response\n").await.unwrap();
    assert_eq!(read.await.unwrap().unwrap(), b"read response");
    for kind in ["send_prompt", "action_commit"] {
        let frame = serde_json::to_vec(&serde_json::json!({"type":kind})).unwrap();
        assert_eq!(
            service.controllers.dispatch(&id, frame).await,
            Err(super::super::authority::Refusal::Unavailable)
        );
    }
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn receiver_barrier_outlives_a_controller_and_the_server_until_last_registry_owner() {
    let root = tempfile::tempdir().unwrap();
    // The fixture supplies a bound barrier; it makes no host or origin claim.
    let (receiver_id, generation, receiver) = super::super::barrier::receiver_fixture(root.path());
    let binding = super::super::barrier::BarrierBinding {
        anchor: crate::phone_lock::FileIdentity::at(
            &root.path().join("phone-admission-barrier.json"),
        )
        .unwrap(),
        lock: crate::phone_lock::FileIdentity::at(
            &root.path().join("phone-admission-barrier.lock"),
        )
        .unwrap(),
        lock_nonce: "ab".repeat(16),
        registrar: crate::phone_lock::FileIdentity::at(
            &root.path().join("phone-admission-registrar.lock"),
        )
        .unwrap(),
        registrar_nonce: "cd".repeat(16),
        cutoff: crate::phone_lock::FileIdentity::at(
            &root.path().join("phone-admission-cutoff.json"),
        )
        .unwrap(),
        cutoff_nonce: "ef".repeat(16),
    };
    let next =
        super::super::barrier::Barrier::existing(root.path(), &receiver_id, binding).unwrap();
    let registry = Arc::new(ControllerRegistry::new(Some((
        generation.clone(),
        receiver,
    ))));
    let path = root.path().join("controller.sock");
    let capability = root.path().join("operator-capability");
    let server = ControllerSocket::bind(&path, &capability, Arc::clone(&registry))
        .await
        .unwrap();
    let token = std::fs::read_to_string(&capability).unwrap();
    let task = tokio::spawn(server.serve());
    let (mut peer, reply) = attach(&path, &token).await;
    let id = reply["controller_id"].as_str().unwrap();
    assert_eq!(
        next.clone().receiver(&generation).err().unwrap().kind(),
        std::io::ErrorKind::WouldBlock
    );
    task.abort();
    assert!(task.await.unwrap_err().is_cancelled());
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), peer.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    assert!(!registry.contains(id));
    assert_eq!(
        next.clone().receiver(&generation).err().unwrap().kind(),
        std::io::ErrorKind::WouldBlock
    );
    drop(registry);
    let replacement_receiver = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            match next.clone().receiver(&generation) {
                Ok(receiver) => break Ok(receiver),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    tokio::task::yield_now().await;
                }
                Err(error) => break Err(error),
            }
        }
    })
    .await
    .unwrap()
    .unwrap();
    drop(replacement_receiver);
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
async fn read_registration_and_cutoff_share_the_real_barrier() {
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
        !registry
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

#[tokio::test]
async fn period_replacement_cuts_old_gate_connections_but_keeps_read_attachments() {
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
    let (_read, read_response) = attach(&path, &token).await;
    let (mut gate, gate_response) = attach(&path, &token).await;
    let read_id = read_response["controller_id"].as_str().unwrap();
    let gate_id = gate_response["controller_id"].as_str().unwrap();
    for kind in ["send_prompt", "action_commit"] {
        let frame = serde_json::to_vec(&serde_json::json!({"type":kind})).unwrap();
        assert_eq!(
            registry.dispatch(read_id, frame).await,
            Err(super::super::authority::Refusal::Unavailable)
        );
    }
    // Model an existing gate-bearing registration; ordinary attach never grants it.
    registry
        .attached
        .lock()
        .unwrap()
        .get_mut(gate_id)
        .unwrap()
        .gate_capable = true;
    registry
        .replace_period(|| {
            assert!(!registry.contains(gate_id));
            assert!(registry.contains(read_id));
            Ok(())
        })
        .unwrap();
    assert_eq!(
        gate.write_all(b"gate\n").await.unwrap_err().kind(),
        std::io::ErrorKind::BrokenPipe
    );
    assert!(registry.contains(read_id));
    task.abort();
    let _ = task.await;
}

#[test]
fn period_replacement_refuses_if_an_old_gate_cannot_be_cut() {
    let root = tempfile::tempdir().unwrap();
    let (_, generation, owner) = super::super::barrier::receiver_fixture(root.path());
    let registry = ControllerRegistry::new(Some((generation, owner)));
    let socket = UnixStream::from(std::os::fd::OwnedFd::from(tempfile::tempfile().unwrap()));
    let (sender, _responses) = tokio::sync::mpsc::channel(1);
    registry.attached.lock().unwrap().insert(
        "01".repeat(16),
        ControllerEntry {
            sender: Some(sender),
            socket: Some(socket),
            gate_capable: true,
        },
    );
    let mut called = false;
    assert_eq!(
        registry.replace_period(|| {
            called = true;
            Ok(())
        }),
        Err(super::super::authority::Refusal::Unavailable)
    );
    assert!(!called);
}

#[test]
fn period_replacement_refuses_a_lost_barrier_before_changing_authority() {
    let root = tempfile::tempdir().unwrap();
    let (_, generation, owner) = super::super::barrier::receiver_fixture(root.path());
    let registry = ControllerRegistry::new(Some((generation, owner)));
    std::fs::rename(
        root.path().join("phone-admission-barrier.json"),
        root.path().join("old-anchor"),
    )
    .unwrap();
    let mut called = false;
    assert_eq!(
        registry.replace_period(|| {
            called = true;
            Ok(())
        }),
        Err(super::super::authority::Refusal::Unavailable)
    );
    assert!(!called);
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
