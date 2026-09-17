//! Real frozen-tree, restricted-UID and reconstruction checks in the disposable VM.

use super::*;
use crate::launch_supervisor::recovery::{RecoveryError, RetentionEvidence, RetentionRequest};
use std::{
    io::{BufRead, BufReader, Write},
    os::{fd::OwnedFd, unix::net::UnixStream},
};

fn connect(running: &mut dyn RunningAgent) -> (UnixStream, BufReader<UnixStream>) {
    let (input, agent_input) = UnixStream::pair().unwrap();
    let (output, agent_output) = UnixStream::pair().unwrap();
    output
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    let (sender, receiver) = mpsc::sync_channel(1);
    sender
        .send(
            RelayStdio::new(
                BufReader::new(fs::File::from(OwnedFd::from(agent_input))),
                fs::File::from(OwnedFd::from(agent_output)),
            )
            .unwrap(),
        )
        .unwrap();
    running.start_relay(receiver, Arc::new(|_| true)).unwrap();
    (input, BufReader::new(output))
}

fn retain(
    running: &mut dyn RunningAgent,
    request: RetentionRequest,
) -> Result<RetentionEvidence, RecoveryError> {
    let (sender, receiver) = mpsc::channel();
    running
        .retain_recovery(
            request,
            Box::new(move |result| {
                sender.send(result).unwrap();
            }),
        )
        .unwrap();
    receiver.recv_timeout(Duration::from_secs(5)).unwrap()
}

pub(super) fn checkpoint(
    running: &mut dyn RunningAgent,
    launch: &LaunchRequest,
) -> (RetentionEvidence, UnixStream, BufReader<UnixStream>) {
    let (mut input, mut output) = connect(running);
    input.write_all(b"saved!\n\x1dsave conversation\n").unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "saved!\n");
    line.clear();
    output.read_line(&mut line).unwrap();
    assert_eq!(line, "\x1d7\n");
    let now = super::super::recovery_now_ms().unwrap();
    let request = RetentionRequest {
        request_id: "retain".into(),
        acp_session_id: "conversation".into(),
        expires_at_ms: now + 60_000,
    };
    assert!(matches!(
        retain(running, request.clone()),
        Err(RecoveryError::NotParked)
    ));
    running.park().unwrap();
    assert!(matches!(
        retain(
            running,
            RetentionRequest {
                expires_at_ms: 0,
                ..request.clone()
            }
        ),
        Err(RecoveryError::Expired)
    ));
    let evidence = retain(running, request.clone()).unwrap();
    assert_eq!(evidence.launch, *launch);
    assert_eq!(retain(running, request.clone()).unwrap(), evidence);
    assert!(matches!(
        retain(
            running,
            RetentionRequest {
                expires_at_ms: now + 120_000,
                ..request
            }
        ),
        Err(RecoveryError::Conflict)
    ));
    running.resume().unwrap();
    (evidence, input, output)
}

pub(super) fn reconstruct(
    platform: &SystemLaunchPlatform,
    registry: &Registry,
    sessions: &Path,
    evidence: &RetentionEvidence,
) {
    use std::os::unix::fs::MetadataExt;
    let original = sessions.join(&evidence.launch.session_id);
    assert_eq!(fs::metadata(&original).unwrap().mode() & 0o777, 0o700);
    for path in [
        original.join("home/recovery.json"),
        original.join("retained-recovery/home/recovery.json"),
        original.join("workspace/recovery-counter.json"),
        original.join("inputs/snapshot/snapshot.json"),
        original.join("cache-home/cache-session/tool-cache"),
    ] {
        assert!(
            path.is_file(),
            "retained evidence must exist before the recycled-identity probe"
        );
        assert!(
            !std::process::Command::new("/usr/bin/setpriv")
                .args([
                    "--reuid",
                    "4008000",
                    "--regid",
                    "4008000",
                    "--clear-groups",
                    "/usr/bin/test",
                    "-r"
                ])
                .arg(path)
                .status()
                .unwrap()
                .success(),
            "recycled UID can read old recovery material"
        );
    }
    let mut replacement = evidence.launch.clone();
    replacement.session_id = "replacement".into();
    replacement.request_id = "replacement-launch".into();
    replacement.authorization_id = "replacement-authorization".into();
    let plan = crate::launch::resolve(
        &replacement,
        registry,
        sessions,
        IdentityPlan::HostIdentity {
            uid: 4_008_000,
            gid: 4_008_000,
        },
    )
    .unwrap()
    .plan;
    let prepared = platform.prepare(&replacement, plan).unwrap();
    // This is a trusted fixture restoration into a different prepared tree.
    // Broker operator authorization/restore orchestration remains .6/.2.
    for name in ["home/recovery.json", "workspace/recovery-counter.json"] {
        let destination = sessions.join("replacement").join(name);
        fs::copy(original.join("retained-recovery").join(name), &destination).unwrap();
        std::os::unix::fs::chown(&destination, Some(4_008_000), Some(4_008_000)).unwrap();
    }
    let mut running = prepared.start().unwrap();
    let (mut input, mut output) = connect(&mut *running);
    input.write_all(b"\x1dload conversation\n").unwrap();
    let mut line = String::new();
    output.read_line(&mut line).unwrap();
    running.dispose().unwrap();
    assert_eq!(
        line, "\x1d7\n",
        "fresh measured Agent did not load the retained point"
    );
}

pub(super) fn dispose_with_failed_seal(running: &mut dyn RunningAgent) {
    crate::launch_supervisor::recovery::FAIL_SEAL_SYNC.set(true);
    assert_eq!(running.dispose(), Err(SupervisorError::CleanupUnproven));
    // The lifecycle owner cannot release its identity on that result. Retry may
    // finish sealing, but cannot retroactively turn the failed result into ACK.
    running.dispose().unwrap();
}
