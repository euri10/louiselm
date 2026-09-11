//! Real I/O acceptance for the production running-Agent adapter.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert observed outcomes."
)]

use std::{
    collections::BTreeMap,
    io::{BufReader, Read, Write},
    net::Shutdown,
    os::{fd::OwnedFd, unix::net::UnixStream},
};

use crate::{
    registry::NetworkPolicy,
    sandbox::{Backend, IdentityPlan, default_system_roots},
};

use super::*;

fn running_agent() -> Option<(tempfile::TempDir, SystemRunningAgent)> {
    if std::env::var_os("LOUISELM_REQUIRE_SYSTEM_RELAY").is_none() {
        eprintln!("skipping: production relay acceptance requires the disposable VM");
        return None;
    }
    let fixture = tempfile::tempdir().unwrap();
    // Parallel probes must not share the backend's Session-named cgroup.
    let session_id = format!(
        "system-relay-{}",
        fixture
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .trim_start_matches('.')
    );
    let runtime = fixture.path().join("runtime");
    fs::create_dir(&runtime).unwrap();
    let executable = runtime.join("agent");
    fs::write(&executable, "#!/bin/sh\nexec /bin/cat\n").unwrap();
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o755)).unwrap();
    let session = BubblewrapBackend::at(Path::new("/usr/bin/bwrap"))
        .with_bootstrap(
            &std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("louiselm-launch"),
        )
        .spawn(&ConfinementPlan {
            session_id,
            runtime_root: runtime,
            executable,
            arguments: Vec::new(),
            environment: BTreeMap::new(),
            home: fixture.path().join("home"),
            workspace: fixture.path().join("workspace"),
            system_roots: default_system_roots(),
            network: NetworkPolicy::Denied,
            identity: IdentityPlan::NamespaceOnly,
            channels: vec![Channel::AcpStdio {
                id: "acp".to_owned(),
            }],
        })
        .expect("the guest runs the real namespace-only relay fixture");
    Some((fixture, SystemRunningAgent::new(session)))
}

#[test]
fn quiescence_closes_controller_io_without_waiting_for_controller_eof() {
    close_without_controller_eof(true);
}

#[test]
fn disposal_closes_controller_io_without_prior_quiescence() {
    close_without_controller_eof(false);
}

#[test]
fn ordinary_running_adapter_never_infers_a_recovery_layout() {
    let Some((_fixture, mut agent)) = running_agent() else {
        return;
    };
    let (sender, receiver) = mpsc::channel();
    agent
        .retain_recovery(
            super::super::recovery::RetentionRequest {
                request_id: "retain".into(),
                acp_session_id: "conversation".into(),
                expires_at_ms: u64::MAX,
            },
            Box::new(move |result| {
                sender.send(result).unwrap();
            }),
        )
        .unwrap();
    let result = receiver.recv_timeout(Duration::from_secs(2)).unwrap();
    agent.dispose().unwrap();
    assert!(matches!(
        result,
        Err(super::super::recovery::RecoveryError::Unsupported)
    ));
}

fn close_without_controller_eof(quiesce_first: bool) {
    let Some((_fixture, mut agent)) = running_agent() else {
        return;
    };
    let (mut controller, input) = UnixStream::pair().unwrap();
    let (mut received, output) = UnixStream::pair().unwrap();
    controller
        .set_read_timeout(Some(Duration::from_secs(1)))
        .unwrap();
    received
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let (attachment, controller_io) = mpsc::sync_channel(1);
    attachment
        .send(
            RelayStdio::new(
                BufReader::new(fs::File::from(OwnedFd::from(input))),
                fs::File::from(OwnedFd::from(output)),
            )
            .unwrap(),
        )
        .unwrap();
    agent
        .start_relay(controller_io, Arc::new(|_| true))
        .unwrap();
    controller.write_all(b"relay-ready\n").unwrap();
    let mut echo = [0; 12];
    received.read_exact(&mut echo).unwrap();
    assert_eq!(&echo, b"relay-ready\n");

    let quiesced = if quiesce_first {
        let (sender, complete) = mpsc::sync_channel(1);
        agent
            .quiesce_relay(Box::new(move |result| {
                sender.send(result).unwrap();
            }))
            .unwrap();
        complete.recv_timeout(Duration::from_secs(2)).unwrap()
    } else {
        agent.dispose()
    };
    let peer_closed = controller.read(&mut [0]);
    let output_closed = received.read(&mut [0]);
    // Always settle the deliberately blocked old implementation before asserting.
    let _ = controller.shutdown(Shutdown::Write);
    agent.dispose().unwrap();
    assert_eq!(quiesced, Ok(()));
    assert!(
        matches!(peer_closed, Ok(0)),
        "quiescence left controller input owned: {peer_closed:?}"
    );
    assert!(
        matches!(output_closed, Ok(0)),
        "quiescence left controller output owned: {output_closed:?}"
    );
}
