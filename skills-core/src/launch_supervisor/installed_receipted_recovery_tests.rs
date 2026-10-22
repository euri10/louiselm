//! Software-signed historical fixtures share the real reboot/cleanup transaction.
//! Receipt measurements are synthetic; signatures, broker admission and storage are real.
use super::*;
use crate::{
    broker::ReceiptStore,
    launch_receipt::{
        ReceiptAuthority, ReceiptCause, ReceiptOutcome, SessionState, SignedReceipt, StartEvidence,
    },
    launcher_install::{LauncherSigner, LauncherVerifier},
};

pub(super) fn admit(root: &Path, broker: &InstalledBroker, subject: &str) {
    let source = root.join(format!("{subject}-chain.json"));
    if !source.exists() {
        return;
    }
    let verifier =
        Arc::new(LauncherVerifier::open(&LauncherPaths::system(), &root.join("state")).unwrap());
    let receipts =
        ReceiptStore::installed(&root.join("state/receipts"), Arc::clone(&verifier)).unwrap();
    if receipts.head(subject).unwrap().is_some() {
        return;
    }
    let authorization = broker
        .consumed_recovery_snapshot(&format!("approval-{subject}"))
        .unwrap()
        .binding
        .authorization;
    let chain: Vec<SignedReceipt> = serde_json::from_slice(&fs::read(source).unwrap()).unwrap();
    for receipt in chain {
        receipts
            .append(
                &authorization,
                &receipt.canonical_bytes(),
                None,
                |key, payload, signature| {
                    verifier.verify(key, payload, signature).unwrap();
                    true
                },
            )
            .unwrap();
    }
}

pub(super) fn seed(root: &Path, paths: &LauncherPaths, endpoint: &Path, pid: u32) {
    let Response::Snapshot { snapshot } = crate::broker::operator::consumed_recovery::exchange(
        endpoint,
        BROKER_UID,
        &Request::Prepare {
            authorization_id: "approval-receipted".into(),
        },
    )
    .unwrap() else {
        panic!("snapshot")
    };
    let authorization = &snapshot.binding.authorization;
    let signer = LauncherSigner::open(paths).unwrap();
    let sign = |payload: crate::launch_receipt::ReceiptPayload| {
        let signature = signer
            .sign_receipt(&payload.signing_key_id, &payload.canonical_bytes())
            .unwrap();
        SignedReceipt {
            schema: crate::launch_receipt::SIGNED_RECEIPT_SCHEMA.into(),
            payload,
            signature,
        }
    };
    let launch = sign(super::super::receipt_history::genesis(
        &signer,
        authorization,
    ));
    let start = sign(crate::launch_receipt::ReceiptPayload {
        sequence: 1,
        previous_receipt_digest: Some(launch.digest().to_string()),
        request_id: "start-receipted".into(),
        outcome: ReceiptOutcome::Start {
            evidence: StartEvidence {
                agent_pid: pid,
                assigned_uid: authorization.assigned_uid,
                assigned_gid: authorization.assigned_gid,
                tool_isolation_digest: Digest::of(b"synthetic installed recovery tools")
                    .to_string(),
                sender_guard_required: false,
            },
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::LaunchAcknowledged,
            },
        },
        resulting_state: SessionState::Running,
        ..launch.payload.clone()
    });
    let park = sign(crate::launch_receipt::ReceiptPayload {
        sequence: 2,
        previous_receipt_digest: Some(start.digest().to_string()),
        request_id: "park-receipted".into(),
        outcome: ReceiptOutcome::Park {
            authority: ReceiptAuthority::Cause {
                cause: ReceiptCause::BrokerLost,
            },
        },
        resulting_state: SessionState::Parked,
        ..start.payload.clone()
    });
    write_json(
        &root.join("receipted-chain.json"),
        &vec![launch, start, park],
    );
    let retained = sessions().join("receipted");
    fs::create_dir(&retained).unwrap();
    fs::set_permissions(&retained, fs::Permissions::from_mode(0o711)).unwrap();
    let bytes = retained.join("private-fixture");
    fs::write(&bytes, b"receipted private fixture").unwrap();
    chown(&bytes, Some(AGENT_UID + 2), Some(AGENT_UID + 2)).unwrap();
    fs::set_permissions(bytes, fs::Permissions::from_mode(0o600)).unwrap();
}

pub(super) fn lose_ack(root: &Path, paths: &LauncherPaths, operator: u32, endpoint: &Path) {
    fs::write(root.join("withhold-ack"), b"receipted lost ACK").unwrap();
    assert!(matches!(
        complete_at(paths, operator, "approval-receipted", endpoint),
        Err(RecoveryError::Broker(_))
    ));
    fs::remove_file(root.join("withhold-ack")).unwrap();
}

pub(super) fn complete(
    root: &Path,
    paths: &LauncherPaths,
    operator: u32,
    endpoint: &Path,
) -> crate::launch_recovery::Proof {
    let saved = fs::read(
        paths
            .state_root
            .join("consumed-recovery")
            .join(Digest::of(b"approval-receipted").hex())
            .join("outcome.json"),
    )
    .unwrap();
    fs::write(
        root.join("state/authorize-receipted-successor"),
        b"test exact identity reuse",
    )
    .unwrap();
    let successor = crate::launcher_install::acquire_identity(paths, 2).unwrap();
    let proof = complete_at(paths, operator, "approval-receipted", endpoint).unwrap();
    assert_eq!(proof.canonical_bytes(), saved);
    assert_eq!(
        complete_at(paths, operator, "approval-receipted", endpoint).unwrap(),
        proof
    );
    assert!(matches!(
        crate::launcher_install::acquire_identity(paths, 2),
        Err(crate::launcher_install::LauncherError::Occupied { slot: 2 })
    ));
    let verifier = LauncherVerifier::open(paths, &paths.state_root).unwrap();
    verifier.verify_recovery(&proof).unwrap();
    let original: Vec<SignedReceipt> =
        serde_json::from_slice(&fs::read(root.join("receipted-chain.json")).unwrap()).unwrap();
    for receipt in &original {
        let retained = root
            .join("state/receipts/sessions/receipted")
            .join(format!("{:020}.receipt.json", receipt.payload.sequence));
        assert_eq!(fs::read(retained).unwrap(), receipt.canonical_bytes());
    }
    assert_eq!(
        proof.payload.binding.receipt_head,
        Some(crate::launch_receipt::ReceiptHead {
            sequence: 2,
            digest: original.last().unwrap().digest().to_string(),
        })
    );
    assert_eq!(
        fs::read(sessions().join("receipted/private-fixture")).unwrap(),
        b"receipted private fixture"
    );
    assert_eq!(
        fs::metadata(sessions().join("receipted")).unwrap().mode() & 0o7777,
        0o700
    );
    successor.release().unwrap();
    println!("RECOVERY_RECEIPTED_HEAD_PRESERVED");
    proof
}
