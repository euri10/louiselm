//! Signed durable Run reads remain distinct from live controller authority.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "test fixtures and assertions"
)]

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::time::Instant;
use std::{os::unix::fs::PermissionsExt, sync::Arc};

use super::super::{Service, authority::StoredGrant, fixed, prefix};
use super::tests::{history_grant, post, unhex};
use crate::{PairingRegistry, RunAdmission, RunProjectBinding, RunSession, hex::lower_hex};

#[expect(
    clippy::too_many_arguments,
    reason = "the signed fixture spells out each wire field"
)]
fn signed_run(
    fixture: &Value,
    grant: &StoredGrant,
    period: &str,
    action: u8,
    subject: &str,
    payload: &[u8],
    revision: u64,
    operation: u128,
) -> Value {
    let zero = "00".repeat(16);
    let hash = lower_hex(&Sha256::digest(payload));
    let operation = format!("{operation:032x}");
    let mut bytes = prefix(2);
    bytes.extend(fixed::<16>(&grant.grant_id).unwrap());
    bytes.extend(grant.grant_revision.to_be_bytes());
    bytes.extend(fixed::<16>(period).unwrap());
    bytes.extend(fixed::<32>(&grant.receiver_identity).unwrap());
    bytes.extend(fixed::<16>(&zero).unwrap());
    bytes.extend(fixed::<16>(subject).unwrap());
    bytes.push(action);
    bytes.extend(fixed::<32>(&hash).unwrap());
    bytes.extend(fixed::<16>(&operation).unwrap());
    bytes.extend(revision.to_be_bytes());
    bytes.push(1);
    let key = EcdsaKeyPair::from_pkcs8(
        &ECDSA_P256_SHA256_ASN1_SIGNING,
        &unhex(fixture["keys"]["working"]["pkcs8"].as_str().unwrap()),
    )
    .unwrap();
    let signature = key.sign(&SystemRandom::new(), &bytes).unwrap();
    json!({"v":1,"type":"request","request":{
        "grant_id":grant.grant_id,"grant_revision":grant.grant_revision,"period_id":period,
        "receiver_identity":grant.receiver_identity,"controller_id":zero,"subject_id":subject,
        "action":action,"payload":lower_hex(payload),"payload_hash":hash,"operation_id":operation,
        "expected_revision":revision,"interaction":1,"signature":lower_hex(signature.as_ref())
    }})
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one signed lifecycle exercises the same grant across scope and revocation changes"
)]
async fn durable_run_reads_are_scoped_and_fenced_against_contributor_growth() {
    use super::super::{
        prepared_owner::{PreparedAdmission, Publication, admit_with, publish_with},
        prepared_owner_tests::selection,
        source::PhoneSourceStore,
        source_checkout_tests::{materialize, setup},
    };
    let (root, sources, source_id) = setup();
    let run = "11111111-1111-4111-8111-111111111111";
    let subject = run.replace('-', "");
    let checkout = sources
        .prepare_checkout_with(run, &source_id, &root.path().join("checkout"), materialize)
        .unwrap();
    std::fs::create_dir(root.path().join("phone")).unwrap();
    std::fs::rename(
        root.path().join("sources"),
        root.path().join("phone/run-sources"),
    )
    .unwrap();
    std::fs::rename(
        root.path().join("projects"),
        root.path().join("phone/project-identities"),
    )
    .unwrap();
    let sources = PhoneSourceStore::open(root.path().join("phone/run-sources")).unwrap();
    let project_id = sources.read(run).unwrap().project_id;
    let prepared = crate::PreparedRunStore::open(root.path().join("phone/prepared-runs")).unwrap();
    let runs = crate::RunStore::new(root.path().join("runs")).unwrap();
    let selection_path = root.path().join("selection.json");
    let bytes = selection(
        root.path(),
        &checkout.head,
        "private instruction not in Run list",
    );
    std::fs::write(&selection_path, &bytes).unwrap();
    std::fs::set_permissions(&selection_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let publication = Publication {
        run_id: run,
        source_id: &source_id,
        checkout_id: &checkout.checkout_id,
        selection: &selection_path,
        previous: None,
    };
    let saved = publish_with(&sources, &prepared, &runs, &publication, |_| Ok(())).unwrap();
    let digest = lower_hex(&Sha256::digest(bytes.as_bytes()));
    let admission = PreparedAdmission {
        registration_id: &saved.id,
        revision: saved.revision,
        selection_sha256: &digest,
        token: "generate-token-1234",
    };
    admit_with(&sources, &prepared, &runs, &admission, |_| Ok(())).unwrap();
    let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
    let receiver = "01".repeat(32);
    let offer = pairing
        .issue("https://192.168.1.2:7391", &receiver, 1_000, 10_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "phone", 1_001).unwrap();
    let mut service = Service::open(
        &root.path().join("phone/grants"),
        &receiver,
        pairing.clone(),
    )
    .unwrap()
    .with_runs(Some(runs.clone()));
    service
        .prepared
        .as_mut()
        .unwrap()
        .set_test_integration(|| Ok(()));
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
    service
        .authority
        .link_project_roots(&project_id, grant.project_roots().unwrap().unwrap())
        .unwrap();
    let old_id = "00000000-0000-4000-8000-000000000001";
    runs.admit(
        RunAdmission {
            id: old_id.into(),
            generated_work_ceiling: 3,
            park_ttl_ms: 1000,
        },
        "old-generate-token",
    )
    .unwrap();
    let foreign_path = root.path().join("foreign");
    std::fs::create_dir(&foreign_path).unwrap();
    let foreign_root = service
        .authority
        .register_root(foreign_path.to_str().unwrap())
        .unwrap();
    let foreign_project = service
        .authority
        .register_project(foreign_path.to_str().unwrap(), &[foreign_root])
        .unwrap();
    let foreign_id = "00000000-0000-4000-8000-000000000002";
    runs.admit_phone_bound(
        RunAdmission {
            id: foreign_id.into(),
            generated_work_ceiling: 3,
            park_ttl_ms: 1000,
        },
        "foreign-generate-token",
        RunProjectBinding {
            run_id: foreign_id.into(),
            project_id: foreign_project,
            source_id: "aa".repeat(16),
            checkout_id: "bb".repeat(16),
            agents: vec!["codex".into()],
        },
    )
    .unwrap();
    let signed = signed_run(&fixture, &grant, &period, 55, &subject, &[], 1, 899);
    let signed: super::SignedRequest = serde_json::from_value(signed).unwrap();
    let mut permit = service
        .authority
        .begin_read(&phone.device_id, &signed.request, Instant::now())
        .unwrap();
    let mut frame: Value = serde_json::from_slice(&permit.frame).unwrap();
    frame["request"]["expected_revision"] = 2.into();
    permit.frame = serde_json::to_vec(&frame).unwrap();
    let reader = service.prepared.as_ref().unwrap().reader();
    assert_eq!(
        service.authority.finish_owner_delivery(
            &permit,
            &pairing,
            &phone.credential,
            Some(&reader)
        ),
        Err(super::Refusal::Unauthenticated)
    );
    let list = signed_run(
        &fixture,
        &grant,
        &period,
        54,
        &"00".repeat(16),
        &[0; 17],
        0,
        900,
    );
    let (_, page) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &list.to_string(),
    )
    .await;
    assert_eq!(page["type"], "run_page");
    assert_eq!(page["items"][0]["run_id"], run);
    assert!(!page.to_string().contains(old_id));
    assert!(!page.to_string().contains(foreign_id));
    assert!(!page.to_string().contains("private instruction"));
    let mut after = vec![1];
    after.extend(fixed::<16>(&subject).unwrap());
    let end = signed_run(
        &fixture,
        &grant,
        &period,
        54,
        &"00".repeat(16),
        &after,
        0,
        905,
    );
    let (_, end) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &end.to_string(),
    )
    .await;
    assert_eq!(end["items"], json!([]));
    let detail = signed_run(&fixture, &grant, &period, 55, &subject, &[], 1, 901);
    let (_, detail) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &detail.to_string(),
    )
    .await;
    assert_eq!(detail["type"], "run_detail");
    assert_eq!(detail["run"]["state"], "admitted");
    assert_eq!(detail["run"]["controller"], "not_checked");
    assert_eq!(detail["run"]["work"]["remaining"], 3);
    assert_eq!(detail["run"]["load_session"], "unknown");
    assert_eq!(detail["run"]["resume_deadline_ms"], 0);
    let next_offer = pairing
        .issue("https://192.168.1.2:7391", &receiver, 1_000, 10_000)
        .unwrap();
    let no_read_phone = pairing
        .consume(&next_offer.token, "no read", 1_001)
        .unwrap();
    let (no_read, no_read_period) = history_grant(
        &service,
        &app,
        &no_read_phone,
        &fixture,
        root.path(),
        vec!["control_runs".into()],
    )
    .await;
    let no_read_request = signed_run(
        &fixture,
        &no_read,
        &no_read_period,
        55,
        &subject,
        &[],
        0,
        908,
    );
    let (_, no_read_result) = post(
        app.clone(),
        "/v1/phone-control/request",
        &no_read_phone.credential,
        &no_read_request.to_string(),
    )
    .await;
    assert_eq!(no_read_result["code"], "out_of_scope");
    runs.attach(RunSession {
        id: run.into(),
        session_id: "codex/acp".into(),
        agent: "codex".into(),
        acp_session_id: "acp".into(),
        working_dir: "/private/worker".into(),
        load_session: false,
    })
    .unwrap();
    let stale = signed_run(&fixture, &grant, &period, 55, &subject, &[], 1, 902);
    let (_, stale) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &stale.to_string(),
    )
    .await;
    assert_eq!(stale["code"], "stale_subject");
    let active = signed_run(&fixture, &grant, &period, 55, &subject, &[], 2, 906);
    let (_, active) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &active.to_string(),
    )
    .await;
    assert_eq!(active["run"]["state"], "active");
    assert_eq!(active["run"]["load_session"], "unsupported");
    assert!(!active.to_string().contains("/private/worker"));
    let pending = signed_run(&fixture, &grant, &period, 55, &subject, &[], 0, 909);
    let pending: super::SignedRequest = serde_json::from_value(pending).unwrap();
    let pending = service
        .authority
        .begin_read(&phone.device_id, &pending.request, Instant::now())
        .unwrap();
    runs.add_phone_contributor(run, "claude").unwrap();
    assert_eq!(
        service.authority.finish_owner_delivery(
            &pending,
            &pairing,
            &phone.credential,
            Some(&reader)
        ),
        Err(super::Refusal::OutOfScope)
    );
    let denied = signed_run(&fixture, &grant, &period, 55, &subject, &[], 0, 903);
    let (_, denied) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &denied.to_string(),
    )
    .await;
    assert_eq!(denied["code"], "out_of_scope");
    let list = signed_run(
        &fixture,
        &grant,
        &period,
        54,
        &"00".repeat(16),
        &[0; 17],
        0,
        904,
    );
    let (_, page) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &list.to_string(),
    )
    .await;
    assert_eq!(page["items"], json!([]));
    let pending = signed_run(&fixture, &grant, &period, 55, &subject, &[], 0, 910);
    let pending: super::SignedRequest = serde_json::from_value(pending).unwrap();
    let pending = service
        .authority
        .begin_read(&phone.device_id, &pending.request, Instant::now())
        .unwrap();
    service.authority.disable(&phone.device_id).unwrap();
    assert_eq!(
        service.authority.finish_owner_delivery(
            &pending,
            &pairing,
            &phone.credential,
            Some(&reader)
        ),
        Err(super::Refusal::Revoked)
    );
    let revoked = signed_run(&fixture, &grant, &period, 55, &subject, &[], 0, 907);
    let (_, revoked) = post(
        app,
        "/v1/phone-control/request",
        &phone.credential,
        &revoked.to_string(),
    )
    .await;
    assert_eq!(revoked["code"], "revoked");
}
