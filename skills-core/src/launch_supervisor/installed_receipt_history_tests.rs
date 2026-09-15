//! Installed historical verification and new-chain admission use root authority.

use super::*;

#[path = "installed_key_cleanup_tests.rs"]
mod cleanup;
#[path = "installed_history_isolation_tests.rs"]
mod isolation;
#[path = "installed_key_revocation_tests.rs"]
mod revocation;
use crate::{
    broker::{AuthorizationStore, ReceiptStore},
    launch_protocol::LaunchAuthorization,
    launch_receipt::{
        Authorization, ReceiptOutcome, ReceiptPayload, SIGNED_RECEIPT_SCHEMA, SessionState,
        SignedReceipt,
    },
    launcher_install::{LauncherSigner, LauncherVerifier, RotationRequest, rotate},
};

fn authorization(root: &Path, config: &LauncherConfig, session: &str) -> LaunchAuthorization {
    let request = LaunchRequest {
        session_id: session.into(),
        request_id: format!("request-{session}"),
        authorization_id: format!("authorization-{session}"),
        ..request()
    };
    let store = AuthorizationStore::open(&root.join("approvals"), config.pool.clone()).unwrap();
    store
        .authorize(
            &GrantRequest {
                request: request.clone(),
                controller_uid: config.operator_uid,
                expires_at_ms: 30000,
                broker_loss_grace_ms: 5000,
                commands: None,
                require_cold_recovery: false,
            },
            1000,
        )
        .unwrap();
    store.consume_for_launcher(&request, 2000).unwrap()
}

fn genesis(signer: &LauncherSigner, auth: &LaunchAuthorization) -> ReceiptPayload {
    ReceiptPayload {
        schema: crate::launch_receipt::RECEIPT_SCHEMA.into(),
        session_id: auth.session_id.clone(),
        run_id: auth.run_id.clone(),
        request_id: auth.request_id.clone(),
        envelope_revision: auth.envelope_revision,
        sequence: 0,
        previous_receipt_digest: None,
        release_id: signer.release_id().into(),
        signing_key_id: signer.active_key_id().into(),
        resulting_state: SessionState::Starting,
        outcome: ReceiptOutcome::Launch {
            authorization: Authorization {
                authorization_id: auth.authorization_id.clone(),
                request_id: auth.request_id.clone(),
                request_digest: auth.request_digest.clone(),
            },
            evidence: Box::new(crate::launch_receipt::LaunchEvidence {
                conformance: crate::launch_receipt::ConformanceEvidence::Unevaluated,
                launch_request_digest: auth.request_digest.clone(),
                runtime_measurement_digest: Digest::of(b"runtime").to_string(),
                skill_generation_id: Digest::of(b"generation").to_string(),
                session_input_manifest_id: Digest::of(b"input").to_string(),
                isolation_contract: "louiselm.isolation/1".into(),
                isolation_backend_id: "bubblewrap-0_12".into(),
                kernel_identity: "linux-6_12".into(),
                isolation_evidence_digest: Digest::of(b"isolation").to_string(),
                broker_loss_grace_ms: auth.broker_loss_grace_ms,
                capability_channel_ids: vec!["acp".into(), "broker".into()],
            }),
        },
    }
}

fn sign(signer: &LauncherSigner, payload: ReceiptPayload) -> SignedReceipt {
    let signature = signer
        .sign_receipt(&payload.signing_key_id, &payload.canonical_bytes())
        .unwrap();
    SignedReceipt {
        schema: SIGNED_RECEIPT_SCHEMA.into(),
        payload,
        signature,
    }
}

fn append(
    store: &ReceiptStore,
    verifier: &LauncherVerifier,
    auth: &LaunchAuthorization,
    receipt: &SignedReceipt,
) {
    store
        .append(
            auth,
            &receipt.canonical_bytes(),
            None,
            |key, payload, signature| {
                verifier.verify(key, payload, signature).unwrap();
                true
            },
        )
        .unwrap();
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One installed history transaction checks rotation, stale admission, exact enrollment, upgrade and restart together."
)]
fn privileged_installed_receipt_history_survives_rotation_and_upgrade() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_LAUNCH").is_none() {
        eprintln!("skipping: installed receipt history requires the disposable launcher VM");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let _account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-receipt-history-")
        .tempdir_in("/var/lib")
        .unwrap();
    let (paths, config, _) = install_fixture_with_slots(root.path(), 4);
    let signer = LauncherSigner::open(&paths).unwrap();
    let verifier = Arc::new(LauncherVerifier::open(&paths, &root.path().join("state")).unwrap());
    let receipt_root = root.path().join("receipts");
    let store = ReceiptStore::installed(&receipt_root, Arc::clone(&verifier)).unwrap();
    let auth = authorization(root.path(), &config, "original");
    let launch = sign(&signer, genesis(&signer, &auth));
    assert_eq!(
        fs::metadata(paths.state_root.join("receipt-bindings"))
            .unwrap()
            .mode()
            & 0o777,
        0o711,
        "the dedicated broker must traverse history regardless of the signer's umask"
    );
    append(&store, &verifier, &auth, &launch);

    let rotation = RotationRequest {
        rotation_id: "routine".into(),
        expected_active_key_id: signer.active_key_id().into(),
    };
    rotate(&paths, &SystemCommandRunner, &rotation, 4000).unwrap();
    // The verifier opened before rotation must see newly registered keys.
    let current = LauncherSigner::open(&paths).unwrap();
    let fresh = authorization(root.path(), &config, "fresh");
    assert!(
        signer
            .sign_receipt(
                signer.active_key_id(),
                &genesis(&signer, &fresh).canonical_bytes()
            )
            .is_err()
    );
    let new_launch = sign(&current, genesis(&current, &fresh));
    append(&store, &verifier, &fresh, &new_launch);

    let start = sign(
        &signer,
        ReceiptPayload {
            sequence: 1,
            previous_receipt_digest: Some(launch.digest().to_string()),
            request_id: "start-original".into(),
            outcome: ReceiptOutcome::Start {
                evidence: crate::launch_receipt::StartEvidence {
                    agent_pid: 123,
                    assigned_uid: auth.assigned_uid,
                    assigned_gid: auth.assigned_gid,
                    tool_isolation_digest: Digest::of(b"tool isolation").to_string(),
                },
                authority: crate::launch_receipt::ReceiptAuthority::Cause {
                    cause: crate::launch_receipt::ReceiptCause::LaunchAcknowledged,
                },
            },
            resulting_state: SessionState::Running,
            ..launch.payload.clone()
        },
    );
    append(&store, &verifier, &auth, &start);
    assert_eq!(
        sign(&signer, launch.payload.clone()).canonical_bytes(),
        launch.canonical_bytes(),
        "exact genesis replay survives retirement"
    );
    let mut changed = launch.payload.clone();
    changed.request_id = "backdated-replacement".into();
    if let ReceiptOutcome::Launch { authorization, .. } = &mut changed.outcome {
        authorization.request_id.clone_from(&changed.request_id);
    }
    assert!(
        signer
            .sign_receipt(signer.active_key_id(), &changed.canonical_bytes())
            .is_err(),
        "existing Session identity cannot authorize a different genesis"
    );
    let forged = raw_signature(&paths, &changed, root.path());
    let keyring = crate::launcher_install::public_keyring(&paths).unwrap();
    crate::sshsig::verify(
        &forged,
        &changed.schema,
        &changed.canonical_bytes(),
        &keyring.key(&changed.signing_key_id).unwrap().public_key,
        crate::sshsig::SkPolicy::none(),
    )
    .unwrap();
    assert!(
        verifier
            .verify(&changed.signing_key_id, &changed.canonical_bytes(), &forged)
            .is_err(),
        "a valid old-key signature cannot replace exact registered history"
    );

    upgrade(&paths, &config);
    let reopened = Arc::new(LauncherVerifier::open(&paths, &root.path().join("state")).unwrap());
    assert_ne!(reopened.config().release_id, launch.payload.release_id);
    let reopened_store = ReceiptStore::installed(&receipt_root, Arc::clone(&reopened)).unwrap();
    for (authorization, receipts) in [(&auth, vec![&launch, &start]), (&fresh, vec![&new_launch])] {
        for receipt in &receipts {
            reopened
                .verify(
                    &receipt.payload.signing_key_id,
                    &receipt.payload.canonical_bytes(),
                    &receipt.signature,
                )
                .unwrap();
        }
        assert_eq!(
            reopened_store
                .stored_bytes(&authorization.session_id)
                .unwrap(),
            receipts
                .iter()
                .map(|r| r.canonical_bytes())
                .collect::<Vec<_>>()
        );
    }
    let interrupt = sign(
        &signer,
        ReceiptPayload {
            sequence: 2,
            previous_receipt_digest: Some(start.digest().to_string()),
            request_id: "interrupt-original".into(),
            outcome: ReceiptOutcome::Interrupt {
                authorization: Authorization {
                    authorization_id: "interrupt-authority".into(),
                    request_id: "interrupt-original".into(),
                    request_digest: Digest::of(b"interrupt").to_string(),
                },
            },
            ..start.payload.clone()
        },
    );
    append(&reopened_store, &reopened, &auth, &interrupt);
    let next = authorization(root.path(), &config, "after-upgrade");
    assert!(
        current
            .sign_receipt(
                current.active_key_id(),
                &genesis(&current, &next).canonical_bytes()
            )
            .is_err(),
        "stale release cannot admit a new chain"
    );
    let upgraded = LauncherSigner::open(&paths).unwrap();
    append(
        &reopened_store,
        &reopened,
        &next,
        &sign(&upgraded, genesis(&upgraded, &next)),
    );
    let failed = authorization(root.path(), &config, "failed-enrollment");
    let failed_binding = paths.state_root.join("receipt-bindings").join(format!(
        "{}.json",
        Digest::of(failed.session_id.as_bytes()).hex()
    ));
    fs::create_dir(&failed_binding).unwrap();
    assert!(
        upgraded
            .sign_receipt(
                upgraded.active_key_id(),
                &genesis(&upgraded, &failed).canonical_bytes()
            )
            .is_err()
    );
    assert!(
        reopened_store
            .stored_bytes(&failed.session_id)
            .unwrap()
            .is_empty()
    );
    fs::remove_dir(&failed_binding).unwrap();
    rustix::fs::mkfifoat(
        rustix::fs::CWD,
        &failed_binding,
        rustix::fs::Mode::RUSR | rustix::fs::Mode::RGRP | rustix::fs::Mode::ROTH,
    )
    .unwrap();
    assert!(
        upgraded
            .sign_receipt(
                upgraded.active_key_id(),
                &genesis(&upgraded, &failed).canonical_bytes()
            )
            .is_err(),
        "special history files refuse before a blocking open"
    );
    fs::remove_file(&failed_binding).unwrap();
    append(
        &reopened_store,
        &reopened,
        &failed,
        &sign(&upgraded, genesis(&upgraded, &failed)),
    );

    // A damaged protected binding must never silently enroll replacement history.
    let binding = paths.state_root.join("receipt-bindings").join(format!(
        "{}.json",
        Digest::of(auth.session_id.as_bytes()).hex()
    ));
    fs::set_permissions(&binding, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(
        reopened
            .verify(
                &launch.payload.signing_key_id,
                &launch.payload.canonical_bytes(),
                &launch.signature
            )
            .is_err()
    );
    assert!(
        signer
            .sign_receipt(signer.active_key_id(), &launch.payload.canonical_bytes())
            .is_err()
    );
}

fn raw_signature(paths: &LauncherPaths, receipt: &ReceiptPayload, scratch: &Path) -> String {
    let message = scratch.join("forged-payload");
    fs::write(&message, receipt.canonical_bytes()).unwrap();
    let key = paths
        .state_root
        .join("private/keys")
        .join(
            Digest::parse(&receipt.signing_key_id)
                .unwrap()
                .directory_name(),
        )
        .join("key");
    assert!(
        Command::new(&paths.ssh_keygen)
            .args(["-Y", "sign", "-q", "-n", &receipt.schema, "-f"])
            .arg(key)
            .arg(&message)
            .status()
            .unwrap()
            .success()
    );
    fs::read_to_string(message.with_extension("sig")).unwrap()
}

pub(super) fn upgrade(paths: &LauncherPaths, config: &LauncherConfig) {
    let old = paths
        .release_prefix
        .join("releases")
        .join(&config.release_id);
    let mut manifest: crate::release::ReleaseManifest =
        serde_json::from_slice(&fs::read(old.join("manifest.json")).unwrap()).unwrap();
    manifest.version = "0.2.0".into();
    manifest.release_id = manifest.digest().to_string();
    let next = paths
        .release_prefix
        .join("releases")
        .join(&manifest.release_id);
    fs::create_dir_all(next.join("bin")).unwrap();
    for component in &manifest.components {
        fs::copy(old.join(&component.path), next.join(&component.path)).unwrap();
    }
    write_json(&next.join("manifest.json"), &manifest);
    fs::remove_file(paths.release_prefix.join("current")).unwrap();
    symlink(
        Path::new("releases").join(&manifest.release_id),
        paths.release_prefix.join("current"),
    )
    .unwrap();
    write_json(
        &paths.release_prefix.join("state.json"),
        &InstalledState {
            schema: STATE_SCHEMA.into(),
            release_id: manifest.release_id,
            built_at_ms: 1,
            installed_at_ms: 5000,
            source_commit: "fixture".into(),
            policy_version: "fixture".into(),
        },
    );
    crate::launcher_install::install(
        paths,
        &SystemCommandRunner,
        &InstallRequest {
            operator: config.operator.clone(),
            broker_uid: config.broker_uid,
            broker_gid: config.broker_gid,
            pool: config.pool.clone(),
        },
        5000,
    )
    .unwrap();
}
