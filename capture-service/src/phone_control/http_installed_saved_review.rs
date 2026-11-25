//! Installed saved-result owner through the real protected phone HTTP route.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "disposable installed fixture setup and assertions"
)]

use std::{os::unix::fs::PermissionsExt, path::Path, sync::Arc};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::super::{
    Service,
    authority::Scope,
    prepared_owner::{PreparedAdmission, PreparedOwner, Publication},
    prepared_owner_tests::selection,
    source::PhoneSourceStore,
};
use super::{
    run_tests::signed_run,
    tests::{scoped_grant, unhex},
};
use crate::{PairingRegistry, PhoneProjectStore, RunDraft, RunStore, hex::lower_hex};

const RUN: &str = "44444444-4444-4444-8444-444444444444";
const TEST: &str = "phone_control::http::installed_saved_review::installed_saved_review";

/// The launcher fixture runs prepare before its producer and review after Disposal.
#[tokio::test]
#[ignore = "requires the installed broker fixture in a disposable launcher VM"]
async fn installed_saved_review() {
    let root = std::env::var_os("LOUISELM_TEST_REVIEW_ROOT").expect("fixture root");
    let root = Path::new(&root);
    match std::env::var("LOUISELM_TEST_REVIEW_PHASE").as_deref() {
        Ok("prepare") => prepare(root),
        Ok("review") => review(root).await,
        _ => panic!("{TEST} requires an explicit fixture phase"),
    }
}

fn prepare(root: &Path) {
    let phone = root.join("phone");
    std::fs::create_dir(&phone).unwrap();
    std::fs::set_permissions(&phone, std::fs::Permissions::from_mode(0o700)).unwrap();
    let projects = PhoneProjectStore::open(phone.join("project-identities")).unwrap();
    let project = root.join("project");
    let approved = projects.register_root(&project).unwrap();
    let project_id = projects.register_project(&project, &[approved]).unwrap();
    let sources = PhoneSourceStore::open(phone.join("run-sources")).unwrap();
    let source = sources
        .freeze(
            &projects,
            &project_id,
            RUN,
            &root.join("phone-snapshot"),
            &[],
        )
        .unwrap();
    let checkout = sources
        .prepare_checkout(RUN, &source.source_id, &root.join("checkout"))
        .unwrap();
    let runs = RunStore::new(root.join("runs")).unwrap();
    let owner = PreparedOwner::new(phone, runs.clone());
    let mut input: Value =
        serde_json::from_str(&selection(root, &checkout.head, "fixture")).unwrap();
    input["envelope"]["run_id"] = json!(RUN);
    input["manifest"]["agent"]["id"] = json!("agent");
    let bytes = serde_json::to_vec(&input).unwrap();
    let file = root.join("phone-selection.json");
    std::fs::write(&file, &bytes).unwrap();
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let saved = owner
        .publish(&Publication {
            run_id: RUN,
            source_id: &source.source_id,
            checkout_id: &checkout.checkout_id,
            selection: &file,
            previous: None,
        })
        .unwrap();
    owner
        .admit(&PreparedAdmission {
            registration_id: &saved.id,
            revision: saved.revision,
            selection_sha256: &lower_hex(&Sha256::digest(&bytes)),
            generated_work_max: 3,
            park_ttl_ms: 3_600_000,
            token: "fixture-generate-token",
        })
        .unwrap();
    println!("SAVED_REVIEW_SOURCE_AND_RUN_BOUND");
}

fn payload(result: &str, revision: Option<&str>, cursor: Option<&Value>) -> Vec<u8> {
    let mut payload = vec![1, u8::try_from(result.len()).unwrap()];
    payload.extend(result.as_bytes());
    payload.extend(revision.map_or_else(|| vec![0; 32], unhex));
    let file = cursor.map_or(0, |value| value["file"].as_u64().unwrap());
    let side = cursor.map_or(0, |value| u8::from(value["side"] == "after"));
    let offset = cursor.map_or(0, |value| value["offset"].as_u64().unwrap());
    payload.extend(u32::try_from(file).unwrap().to_be_bytes());
    payload.push(side);
    payload.extend(u32::try_from(offset).unwrap().to_be_bytes());
    payload
}

async fn request(client: &reqwest::Client, url: &str, credential: &str, body: &Value) -> Value {
    client
        .post(format!("{url}/v1/phone-control/request"))
        .bearer_auth(credential)
        .json(body)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}

#[expect(
    clippy::too_many_lines,
    reason = "one installed transaction checks the same retained result across delivery fences"
)]
async fn review(root: &Path) {
    let result_id = std::env::var("LOUISELM_TEST_REVIEW_RESULT").expect("saved result identity");
    let runs = RunStore::new(root.join("runs")).unwrap();
    runs.park_cold(
        RunDraft {
            id: RUN.into(),
            session_id: "agent/fixture-acp".into(),
            agent: "agent".into(),
            acp_session_id: "fixture-acp".into(),
            working_dir: root.join("checkout").to_string_lossy().into_owned(),
            load_session: true,
            claimed_issue_ids: vec![],
        },
        1,
    )
    .unwrap();
    assert_eq!(
        runs.reap_expired(3_600_002, |_| Ok(())).unwrap().disposed,
        1
    );
    assert_eq!(runs.run(RUN).unwrap().state, "disposed");
    let attention = crate::AttentionStore::new(root.join("attention"), Some(runs.clone())).unwrap();
    let pending = serde_json::from_value(json!({"subject_kind":"run","subject_id":RUN,
        "kind":"promotion_pending","source_operation_id":"aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa",
        "created_at_ms":1,"linked_run_id":null,"stage":null,"result_id":result_id}))
    .unwrap();
    attention
        .project(&crate::BrokerProjection {
            sequence: 1,
            change: crate::ProjectionChange::Upsert { attention: pending },
        })
        .unwrap();
    let original_attention = attention.snapshot().unwrap();
    let tls = crate::TlsIdentity::load_or_create(root.join("tls")).unwrap();
    let pairing = Arc::new(PairingRegistry::open(root.join("pairing")).unwrap());
    let offer = pairing
        .issue(
            "https://localhost:7391",
            tls.public_key_sha256(),
            1_000,
            10_000,
        )
        .unwrap();
    let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
    let service = Service::open(
        &root.join("phone/grants"),
        tls.public_key_sha256(),
        pairing.clone(),
    )
    .unwrap()
    .with_runs(Some(runs));
    let app = service.router();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let scope = Scope {
        projects: vec![root.join("project").to_str().unwrap().into()],
        agents: vec!["agent".into()],
        groups: vec!["read".into()],
    };
    let (grant, period) = scoped_grant(&service, &app, &phone, &fixture, scope.clone()).await;
    let sources = PhoneSourceStore::open(root.join("phone/run-sources")).unwrap();
    let source = sources.read(RUN).unwrap();
    service
        .authority
        .link_project_roots(&source.project_id, grant.project_roots().unwrap().unwrap())
        .unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        tls.certificate_path(),
        tls.private_key_path(),
    )
    .await
    .unwrap();
    let server = tokio::spawn(
        axum_server::from_tcp_rustls(listener, config)
            .unwrap()
            .serve(app.clone().into_make_service()),
    );
    let cert =
        reqwest::Certificate::from_pem(&std::fs::read(tls.certificate_path()).unwrap()).unwrap();
    let client = reqwest::Client::builder()
        .add_root_certificate(cert)
        .build()
        .unwrap();
    let url = format!("https://localhost:{}", address.port());
    let subject = RUN.replace('-', "");
    let mut next = None;
    let mut revision: Option<String> = None;
    let mut text = String::new();
    let mut pages = 0;
    for operation in 1..=4 {
        let signed = signed_run(
            &fixture,
            &grant,
            &period,
            80,
            &subject,
            &payload(&result_id, revision.as_deref(), next.as_ref()),
            0,
            operation,
        );
        let page = request(&client, &url, &phone.credential, &signed).await;
        assert_eq!(page["type"], "saved_review", "{page}");
        assert_eq!(page["run_id"], RUN);
        assert_eq!(page["result_id"], result_id);
        assert_eq!(page["producer_agent"], "agent");
        assert_eq!(page["destination"], root.join("checkout").to_str().unwrap());
        assert_eq!(page["file"]["path"], "review-pages.txt");
        assert_eq!(page["file"]["side"], "after");
        assert_eq!(page["file"]["offset"], text.len());
        if let Some(expected) = &revision {
            assert_eq!(page["revision"], *expected);
        }
        let part = page["file"]["text"].as_str().unwrap();
        assert!(part.len() <= 4096);
        text.push_str(part);
        pages += 1;
        revision = Some(page["revision"].as_str().unwrap().to_owned());
        next = (!page["next"].is_null()).then(|| page["next"].clone());
        if next.is_none() {
            break;
        }
    }
    assert!(next.is_none());
    assert_eq!(pages, 3);
    assert_eq!(text, "review page ".repeat(800));
    assert!(text.len() > 4096);
    let wrong = signed_run(
        &fixture,
        &grant,
        &period,
        80,
        &subject,
        &payload(&result_id, Some(&"ab".repeat(32)), None),
        0,
        10,
    );
    assert_eq!(
        request(&client, &url, &phone.credential, &wrong).await["code"],
        "unavailable"
    );
    let wrong = signed_run(
        &fixture,
        &grant,
        &period,
        80,
        &subject,
        &payload("other-result", None, None),
        0,
        11,
    );
    assert_eq!(
        request(&client, &url, &phone.credential, &wrong).await["code"],
        "unavailable"
    );
    let checkout = root.join("checkout");
    let original = root.join("original-checkout");
    std::fs::rename(&checkout, &original).unwrap();
    let missing = signed_run(
        &fixture,
        &grant,
        &period,
        80,
        &subject,
        &payload(&result_id, None, None),
        0,
        12,
    );
    assert_eq!(
        request(&client, &url, &phone.credential, &missing).await["code"],
        "unavailable"
    );
    std::fs::create_dir(&checkout).unwrap();
    let replaced = signed_run(
        &fixture,
        &grant,
        &period,
        80,
        &subject,
        &payload(&result_id, None, None),
        0,
        13,
    );
    assert_eq!(
        request(&client, &url, &phone.credential, &replaced).await["code"],
        "unavailable"
    );
    std::fs::remove_dir(&checkout).unwrap();
    std::fs::rename(&original, &checkout).unwrap();
    let restored = signed_run(
        &fixture,
        &grant,
        &period,
        80,
        &subject,
        &payload(&result_id, None, None),
        0,
        16,
    );
    assert_eq!(
        request(&client, &url, &phone.credential, &restored).await["type"],
        "saved_review"
    );
    let offer = pairing
        .issue(
            "https://localhost:7391",
            tls.public_key_sha256(),
            2_000,
            10_000,
        )
        .unwrap();
    let other = pairing
        .consume(&offer.token, "second fixture", 2_001)
        .unwrap();
    let (other_grant, other_period) = scoped_grant(&service, &app, &other, &fixture, scope).await;
    service
        .authority
        .link_project_roots(
            &source.project_id,
            other_grant.project_roots().unwrap().unwrap(),
        )
        .unwrap();
    let before_revoke = signed_run(
        &fixture,
        &other_grant,
        &other_period,
        80,
        &subject,
        &payload(&result_id, None, None),
        0,
        17,
    );
    assert_eq!(
        request(&client, &url, &other.credential, &before_revoke).await["type"],
        "saved_review"
    );
    pairing.revoke(&other.device_id).unwrap();
    let revoked = signed_run(
        &fixture,
        &other_grant,
        &other_period,
        80,
        &subject,
        &payload(&result_id, None, None),
        0,
        14,
    );
    assert_eq!(
        request(&client, &url, &other.credential, &revoked).await["code"],
        "unauthenticated"
    );
    service.authority.disable(&phone.device_id).unwrap();
    let revoked = signed_run(
        &fixture,
        &grant,
        &period,
        80,
        &subject,
        &payload(&result_id, None, None),
        0,
        15,
    );
    assert_eq!(
        request(&client, &url, &phone.credential, &revoked).await["code"],
        "revoked"
    );
    assert_eq!(attention.snapshot().unwrap(), original_attention);
    server.abort();
    println!("SAVED_REVIEW_INSTALLED_HTTP_PASSED");
}
