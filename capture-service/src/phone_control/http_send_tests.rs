//! Receiver authorization of explicitly signed phone prompts.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Fixtures abort on setup failure and assert exact outcomes."
)]

use std::{sync::Arc, time::Instant};

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::tests::{
    approve_scoped_grant, history_grant, history_period, post, signed_history, signed_history_page,
    signed_unlock, unhex,
};
use crate::{
    PairingRegistry,
    hex::lower_hex,
    phone_control::{
        Request, Service,
        authority::{Refusal, StoredGrant},
        fixed, prefix,
    },
};

#[test]
fn owner_outcome_is_exact_and_does_not_invent_admission() {
    use crate::phone_control::ActionOutcome;

    let operation = "11".repeat(16);
    let evidence = "22".repeat(32);
    let accepted = json!({"v":1,"type":"action_result","operation_id":operation,
        "state":"accepted","code":"","evidence":evidence});
    assert_eq!(
        super::send::owner_outcome(accepted.to_string().as_bytes(), &operation),
        Ok(ActionOutcome::Accepted {
            evidence: evidence.clone()
        })
    );
    let mut fence = accepted.clone();
    fence["state"] = json!("fenced_no_effect");
    fence["code"] = json!("stale_subject");
    assert_eq!(
        super::send::owner_outcome(fence.to_string().as_bytes(), &operation),
        Ok(ActionOutcome::FencedNoEffect {
            evidence: evidence.clone()
        })
    );
    for bad in [
        json!({"v":1,"type":"error","code":"unavailable"}),
        json!({"v":1,"type":"action_result","operation_id":"33".repeat(16),
            "state":"accepted","code":"","evidence":evidence}),
        json!({"v":1,"type":"action_result","operation_id":operation,
            "state":"accepted","code":"busy","evidence":evidence}),
        json!({"v":1,"type":"action_result","operation_id":operation,
            "state":"completed","code":"","evidence":evidence}),
        json!({"v":1,"type":"action_result","operation_id":operation,
            "state":"fenced_no_effect","code":"unknown","evidence":evidence}),
        json!({"v":1,"type":"action_result","operation_id":operation,
            "state":"accepted","code":"","evidence":"invalid"}),
        json!({"v":1,"type":"action_result","operation_id":operation,
            "state":"accepted","code":"","evidence":evidence,"extra":1}),
    ] {
        assert_eq!(
            super::send::owner_outcome(bad.to_string().as_bytes(), &operation),
            Err(Refusal::Unavailable)
        );
    }
}

#[test]
fn owner_preparation_is_bound_to_the_exact_connection_session_and_operation() {
    let controller = "11".repeat(16);
    let subject = "22".repeat(16);
    let operation = "33".repeat(16);
    let prepared = json!({"v":1,"type":"action_prepared","controller_id":controller,
        "subject_id":subject,"operation_id":operation});
    let check = |value: &Value| {
        super::send::owner_prepared(
            value.to_string().as_bytes(),
            &controller,
            &subject,
            &operation,
        )
    };
    assert_eq!(check(&prepared), Ok(()));
    for field in ["controller_id", "subject_id", "operation_id"] {
        let mut wrong = prepared.clone();
        wrong[field] = json!("44".repeat(16));
        assert_eq!(check(&wrong), Err(Refusal::Unavailable));
    }
    let mut extra = prepared;
    extra["extra"] = json!(true);
    assert_eq!(check(&extra), Err(Refusal::Unavailable));
}

#[test]
fn archive_keeps_proven_owner_phases_and_rejects_conflicting_evidence() {
    use crate::phone_control::{
        ActionEvidence, ActionEvidenceStore, ActionIdentity, ActionOutcome,
    };

    let root = tempfile::tempdir().unwrap();
    let identity = ActionIdentity {
        operation_id: "01".repeat(16),
        receiver_identity: "02".repeat(32),
        controller_id: "03".repeat(16),
        subject_id: "04".repeat(16),
        action: 3,
        payload_hash: "05".repeat(32),
        grant_id: "06".repeat(16),
        grant_revision: 1,
        period_id: "07".repeat(16),
        expected_revision: 7,
        project: root.path().to_str().unwrap().to_owned(),
        agent: "codex".into(),
        recorded_at_ms: 1,
    };
    let store = ActionEvidenceStore::open(root.path().join("archive")).unwrap();
    store.reserve_fixture(&identity).unwrap();
    let accepted = ActionOutcome::Accepted {
        evidence: "11".repeat(32),
    };
    let dispatched = ActionOutcome::Dispatched {
        evidence: "22".repeat(32),
    };
    let owner = ActionEvidence {
        identity: identity.clone(),
        outcome: ActionOutcome::Unknown,
        phases: vec![accepted.clone(), dispatched.clone()],
        updated_at_ms: 2,
    };
    let initial = store.lookup(&identity.operation_id).unwrap();
    assert_eq!(
        store.unresolved(&identity.receiver_identity).unwrap()[0].identity,
        identity
    );
    super::send::archive_owner_phases(&initial, &owner, |outcome| {
        store
            .report_fixture(&identity, outcome, 3)
            .map_err(|_| Refusal::Unavailable)
    })
    .unwrap();
    let saved = store.lookup(&identity.operation_id).unwrap();
    assert_eq!(saved.phases, vec![accepted, dispatched]);
    assert_eq!(saved.outcome, ActionOutcome::Unknown);
    assert_eq!(
        store.unresolved(&identity.receiver_identity).unwrap()[0].identity,
        identity
    );
    let mut contradiction = owner.clone();
    contradiction.phases.clear();
    contradiction.outcome = ActionOutcome::FencedNoEffect {
        evidence: "33".repeat(32),
    };
    assert_eq!(
        super::send::archive_owner_phases(&saved, &contradiction, |_| Ok(())),
        Err(Refusal::Unavailable)
    );
    contradiction = owner;
    contradiction.identity.payload_hash = "44".repeat(32);
    assert_eq!(
        super::send::archive_owner_phases(&saved, &contradiction, |_| Ok(())),
        Err(Refusal::Unavailable)
    );
    assert_eq!(store.lookup(&identity.operation_id).unwrap(), saved);
}

#[test]
fn independence_reply_must_name_only_proved_original_operations() {
    use crate::phone_control::ActionIdentity;
    let controller = "11".repeat(16);
    let subject = "22".repeat(16);
    let operation = "33".repeat(16);
    let request: Request = serde_json::from_value(json!({
        "grant_id":"44".repeat(16),"grant_revision":1,"period_id":"55".repeat(16),
        "receiver_identity":"66".repeat(32),"controller_id":controller,"subject_id":subject,
        "action":3,"payload":"0161","payload_hash":"77".repeat(32),"operation_id":operation,
        "expected_revision":1,"interaction":1,"signature":"00"}))
    .unwrap();
    let old = ActionIdentity {
        operation_id: "88".repeat(16),
        receiver_identity: "66".repeat(32),
        controller_id: controller.clone(),
        subject_id: "99".repeat(16),
        action: 3,
        payload_hash: "aa".repeat(32),
        grant_id: "44".repeat(16),
        grant_revision: 1,
        period_id: "55".repeat(16),
        expected_revision: 1,
        project: "/tmp".into(),
        agent: "codex".into(),
        recorded_at_ms: 1,
    };
    let reply = json!({"v":1,"type":"action_independence","controller_id":controller,
        "subject_id":subject,"operation_id":operation,"independent":[old.operation_id]});
    let check = |value: &Value, identities: &[ActionIdentity]| {
        super::send::validate_independence(value.to_string().as_bytes(), &request, identities)
    };
    assert_eq!(
        check(&reply, std::slice::from_ref(&old)),
        Ok(vec![old.operation_id.clone()])
    );
    let mut previous_controller = old.clone();
    previous_controller.controller_id = "bb".repeat(16);
    assert_eq!(
        check(&reply, &[previous_controller]),
        Ok(vec![old.operation_id.clone()])
    );
    let mut same_subject = old.clone();
    same_subject.subject_id.clone_from(&request.subject_id);
    assert_eq!(check(&reply, &[same_subject]), Err(Refusal::Unavailable));
    let mut duplicate = reply.clone();
    duplicate["independent"] = json!([old.operation_id, old.operation_id]);
    assert_eq!(
        check(&duplicate, std::slice::from_ref(&old)),
        Err(Refusal::Unavailable)
    );
    let mut wrong = reply;
    wrong["subject_id"] = json!("cc".repeat(16));
    assert_eq!(check(&wrong, &[old]), Err(Refusal::Unavailable));
}

fn signed_send(fixture: &Value, grant: &StoredGrant, period: &str, operation: u128) -> Request {
    signed_send_to(
        fixture,
        grant,
        period,
        operation,
        &"11".repeat(16),
        &"22".repeat(16),
    )
}

pub(super) fn signed_send_to(
    fixture: &Value,
    grant: &StoredGrant,
    period: &str,
    operation: u128,
    controller: &str,
    subject: &str,
) -> Request {
    let payload = b"\x01A reviewed prompt";
    let hash = lower_hex(&Sha256::digest(payload));
    let operation = format!("{operation:032x}");
    let mut bytes = prefix(2);
    bytes.extend(fixed::<16>(&grant.grant_id).unwrap());
    bytes.extend(grant.grant_revision.to_be_bytes());
    bytes.extend(fixed::<16>(period).unwrap());
    bytes.extend(fixed::<32>(&grant.receiver_identity).unwrap());
    bytes.extend(fixed::<16>(controller).unwrap());
    bytes.extend(fixed::<16>(subject).unwrap());
    bytes.push(3);
    bytes.extend(fixed::<32>(&hash).unwrap());
    bytes.extend(fixed::<16>(&operation).unwrap());
    bytes.extend(7_u64.to_be_bytes());
    bytes.push(1);
    let key = EcdsaKeyPair::from_pkcs8(
        &ECDSA_P256_SHA256_ASN1_SIGNING,
        &unhex(fixture["keys"]["working"]["pkcs8"].as_str().unwrap()),
    )
    .unwrap();
    let signature = key.sign(&SystemRandom::new(), &bytes).unwrap();
    serde_json::from_value(json!({
        "grant_id":grant.grant_id,"grant_revision":grant.grant_revision,"period_id":period,
        "receiver_identity":grant.receiver_identity,"controller_id":controller,"subject_id":subject,
        "action":3,"payload":lower_hex(payload),"payload_hash":hash,"operation_id":operation,
        "expected_revision":7,"interaction":1,"signature":lower_hex(signature.as_ref())
    }))
    .unwrap()
}

#[tokio::test]
async fn send_authority_requires_signed_prompt_and_operate_group() {
    for group in ["read", "operate_sessions"] {
        let root = tempfile::tempdir().unwrap();
        let receiver = "01".repeat(32);
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let offer = pairing
            .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
            .unwrap();
        let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
        let service = Service::open(&root.path().join("phone/grants"), &receiver, pairing).unwrap();
        let app = service.router();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../android/app/src/test/resources/phone-control-v1.json"
        ))
        .unwrap();
        let (grant, period) = history_grant(
            &service,
            &app,
            &phone,
            &fixture,
            root.path(),
            vec![group.into()],
        )
        .await;
        let request = signed_send(&fixture, &grant, &period, 1);
        let mut tampered = request.clone();
        tampered.expected_revision += 1;
        assert!(matches!(
            service
                .authority
                .begin_read(&phone.device_id, &tampered, Instant::now()),
            Err(Refusal::Unauthenticated)
        ));
        let result = service
            .authority
            .begin_read(&phone.device_id, &request, Instant::now());
        if group == "read" {
            assert!(matches!(result, Err(Refusal::OutOfScope)));
        } else {
            assert!(result.is_ok());
            assert!(matches!(
                service
                    .authority
                    .begin_read(&phone.device_id, &request, Instant::now()),
                Err(Refusal::Unauthenticated)
            ));
        }
    }
}

#[tokio::test]
async fn send_scope_comes_from_owner_and_is_rechecked_with_pairing() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let receiver = "01".repeat(32);
    let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
    let offer = pairing
        .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
    let service = Service::open(
        &root.path().join("phone/grants"),
        &receiver,
        pairing.clone(),
    )
    .unwrap();
    let app = service.router();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let (grant, period) = history_grant(
        &service,
        &app,
        &phone,
        &fixture,
        root.path(),
        vec!["operate_sessions".into()],
    )
    .await;
    let request = signed_send(&fixture, &grant, &period, 7);
    let permit = service
        .authority
        .begin_read(&phone.device_id, &request, Instant::now())
        .unwrap();
    let project = std::fs::canonicalize(root.path()).unwrap();
    let (identity, remaining) = service
        .authority
        .authorize_send(&permit, &pairing, &phone.credential, &project, "codex")
        .unwrap();
    assert!((1..=5_000).contains(&remaining));
    assert_eq!(identity.operation_id, request.operation_id);
    assert_eq!(identity.project, project.to_str().unwrap());
    assert_eq!(identity.agent, "codex");
    assert_eq!(identity.payload_hash, request.payload_hash);
    assert_eq!(identity.expected_revision, 7);
    assert!(matches!(
        service.authority.authorize_send(
            &permit,
            &pairing,
            &phone.credential,
            outside.path(),
            "codex",
        ),
        Err(Refusal::OutOfScope)
    ));
    assert!(matches!(
        service.authority.authorize_send(
            &permit,
            &pairing,
            &phone.credential,
            &project,
            "other-agent",
        ),
        Err(Refusal::OutOfScope)
    ));
    pairing.revoke(&phone.device_id).unwrap();
    assert!(matches!(
        service
            .authority
            .authorize_send(&permit, &pairing, &phone.credential, &project, "codex",),
        Err(Refusal::Revoked)
    ));
}

#[tokio::test]
async fn unlock_preflight_checks_proof_before_deciding_to_cut_a_period() {
    use crate::phone_control::{Proof, authority::Scope};

    let root = tempfile::tempdir().unwrap();
    let receiver = "01".repeat(32);
    let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
    let offer = pairing
        .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
    let service = Service::open(&root.path().join("phone/grants"), &receiver, pairing).unwrap();
    let app = service.router();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let grant = approve_scoped_grant(
        &service,
        &phone,
        &fixture,
        Scope {
            projects: vec![root.path().to_str().unwrap().into()],
            agents: vec!["codex".into()],
            groups: vec!["operate_sessions".into()],
        },
    );
    let get_proof = |challenge: &Value| -> Proof {
        serde_json::from_value(signed_unlock(&fixture, challenge)["proof"].clone()).unwrap()
    };
    let challenge_request = json!({"v":1,"type":"challenge","grant_id":grant.grant_id,
        "grant_revision":grant.grant_revision})
    .to_string();
    let (_, challenge) = post(
        app.clone(),
        "/v1/phone-control/challenge",
        &phone.credential,
        &challenge_request,
    )
    .await;
    let first = get_proof(&challenge);
    assert_eq!(
        service.authority.validate_unlock(&phone.device_id, &first),
        Ok(false)
    );
    service
        .authority
        .unlock(
            &phone.device_id,
            &first,
            Instant::now(),
            crate::time::now_ms(),
        )
        .unwrap();
    let (_, challenge) = post(
        app,
        "/v1/phone-control/challenge",
        &phone.credential,
        &challenge_request,
    )
    .await;
    let next = get_proof(&challenge);
    assert_eq!(
        service.authority.validate_unlock(&phone.device_id, &next),
        Ok(true)
    );
    let mut forged = next;
    forged.challenge = "00".repeat(32);
    assert_eq!(
        service.authority.validate_unlock(&phone.device_id, &forged),
        Err(Refusal::Unauthenticated)
    );
}

#[tokio::test]
async fn fenced_and_replayed_sends_do_not_renew_the_working_period() {
    for admitted in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let receiver = "01".repeat(32);
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let offer = pairing
            .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
            .unwrap();
        let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
        let service = Service::open(
            &root.path().join("phone/grants"),
            &receiver,
            pairing.clone(),
        )
        .unwrap();
        let app = service.router();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../android/app/src/test/resources/phone-control-v1.json"
        ))
        .unwrap();
        let (grant, period) = history_grant(
            &service,
            &app,
            &phone,
            &fixture,
            root.path(),
            vec!["operate_sessions".into()],
        )
        .await;
        let now = Instant::now();
        let first = signed_send(&fixture, &grant, &period, 200);
        let permit = service
            .authority
            .begin_read(
                &phone.device_id,
                &first,
                now + std::time::Duration::from_secs(60),
            )
            .unwrap();
        service
            .authority
            .finish_send_delivery(&permit, &pairing, &phone.credential, admitted, || true)
            .unwrap();
        let later = signed_send(&fixture, &grant, &period, 201);
        let result = service.authority.begin_read(
            &phone.device_id,
            &later,
            now + std::time::Duration::from_secs(301),
        );
        if admitted {
            assert!(result.is_ok());
        } else {
            assert!(matches!(result, Err(Refusal::Expired)));
        }
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One unlock sequence checks registration, replay and automatic replacement."
)]
async fn unlock_advertises_only_a_live_registered_controller_and_reconnects() {
    use crate::phone_control::{authority::Scope, personal_startup, socket::ControllerSocket};
    use tokio::{
        io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
        net::UnixStream,
    };

    async fn attach(path: &std::path::Path, token: &str) -> (UnixStream, String) {
        let mut peer = UnixStream::connect(path).await.unwrap();
        peer.write_all(
            format!(
                "{}\n",
                json!({"v":1,"type":"attach_write","capability":token})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut line = String::new();
        BufReader::new(&mut peer)
            .read_line(&mut line)
            .await
            .unwrap();
        let controller = serde_json::from_str::<Value>(&line).unwrap()["controller_id"]
            .as_str()
            .unwrap()
            .to_owned();
        peer.write_all(
            format!(
                "{}\n",
                json!({"v":1,"type":"register","controller_id":controller,"nonce":"77".repeat(16)})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        line.clear();
        BufReader::new(&mut peer)
            .read_line(&mut line)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["type"],
            "registered"
        );
        (peer, controller)
    }
    let root = tempfile::tempdir().unwrap();
    let capture = root.path().join("louiselm/capture");
    let tls = capture.join("tls");
    let pairing = Arc::new(PairingRegistry::open(capture.join("pairing")).unwrap());
    let receiver = pairing
        .with_personal_receiver::<_, crate::PairingError>(|unbound| {
            assert!(unbound);
            let receiver = crate::TlsIdentity::load_or_create(&tls)
                .unwrap()
                .public_key_sha256()
                .to_owned();
            Ok((receiver.clone(), Some(receiver)))
        })
        .unwrap();
    let offer = pairing
        .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
    personal_startup::prepare(&capture, &receiver, &pairing).unwrap();
    let service = Service::open(
        &capture.join("phone-control/grants.json"),
        &receiver,
        pairing,
    )
    .unwrap();
    let app = service.router();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let grant = approve_scoped_grant(
        &service,
        &phone,
        &fixture,
        Scope {
            projects: vec![root.path().to_str().unwrap().into()],
            agents: vec!["codex".into()],
            groups: vec!["read".into(), "operate_sessions".into()],
        },
    );
    let socket = root.path().join("controller.sock");
    let capability = root.path().join("capability");
    let server = ControllerSocket::bind(&socket, &capability, service.controllers.clone())
        .await
        .unwrap();
    let token = std::fs::read_to_string(&capability).unwrap();
    let server_task = tokio::spawn(server.serve());
    let (mut first, first_id) = attach(&socket, &token).await;
    let challenge_request = json!({"v":1,"type":"challenge","grant_id":grant.grant_id,
        "grant_revision":grant.grant_revision})
    .to_string();
    let (_, challenge) = post(
        app.clone(),
        "/v1/phone-control/challenge",
        &phone.credential,
        &challenge_request,
    )
    .await;
    let unlock = signed_unlock(&fixture, &challenge).to_string();
    let (_, result) = post(
        app.clone(),
        "/v1/phone-control/unlock",
        &phone.credential,
        &unlock,
    )
    .await;
    assert_eq!(result["send_supported"], true);
    assert!(service.controllers.contains(&first_id));
    let (_, replay) = post(
        app.clone(),
        "/v1/phone-control/unlock",
        &phone.credential,
        &unlock,
    )
    .await;
    assert_eq!(replay["code"], "unauthenticated");
    assert!(service.controllers.contains(&first_id));
    let (_, challenge) = post(
        app.clone(),
        "/v1/phone-control/challenge",
        &phone.credential,
        &challenge_request,
    )
    .await;
    let replacement = signed_unlock(&fixture, &challenge).to_string();
    let pending = tokio::spawn({
        let app = app.clone();
        let credential = phone.credential.clone();
        async move { post(app, "/v1/phone-control/unlock", &credential, &replacement).await }
    });
    let mut byte = [0];
    assert_eq!(
        tokio::time::timeout(std::time::Duration::from_secs(2), first.read(&mut byte))
            .await
            .unwrap()
            .unwrap(),
        0
    );
    let (_second, second_id) = attach(&socket, &token).await;
    let (_, result) = pending.await.unwrap();
    assert_eq!(result["send_supported"], true);
    assert_ne!(first_id, second_id);
    server_task.abort();
}

#[tokio::test]
async fn history_delivery_rechecks_the_pairing_after_archive_access() {
    let root = tempfile::tempdir().unwrap();
    let receiver = "01".repeat(32);
    let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
    let offer = pairing
        .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
    crate::phone_control::evidence::initialize_receiver_fixture(
        &root.path().join("phone/action-evidence"),
        &root.path().join("tls/phone-action-evidence.json"),
        &receiver,
    );
    let service = Service::open(
        &root.path().join("phone/grants"),
        &receiver,
        pairing.clone(),
    )
    .unwrap();
    let app = service.router();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let (grant, period) = history_grant(
        &service,
        &app,
        &phone,
        &fixture,
        root.path(),
        vec!["read".into()],
    )
    .await;
    let signed = signed_history_page(&fixture, &grant, &period, None, 10, 100);
    let request: Request = serde_json::from_value(signed["request"].clone()).unwrap();
    let store = service.evidence.as_ref().as_ref().unwrap();
    let history = service
        .authority
        .read_history(store, &phone.device_id, &request, Instant::now())
        .unwrap();
    assert_eq!(
        service.authority.confirm_history(
            &phone.device_id,
            &request,
            &history,
            &pairing,
            &phone.credential
        ),
        Ok(())
    );
    pairing.revoke(&phone.device_id).unwrap();
    assert_eq!(
        service.authority.confirm_history(
            &phone.device_id,
            &request,
            &history,
            &pairing,
            &phone.credential
        ),
        Err(Refusal::Revoked)
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One journal sequence verifies all retained phases over signed history."
)]
async fn history_imports_durable_native_phases_for_the_original_action() {
    use crate::phone_control::{
        ActionEvidenceKind, ActionEvidenceStore, ActionIdentity, ActionOutcome, barrier::Barrier,
        personal_startup,
    };

    let root = tempfile::tempdir().unwrap();
    let capture = root.path().join("louiselm/capture");
    let tls = capture.join("tls");
    let pairing = Arc::new(PairingRegistry::open(capture.join("pairing")).unwrap());
    let receiver = pairing
        .with_personal_receiver::<_, crate::PairingError>(|unbound| {
            assert!(unbound);
            let receiver = crate::TlsIdentity::load_or_create(&tls)
                .unwrap()
                .public_key_sha256()
                .to_owned();
            Ok((receiver.clone(), Some(receiver)))
        })
        .unwrap();
    let offer = pairing
        .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
    personal_startup::prepare(&capture, &receiver, &pairing).unwrap();
    let service = Service::open(
        &capture.join("phone-control/grants.json"),
        &receiver,
        pairing,
    )
    .unwrap();
    let app = service.router();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let (grant, period) = history_grant(
        &service,
        &app,
        &phone,
        &fixture,
        root.path(),
        vec!["read".into()],
    )
    .await;
    let archive = service.evidence.as_ref().as_ref().unwrap();
    let generation = archive.bound_generation().unwrap().to_owned();
    let identity = ActionIdentity {
        operation_id: "10".repeat(16),
        receiver_identity: receiver.clone(),
        controller_id: "11".repeat(16),
        subject_id: "12".repeat(16),
        action: 3,
        payload_hash: "13".repeat(32),
        grant_id: grant.grant_id.clone(),
        grant_revision: grant.grant_revision,
        period_id: period.clone(),
        expected_revision: 9,
        project: std::fs::canonicalize(root.path())
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
        agent: "codex".into(),
        recorded_at_ms: crate::time::now_ms(),
    };
    archive
        .reserve_session_prompt(&generation, &identity, &[])
        .unwrap();
    let owner_path = root.path().join("louiselm/workflow/phone-action-owner");
    let owner = ActionEvidenceStore::open_bound(
        &owner_path,
        tls.join("phone-action-owner.json"),
        &receiver,
        ActionEvidenceKind::Owner,
    )
    .unwrap();
    let barrier =
        Barrier::existing(&tls, &receiver, owner.bound_barrier().unwrap().clone()).unwrap();
    let accepted = ActionOutcome::Accepted {
        evidence: "14".repeat(32),
    };
    let dispatched = ActionOutcome::Dispatched {
        evidence: "15".repeat(32),
    };
    let completed = ActionOutcome::Completed {
        evidence: "16".repeat(32),
    };
    barrier
        .with_generation(&generation, || {
            owner.reserve_owner(&identity).unwrap();
            for phase in [&accepted, &dispatched, &completed] {
                owner
                    .report_under_barrier(&identity, phase, crate::time::now_ms())
                    .unwrap();
            }
        })
        .unwrap();
    drop(owner);
    let lookup = signed_history(&fixture, &grant, &period, &identity, 100);
    let (status, history) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &lookup.to_string(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(history["record"]["operation_id"], identity.operation_id);
    assert_eq!(history["record"]["state"], "completed");
    assert_eq!(history["record"]["accepted_evidence"], "14".repeat(32));
    assert_eq!(history["record"]["dispatched_evidence"], "15".repeat(32));
    assert_eq!(history["record"]["completed_evidence"], "16".repeat(32));
    assert_eq!(
        archive.lookup(&identity.operation_id).unwrap().outcome,
        completed
    );

    let mut unresolved = identity.clone();
    unresolved.operation_id = "17".repeat(16);
    archive
        .reserve_session_prompt(&generation, &unresolved, &[])
        .unwrap();
    let owner = ActionEvidenceStore::open_bound(
        &owner_path,
        tls.join("phone-action-owner.json"),
        &receiver,
        ActionEvidenceKind::Owner,
    )
    .unwrap();
    barrier
        .with_generation(&generation, || {
            owner.reserve_owner(&unresolved).unwrap();
            for phase in [&accepted, &dispatched, &ActionOutcome::Unknown] {
                owner
                    .report_under_barrier(&unresolved, phase, crate::time::now_ms())
                    .unwrap();
            }
        })
        .unwrap();
    drop(owner);
    let lookup = signed_history(&fixture, &grant, &period, &unresolved, 101);
    let (status, history) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &lookup.to_string(),
    )
    .await;
    assert_eq!(status, axum::http::StatusCode::OK);
    assert_eq!(history["record"]["state"], "unknown");
    assert_eq!(history["record"]["accepted_evidence"], "14".repeat(32));
    assert_eq!(history["record"]["dispatched_evidence"], "15".repeat(32));
    assert_eq!(history["record"]["completed_evidence"], "");

    drop(app);
    drop(service);
    std::fs::rename(&owner_path, owner_path.with_extension("lost")).unwrap();
    let pairing = Arc::new(PairingRegistry::open(capture.join("pairing")).unwrap());
    let restarted = Service::open(
        &capture.join("phone-control/grants.json"),
        &receiver,
        pairing,
    )
    .unwrap();
    let app = restarted.router();
    let period = history_period(&app, &phone, &fixture, &grant).await;
    for (operation, state, completed_evidence, sequence) in [
        (&identity, "completed", "16".repeat(32), 102),
        (&unresolved, "unknown", String::new(), 103),
    ] {
        let lookup = signed_history(&fixture, &grant, &period, operation, sequence);
        let (status, history) = post(
            app.clone(),
            "/v1/phone-control/request",
            &phone.credential,
            &lookup.to_string(),
        )
        .await;
        assert_eq!(status, axum::http::StatusCode::OK);
        assert_eq!(history["record"]["operation_id"], operation.operation_id);
        assert_eq!(history["record"]["state"], state);
        assert_eq!(history["record"]["accepted_evidence"], "14".repeat(32));
        assert_eq!(history["record"]["dispatched_evidence"], "15".repeat(32));
        assert_eq!(history["record"]["completed_evidence"], completed_evidence);
        assert!(history["record"].get("payload").is_none());
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One disposable receiver sequence covers admission, lost reply, and replay."
)]
async fn signed_send_reserves_before_dispatch_and_recovers_a_lost_reply() {
    use crate::phone_control::{
        ActionEvidenceKind, ActionEvidenceStore, ActionIdentity, ActionOutcome, barrier::Barrier,
        personal_startup, socket::ControllerSocket,
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixStream,
    };

    for scenario in [
        "accepted",
        "lost",
        "independent",
        "dependent",
        "prior_completed",
        "damaged",
        "revoked",
    ] {
        let lose_reply = scenario == "lost";
        let root = tempfile::tempdir().unwrap();
        let capture = root.path().join("louiselm/capture");
        let tls = capture.join("tls");
        let pairing = Arc::new(PairingRegistry::open(capture.join("pairing")).unwrap());
        let receiver = pairing
            .with_personal_receiver::<_, crate::PairingError>(|unbound| {
                assert!(unbound);
                let receiver = crate::TlsIdentity::load_or_create(&tls)
                    .unwrap()
                    .public_key_sha256()
                    .to_owned();
                Ok((receiver.clone(), Some(receiver)))
            })
            .unwrap();
        let offer = pairing
            .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
            .unwrap();
        let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
        personal_startup::prepare(&capture, &receiver, &pairing).unwrap();
        let service = Service::open(
            &capture.join("phone-control/grants.json"),
            &receiver,
            pairing,
        )
        .unwrap();
        let app = service.router();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../android/app/src/test/resources/phone-control-v1.json"
        ))
        .unwrap();
        let (grant, period) = history_grant(
            &service,
            &app,
            &phone,
            &fixture,
            root.path(),
            vec!["read".into(), "operate_sessions".into()],
        )
        .await;
        let socket = root.path().join("controller.sock");
        let capability = root.path().join("capability");
        let server = ControllerSocket::bind(&socket, &capability, service.controllers.clone())
            .await
            .unwrap();
        let token = std::fs::read_to_string(&capability).unwrap();
        let server_task = tokio::spawn(server.serve());
        let mut peer = UnixStream::connect(&socket).await.unwrap();
        peer.write_all(
            format!(
                "{}\n",
                json!({"v":1,"type":"attach_write","capability":token})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        let mut line = String::new();
        BufReader::new(&mut peer)
            .read_line(&mut line)
            .await
            .unwrap();
        let offer: Value = serde_json::from_str(&line).unwrap();
        let controller = offer["controller_id"].as_str().unwrap().to_owned();
        let generation = offer["generation"].as_str().unwrap().to_owned();
        peer.write_all(
            format!(
                "{}\n",
                json!({"v":1,"type":"register","controller_id":controller,"nonce":"77".repeat(16)})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
        line.clear();
        BufReader::new(&mut peer)
            .read_line(&mut line)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&line).unwrap()["type"],
            "registered"
        );
        let subject = "22".repeat(16);
        let request = signed_send_to(&fixture, &grant, &period, 123, &controller, &subject);
        let archive = service.evidence.as_ref().as_ref().unwrap();
        let old_operation = "09".repeat(16);
        if matches!(scenario, "independent" | "dependent" | "prior_completed") {
            let old = ActionIdentity {
                operation_id: old_operation.clone(),
                receiver_identity: receiver.clone(),
                controller_id: controller.clone(),
                subject_id: if matches!(scenario, "dependent" | "prior_completed") {
                    subject.clone()
                } else {
                    "08".repeat(16)
                },
                action: 3,
                payload_hash: "07".repeat(32),
                grant_id: grant.grant_id.clone(),
                grant_revision: grant.grant_revision,
                period_id: period.clone(),
                expected_revision: 1,
                project: std::fs::canonicalize(root.path())
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .into(),
                agent: "codex".into(),
                recorded_at_ms: crate::time::now_ms(),
            };
            archive
                .reserve_session_prompt(&generation, &old, &[])
                .unwrap();
            if scenario == "prior_completed" {
                let accepted = ActionOutcome::Accepted {
                    evidence: "a1".repeat(32),
                };
                archive
                    .report(&old, &accepted, crate::time::now_ms())
                    .unwrap();
                let owner_path = root.path().join("louiselm/workflow/phone-action-owner");
                let owner = ActionEvidenceStore::open_bound(
                    &owner_path,
                    tls.join("phone-action-owner.json"),
                    &receiver,
                    ActionEvidenceKind::Owner,
                )
                .unwrap();
                let barrier =
                    Barrier::existing(&tls, &receiver, owner.bound_barrier().unwrap().clone())
                        .unwrap();
                barrier
                    .with_generation(&generation, || {
                        owner.reserve_owner(&old).unwrap();
                        owner
                            .report_under_barrier(&old, &accepted, crate::time::now_ms())
                            .unwrap();
                        owner
                            .report_under_barrier(
                                &old,
                                &ActionOutcome::Completed {
                                    evidence: "a2".repeat(32),
                                },
                                crate::time::now_ms(),
                            )
                            .unwrap();
                    })
                    .unwrap();
            }
        }
        let body = json!({"v":1,"type":"request","request":request}).to_string();
        if scenario == "revoked" {
            service
                .controllers
                .cutoff(
                    crate::phone_control::barrier_log::CutoffKind::Disable,
                    &phone.device_id,
                    || service.authority.disable(&phone.device_id),
                )
                .unwrap();
            let (_, denied) =
                post(app, "/v1/phone-control/request", &phone.credential, &body).await;
            assert_eq!(denied["code"], "revoked");
            assert!(!archive.is_reserved(&request.operation_id).unwrap());
            server_task.abort();
            continue;
        }
        if scenario == "damaged" {
            std::fs::write(tls.join("phone-action-owner.json"), b"damaged").unwrap();
            let (_, denied) =
                post(app, "/v1/phone-control/request", &phone.credential, &body).await;
            assert_eq!(denied["code"], "unavailable");
            assert!(!archive.is_reserved(&request.operation_id).unwrap());
            server_task.abort();
            continue;
        }
        let pending = tokio::spawn({
            let app = app.clone();
            let credential = phone.credential.clone();
            let body = body.clone();
            async move { post(app, "/v1/phone-control/request", &credential, &body).await }
        });
        let mut reader = BufReader::new(peer);
        line.clear();
        tokio::time::timeout(
            std::time::Duration::from_secs(3),
            reader.read_line(&mut line),
        )
        .await
        .unwrap()
        .unwrap();
        let scope: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(scope["type"], "action_scope");
        let project = std::fs::canonicalize(root.path())
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let answer = json!({"v":1,"type":"action_scope","controller_id":controller,"subject_id":subject,
            "operation_id":request.operation_id,"project":project,"agent":"codex"});
        reader
            .get_mut()
            .write_all(format!("{answer}\n").as_bytes())
            .await
            .unwrap();
        if matches!(scenario, "independent" | "dependent") {
            line.clear();
            reader.read_line(&mut line).await.unwrap();
            let probe: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(probe["type"], "action_independence");
            assert_eq!(probe["unknowns"][0]["operation_id"], old_operation);
            let independent = if scenario == "independent" {
                vec![old_operation.clone()]
            } else {
                Vec::new()
            };
            let proof = json!({"v":1,"type":"action_independence","controller_id":controller,
                "subject_id":subject,"operation_id":request.operation_id,"independent":independent});
            reader
                .get_mut()
                .write_all(format!("{proof}\n").as_bytes())
                .await
                .unwrap();
            if scenario == "dependent" {
                let (_, denied) = pending.await.unwrap();
                assert_eq!(denied["code"], "unavailable");
                assert!(!archive.is_reserved(&request.operation_id).unwrap());
                server_task.abort();
                continue;
            }
        }
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let frame: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["type"], "send_prompt");
        if scenario == "prior_completed" {
            assert!(matches!(
                archive.lookup(&old_operation).unwrap().outcome,
                ActionOutcome::Completed { .. }
            ));
        }
        assert_eq!(
            frame["request"]["payload"],
            lower_hex(b"\x01A reviewed prompt")
        );
        crate::phone_control::verify_phone_control_frame(line.trim_end().as_bytes(), &receiver)
            .unwrap();
        let reserved = archive.lookup(&request.operation_id).unwrap();
        assert_eq!(reserved.outcome, ActionOutcome::Unknown);
        assert_eq!(reserved.identity.project, project);
        assert_eq!(reserved.identity.agent, "codex");
        let owner_path = root.path().join("louiselm/workflow/phone-action-owner");
        let owner = ActionEvidenceStore::open_bound(
            &owner_path,
            tls.join("phone-action-owner.json"),
            &receiver,
            ActionEvidenceKind::Owner,
        )
        .unwrap();
        let barrier =
            Barrier::existing(&tls, &receiver, owner.bound_barrier().unwrap().clone()).unwrap();
        barrier
            .with_generation(&generation, || owner.reserve_owner(&reserved.identity))
            .unwrap()
            .unwrap();
        drop(owner);
        let prepared = json!({"v":1,"type":"action_prepared","controller_id":controller,
            "subject_id":subject,"operation_id":request.operation_id});
        reader
            .get_mut()
            .write_all(format!("{prepared}\n").as_bytes())
            .await
            .unwrap();
        line.clear();
        reader.read_line(&mut line).await.unwrap();
        let commit: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(commit["type"], "action_commit");
        assert_eq!(commit["v"], 2);
        assert_eq!(commit["operation_id"], request.operation_id);
        assert!((1..=5_000).contains(&commit["remaining_ms"].as_u64().unwrap()));
        let witness = json!({"v":2,"type":"gate_use","controller_id":controller,
            "operation_id":request.operation_id,"gate_id":commit["gate_id"]});
        reader
            .get_mut()
            .write_all(format!("{witness}\n").as_bytes())
            .await
            .unwrap();
        let accepted = ActionOutcome::Accepted {
            evidence: "88".repeat(32),
        };
        let owner = ActionEvidenceStore::open_bound(
            &owner_path,
            tls.join("phone-action-owner.json"),
            &receiver,
            ActionEvidenceKind::Owner,
        )
        .unwrap();
        barrier
            .with_generation(&generation, || {
                owner.report_under_barrier(&reserved.identity, &accepted, crate::time::now_ms())
            })
            .unwrap()
            .unwrap();
        drop(owner);
        if lose_reply {
            drop(reader);
        } else {
            let result = json!({"v":1,"type":"action_result","operation_id":request.operation_id,
                "state":"accepted","code":"","evidence":"88".repeat(32)});
            reader
                .get_mut()
                .write_all(format!("{result}\n").as_bytes())
                .await
                .unwrap();
        }
        let (_, response) = pending.await.unwrap();
        if lose_reply {
            assert_eq!(response["code"], "unavailable");
            assert_eq!(
                archive.lookup(&request.operation_id).unwrap().outcome,
                ActionOutcome::Unknown
            );
        } else {
            assert_eq!(response["type"], "action_history");
            assert_eq!(response["request_id"], request.operation_id);
            assert_eq!(response["record"]["state"], "accepted");
            assert_eq!(
                archive.lookup(&request.operation_id).unwrap().outcome,
                accepted
            );
        }
        let (_, replay) = post(
            app.clone(),
            "/v1/phone-control/request",
            &phone.credential,
            &body,
        )
        .await;
        assert_ne!(
            replay["record"]["state"], "accepted",
            "a replay must not dispatch again"
        );
        let lookup = signed_history(&fixture, &grant, &period, &reserved.identity, 124);
        let (_, history) = post(
            app,
            "/v1/phone-control/request",
            &phone.credential,
            &lookup.to_string(),
        )
        .await;
        assert_eq!(history["record"]["operation_id"], request.operation_id);
        assert_eq!(history["record"]["state"], "accepted");
        assert_eq!(
            archive.lookup(&request.operation_id).unwrap().outcome,
            accepted
        );
        server_task.abort();
    }
}
