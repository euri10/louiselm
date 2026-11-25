//! Protected prepared-draft read contract and signed phone fixture.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Instant};

use super::super::{
    Service,
    authority::{Scope, StoredGrant},
    fixed, prefix,
};
use super::tests::{history_grant, post, scoped_grant, unhex};
use crate::{PairingRegistry, hex::lower_hex};

fn signed_prepared(
    fixture: &Value,
    grant: &StoredGrant,
    period: &str,
    registration: &str,
    revision: u64,
    operation: u128,
) -> Value {
    let controller = "00".repeat(16);
    let digest = lower_hex(&Sha256::digest([]));
    let operation = format!("{operation:032x}");
    let mut bytes = prefix(2);
    bytes.extend(fixed::<16>(&grant.grant_id).unwrap());
    bytes.extend(grant.grant_revision.to_be_bytes());
    bytes.extend(fixed::<16>(period).unwrap());
    bytes.extend(fixed::<32>(&grant.receiver_identity).unwrap());
    bytes.extend(fixed::<16>(&controller).unwrap());
    bytes.extend(fixed::<16>(registration).unwrap());
    bytes.push(53);
    bytes.extend(fixed::<32>(&digest).unwrap());
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
        "receiver_identity":grant.receiver_identity,"controller_id":controller,"subject_id":registration,
        "action":53,"payload":"","payload_hash":digest,"operation_id":operation,
        "expected_revision":revision,"interaction":1,"signature":lower_hex(signature.as_ref())
    }})
}

fn signed_prepared_list(
    fixture: &Value,
    grant: &StoredGrant,
    period: &str,
    after: &str,
    operation: u128,
) -> Value {
    let zero = "00".repeat(16);
    let payload = fixed::<16>(after).unwrap();
    let digest = lower_hex(&Sha256::digest(payload));
    let operation = format!("{operation:032x}");
    let mut bytes = prefix(2);
    bytes.extend(fixed::<16>(&grant.grant_id).unwrap());
    bytes.extend(grant.grant_revision.to_be_bytes());
    bytes.extend(fixed::<16>(period).unwrap());
    bytes.extend(fixed::<32>(&grant.receiver_identity).unwrap());
    bytes.extend(fixed::<16>(&zero).unwrap());
    bytes.extend(fixed::<16>(&zero).unwrap());
    bytes.push(52);
    bytes.extend(fixed::<32>(&digest).unwrap());
    bytes.extend(fixed::<16>(&operation).unwrap());
    bytes.extend(0_u64.to_be_bytes());
    bytes.push(1);
    let key = EcdsaKeyPair::from_pkcs8(
        &ECDSA_P256_SHA256_ASN1_SIGNING,
        &unhex(fixture["keys"]["working"]["pkcs8"].as_str().unwrap()),
    )
    .unwrap();
    let signature = key.sign(&SystemRandom::new(), &bytes).unwrap();
    json!({"v":1,"type":"request","request":{
        "grant_id":grant.grant_id,"grant_revision":grant.grant_revision,"period_id":period,
        "receiver_identity":grant.receiver_identity,"controller_id":zero,"subject_id":zero,
        "action":52,"payload":after,"payload_hash":digest,"operation_id":operation,
        "expected_revision":0,"interaction":1,"signature":lower_hex(signature.as_ref())
    }})
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one signed lifecycle covers scope, replay, removal and revocation"
)]
async fn prepared_detail_requires_current_phone_scope_without_a_controller() {
    use super::super::{
        prepared_owner::{Publication, publish_with},
        prepared_owner_tests::selection,
        source::PhoneSourceStore,
        source_checkout_tests::{materialize, setup},
    };
    use std::os::unix::fs::PermissionsExt;
    let (root, sources, source_id) = setup();
    let run = "11111111-1111-4111-8111-111111111111";
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
    std::fs::write(
        &selection_path,
        selection(root.path(), &checkout.head, "private instructions"),
    )
    .unwrap();
    std::fs::set_permissions(&selection_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let publication = Publication {
        run_id: run,
        source_id: &source_id,
        checkout_id: &checkout.checkout_id,
        selection: &selection_path,
        previous: None,
    };
    let registered = publish_with(&sources, &prepared, &runs, &publication, |_| Ok(())).unwrap();
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
    .with_runs(Some(runs));
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
    // A foreign registration has no source proof. Discovery must skip it
    // without leaking its existence or attempting to read that proof.
    let foreign = root.path().join("foreign");
    std::fs::create_dir(&foreign).unwrap();
    let foreign_root = service
        .authority
        .register_root(foreign.to_str().unwrap())
        .unwrap();
    let foreign_project = service
        .authority
        .register_project(foreign.to_str().unwrap(), &[foreign_root])
        .unwrap();
    let foreign_binding = crate::RunProjectBinding {
        run_id: "22222222-2222-4222-8222-222222222222".into(),
        project_id: foreign_project,
        source_id: "aa".repeat(16),
        checkout_id: "bb".repeat(16),
        agents: vec!["codex".into()],
    };
    prepared
        .publish(
            None,
            &foreign_binding,
            &std::fs::read_to_string(&selection_path).unwrap(),
        )
        .unwrap();
    let signed = signed_prepared(
        &fixture,
        &grant,
        &period,
        &registered.id,
        registered.revision,
        500,
    );
    let signed: super::SignedRequest = serde_json::from_value(signed).unwrap();
    let mut permit = service
        .authority
        .begin_read(&phone.device_id, &signed.request, Instant::now())
        .unwrap();
    let mut frame: Value = serde_json::from_slice(&permit.frame).unwrap();
    frame["request"]["expected_revision"] = (registered.revision + 1).into();
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
    let request = signed_prepared(
        &fixture,
        &grant,
        &period,
        &registered.id,
        registered.revision,
        501,
    );
    let (_, detail) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(detail["type"], "prepared_detail");
    assert_eq!(detail["run_id"], run);
    assert_eq!(
        detail["detail"]["beads"][0]["prompt"],
        "private instructions"
    );
    assert!(!detail.to_string().contains(root.path().to_str().unwrap()));
    let page = signed_prepared_list(&fixture, &grant, &period, &"00".repeat(16), 600);
    let (_, list) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &page.to_string(),
    )
    .await;
    assert_eq!(list["type"], "prepared_page");
    assert_eq!(list["items"][0]["registration_id"], registered.id);
    assert_eq!(list["items"][0]["run_id"], run);
    assert_eq!(list["next_after"], registered.id);
    assert!(!list.to_string().contains("private instructions"));
    assert!(!list.to_string().contains(root.path().to_str().unwrap()));
    let (_, replayed_page) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &page.to_string(),
    )
    .await;
    assert_eq!(replayed_page["code"], "unauthenticated");
    let mut malformed_page = signed_prepared_list(&fixture, &grant, &period, &"00".repeat(16), 602);
    malformed_page["request"]["subject_id"] = registered.id.clone().into();
    let (_, malformed) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &malformed_page.to_string(),
    )
    .await;
    assert_eq!(malformed["code"], "malformed");
    let page = signed_prepared_list(&fixture, &grant, &period, &registered.id, 601);
    let (_, end) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &page.to_string(),
    )
    .await;
    assert_eq!(end["items"], serde_json::json!([]));
    assert_eq!(end["next_after"], "");
    let (_, replay) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(replay["code"], "unauthenticated");
    let stale = signed_prepared(
        &fixture,
        &grant,
        &period,
        &registered.id,
        registered.revision + 1,
        502,
    );
    let (_, refused) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &stale.to_string(),
    )
    .await;
    assert_eq!(refused["code"], "stale_subject");
    let mut altered = signed_prepared(
        &fixture,
        &grant,
        &period,
        &registered.id,
        registered.revision,
        504,
    );
    altered["request"]["expected_revision"] = (registered.revision + 1).into();
    let (_, refused) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &altered.to_string(),
    )
    .await;
    assert_eq!(refused["code"], "unauthenticated");

    for (index, selected, agents, groups) in [
        (510, root.path(), vec!["other".into()], vec!["read".into()]),
        (
            511,
            root.path(),
            vec!["codex".into()],
            vec!["operate_sessions".into()],
        ),
        (
            512,
            foreign.as_path(),
            vec!["codex".into()],
            vec!["read".into()],
        ),
    ] {
        let offer = pairing
            .issue("https://192.168.1.2:7391", &receiver, 1_000, 10_000)
            .unwrap();
        let other = pairing.consume(&offer.token, "other phone", 1_001).unwrap();
        let (other_grant, other_period) = scoped_grant(
            &service,
            &app,
            &other,
            &fixture,
            Scope {
                projects: vec![selected.to_str().unwrap().into()],
                agents,
                groups,
            },
        )
        .await;
        if selected == root.path() {
            service
                .authority
                .link_project_roots(&project_id, other_grant.project_roots().unwrap().unwrap())
                .unwrap();
        }
        let request = signed_prepared(
            &fixture,
            &other_grant,
            &other_period,
            &registered.id,
            registered.revision,
            index,
        );
        let (_, denied) = post(
            app.clone(),
            "/v1/phone-control/request",
            &other.credential,
            &request.to_string(),
        )
        .await;
        assert_eq!(denied["code"], "out_of_scope");
        let page = signed_prepared_list(
            &fixture,
            &other_grant,
            &other_period,
            &"00".repeat(16),
            index + 100,
        );
        let (_, list) = post(
            app.clone(),
            "/v1/phone-control/request",
            &other.credential,
            &page.to_string(),
        )
        .await;
        if index == 511 {
            assert_eq!(list["code"], "out_of_scope");
        } else {
            assert_eq!(list["items"], serde_json::json!([]));
        }
    }
    let offer = pairing
        .issue("https://192.168.1.2:7391", &receiver, 1_000, 10_000)
        .unwrap();
    let paired_other = pairing
        .consume(&offer.token, "revoked phone", 1_001)
        .unwrap();
    let (other_grant, other_period) = history_grant(
        &service,
        &app,
        &paired_other,
        &fixture,
        root.path(),
        vec!["read".into()],
    )
    .await;
    service
        .authority
        .link_project_roots(&project_id, other_grant.project_roots().unwrap().unwrap())
        .unwrap();
    let other_page =
        signed_prepared_list(&fixture, &other_grant, &other_period, &"00".repeat(16), 617);
    let (_, visible) = post(
        app.clone(),
        "/v1/phone-control/request",
        &paired_other.credential,
        &other_page.to_string(),
    )
    .await;
    assert_eq!(visible["items"][0]["registration_id"], registered.id);
    let signed: super::SignedRequest = serde_json::from_value(signed_prepared(
        &fixture,
        &other_grant,
        &other_period,
        &registered.id,
        registered.revision,
        517,
    ))
    .unwrap();
    let pending = service
        .authority
        .begin_read(&paired_other.device_id, &signed.request, Instant::now())
        .unwrap();
    pairing.revoke(&paired_other.device_id).unwrap();
    assert_eq!(
        service.authority.finish_owner_delivery(
            &pending,
            &pairing,
            &paired_other.credential,
            Some(&reader)
        ),
        Err(super::Refusal::Revoked)
    );
    service
        .prepared
        .as_mut()
        .unwrap()
        .set_test_integration(|| Err(super::super::source::PhoneSourceError::Unsupported));
    let absent_app = service.router();
    let absent = signed_prepared(
        &fixture,
        &grant,
        &period,
        &registered.id,
        registered.revision,
        513,
    );
    let (_, refused) = post(
        absent_app,
        "/v1/phone-control/request",
        &phone.credential,
        &absent.to_string(),
    )
    .await;
    assert_eq!(refused["code"], "unsupported");
    let absent_page = signed_prepared_list(&fixture, &grant, &period, &"00".repeat(16), 622);
    let (_, refused) = post(
        service.router(),
        "/v1/phone-control/request",
        &phone.credential,
        &absent_page.to_string(),
    )
    .await;
    assert_eq!(refused["code"], "unsupported");
    std::fs::remove_file(&selection_path).unwrap();
    std::fs::rename(
        root.path().join("project"),
        root.path().join("moved-project"),
    )
    .unwrap();
    let historical = signed_prepared(
        &fixture,
        &grant,
        &period,
        &registered.id,
        registered.revision,
        514,
    );
    let (_, detail) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &historical.to_string(),
    )
    .await;
    assert_eq!(
        detail["detail"]["beads"][0]["prompt"],
        "private instructions"
    );
    let signed: super::SignedRequest = serde_json::from_value(signed_prepared(
        &fixture,
        &grant,
        &period,
        &registered.id,
        registered.revision,
        518,
    ))
    .unwrap();
    let pending = service
        .authority
        .begin_read(&phone.device_id, &signed.request, Instant::now())
        .unwrap();
    let signed_list: super::SignedRequest = serde_json::from_value(signed_prepared_list(
        &fixture,
        &grant,
        &period,
        &"00".repeat(16),
        621,
    ))
    .unwrap();
    let pending_list = service
        .authority
        .begin_read(&phone.device_id, &signed_list.request, Instant::now())
        .unwrap();
    prepared
        .unpublish(&registered.id, registered.revision)
        .unwrap();
    assert_eq!(
        service.authority.finish_owner_delivery(
            &pending,
            &pairing,
            &phone.credential,
            Some(&reader)
        ),
        Err(super::Refusal::StaleSubject)
    );
    let empty = service
        .authority
        .finish_owner_delivery(&pending_list, &pairing, &phone.credential, Some(&reader))
        .unwrap();
    assert_eq!(empty["items"], serde_json::json!([]));
    let removed = signed_prepared(
        &fixture,
        &grant,
        &period,
        &registered.id,
        registered.revision,
        515,
    );
    let (_, refused) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &removed.to_string(),
    )
    .await;
    assert_eq!(refused["code"], "stale_subject");
    let page = signed_prepared_list(&fixture, &grant, &period, &"00".repeat(16), 620);
    let (_, empty) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &page.to_string(),
    )
    .await;
    assert_eq!(empty["items"], serde_json::json!([]));
    let signed: super::SignedRequest = serde_json::from_value(signed_prepared(
        &fixture,
        &grant,
        &period,
        &registered.id,
        registered.revision,
        519,
    ))
    .unwrap();
    let pending = service
        .authority
        .begin_read(&phone.device_id, &signed.request, Instant::now())
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
    let revoked = signed_prepared(
        &fixture,
        &grant,
        &period,
        &registered.id,
        registered.revision,
        503,
    );
    let (_, refused) = post(
        app,
        "/v1/phone-control/request",
        &phone.credential,
        &revoked.to_string(),
    )
    .await;
    assert_eq!(refused["code"], "revoked");
}
