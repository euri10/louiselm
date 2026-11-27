//! Scoped reads of removed action details, using disposable phone grants.
#![allow(clippy::unwrap_used, reason = "test fixtures and assertions")]

use std::sync::Arc;

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use super::tests::{history_grant, history_period, post, signed_history, signed_history_page};
use crate::{
    PairingRegistry,
    hex::lower_hex,
    phone_control::{ActionIdentity, ActionOutcome, Service, barrier, evidence},
};

fn fresh_phone_fixture(template: &Value) -> Value {
    let mut fixture = template.clone();
    for name in ["enrolled", "working"] {
        let pkcs8 =
            EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &SystemRandom::new())
                .unwrap();
        let key =
            EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref()).unwrap();
        let public = key.public_key().as_ref();
        fixture["keys"][name]["pkcs8"] = json!(lower_hex(pkcs8.as_ref()));
        fixture["keys"][name]["sec1"] = json!(lower_hex(public));
        if name == "enrolled" {
            fixture["proof"]["enrolled_key_hash"] = json!(lower_hex(&Sha256::digest(public)));
        }
    }
    fixture
}

async fn admin(socket: &std::path::Path, capability: &str, operation_id: &str) -> Value {
    let mut stream = tokio::net::UnixStream::connect(socket).await.unwrap();
    let request = json!({"v":1,"type":"remove_action_details","capability":capability,"operation_id":operation_id});
    stream
        .write_all(format!("{request}\n").as_bytes())
        .await
        .unwrap();
    let mut response = String::new();
    BufReader::new(stream)
        .read_line(&mut response)
        .await
        .unwrap();
    serde_json::from_str(&response).unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one scenario follows a scoped history record across phone and receiver replacement"
)]
async fn removed_details_remain_readable_only_to_a_currently_authorized_phone() {
    let root = tempfile::tempdir().unwrap();
    let tls = root.path().join("tls");
    std::fs::create_dir(&tls).unwrap();
    let (receiver, generation, _registrar) = barrier::receiver_fixture(&tls);
    let archive = root.path().join("phone/action-evidence");
    let guard = tls.join("phone-action-evidence.json");
    evidence::initialize_receiver_fixture(&archive, &guard, &receiver);
    evidence::bind_receiver_fixture(&archive, &guard, &receiver, &generation);
    let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
    let phone = |name: &str| {
        let offer = pairing
            .issue("https://192.168.1.2:7391", &receiver, 1_000, 10_000)
            .unwrap();
        pairing.consume(&offer.token, name, 1_001).unwrap()
    };
    let old = phone("old disposable phone");
    let replacement = phone("replacement disposable phone");
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let replacement_fixture = fresh_phone_fixture(&fixture);
    assert_ne!(
        fixture["keys"]["enrolled"]["sec1"],
        replacement_fixture["keys"]["enrolled"]["sec1"]
    );
    let service = Service::open(
        &root.path().join("phone/grants"),
        &receiver,
        pairing.clone(),
    )
    .unwrap();
    let app = service.router();
    let admin_path = root.path().join("admin.sock");
    let capability_path = root.path().join("capability");
    let sockets = service
        .bind(
            &admin_path,
            &root.path().join("controller.sock"),
            &capability_path,
        )
        .await
        .unwrap();
    let server = tokio::spawn(sockets.serve());
    let capability = std::fs::read_to_string(capability_path).unwrap();
    let (old_grant, old_period) = history_grant(
        &service,
        &app,
        &old,
        &fixture,
        root.path(),
        vec!["read".into()],
    )
    .await;
    let original = ActionIdentity {
        operation_id: "10".repeat(16),
        receiver_identity: receiver.clone(),
        controller_id: "11".repeat(16),
        subject_id: "12".repeat(16),
        action: 3,
        payload_hash: "13".repeat(32),
        grant_id: old_grant.grant_id.clone(),
        grant_revision: old_grant.grant_revision,
        period_id: old_period.clone(),
        expected_revision: 9,
        project: std::fs::canonicalize(root.path())
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
        agent: "codex".into(),
        recorded_at_ms: 1_000,
    };
    let store = service.evidence.as_ref().as_ref().unwrap();
    store.reserve(&generation, &original).unwrap();
    assert_eq!(
        admin(&admin_path, "incorrect", &original.operation_id).await["code"],
        "unauthenticated"
    );
    assert_eq!(
        admin(&admin_path, capability.trim(), &original.operation_id).await["code"],
        "not_completed"
    );
    assert_eq!(
        admin(&admin_path, capability.trim(), "bad-id").await["code"],
        "malformed"
    );
    let mut other_action = original.clone();
    other_action.operation_id = "21".repeat(16);
    other_action.action = 4;
    store.reserve(&generation, &other_action).unwrap();
    let other_outcome = ActionOutcome::Completed {
        evidence: "18".repeat(32),
    };
    store.report(&other_action, &other_outcome, 2_000).unwrap();
    assert_eq!(
        admin(&admin_path, capability.trim(), &other_action.operation_id).await["code"],
        "unsupported"
    );
    assert_eq!(
        store.lookup(&other_action.operation_id).unwrap().outcome,
        other_outcome
    );
    store
        .report(
            &original,
            &ActionOutcome::Completed {
                evidence: "17".repeat(32),
            },
            2_000,
        )
        .unwrap();
    assert_eq!(
        admin(&admin_path, capability.trim(), &original.operation_id).await["type"],
        "action_details_removed"
    );
    assert_eq!(
        admin(&admin_path, capability.trim(), &original.operation_id).await["type"],
        "action_details_removed"
    );
    let (grant, _) = history_grant(
        &service,
        &app,
        &replacement,
        &replacement_fixture,
        root.path(),
        vec!["read".into()],
    )
    .await;
    service.authority.disable(&old.device_id).unwrap();
    let denied_request = signed_history(&fixture, &old_grant, &old_period, &original, 101);
    let (_, denied) = post(
        app.clone(),
        "/v1/phone-control/request",
        &old.credential,
        &denied_request.to_string(),
    )
    .await;
    assert_eq!(denied["code"], "revoked");
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    drop(app);
    drop(service);

    let service = Service::open(&root.path().join("phone/grants"), &receiver, pairing).unwrap();
    let app = service.router(); // no editor/controller is attached
    let period = history_period(&app, &replacement, &replacement_fixture, &grant).await;
    let lookup = signed_history(&replacement_fixture, &grant, &period, &original, 102);
    let (_, result) = post(
        app.clone(),
        "/v1/phone-control/request",
        &replacement.credential,
        &lookup.to_string(),
    )
    .await;
    assert_eq!(result["record"]["operation_id"], original.operation_id);
    assert_eq!(result["record"]["state"], "details_removed");
    for field in [
        "evidence",
        "accepted_evidence",
        "dispatched_evidence",
        "completed_evidence",
    ] {
        assert_eq!(result["record"][field], "");
    }
    let page = signed_history_page(&replacement_fixture, &grant, &period, None, 10, 103);
    let (_, result) = post(
        app.clone(),
        "/v1/phone-control/request",
        &replacement.credential,
        &page.to_string(),
    )
    .await;
    assert_eq!(result["records"][0]["state"], "details_removed");
    let other = tempfile::tempdir().unwrap();
    let (out_of_scope, next_period) = history_grant(
        &service,
        &app,
        &replacement,
        &replacement_fixture,
        other.path(),
        vec!["read".into()],
    )
    .await;
    let lookup = signed_history(
        &replacement_fixture,
        &out_of_scope,
        &next_period,
        &original,
        104,
    );
    let (_, denied) = post(
        app.clone(),
        "/v1/phone-control/request",
        &replacement.credential,
        &lookup.to_string(),
    )
    .await;
    assert_eq!(denied["code"], "out_of_scope");
    let (no_read, next_period) = history_grant(
        &service,
        &app,
        &replacement,
        &replacement_fixture,
        root.path(),
        vec!["operate_sessions".into()],
    )
    .await;
    let lookup = signed_history(&replacement_fixture, &no_read, &next_period, &original, 105);
    let (_, denied) = post(
        app,
        "/v1/phone-control/request",
        &replacement.credential,
        &lookup.to_string(),
    )
    .await;
    assert_eq!(denied["code"], "out_of_scope");
}
