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
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use super::super::{Service, authority::StoredGrant, fixed, prefix};
use super::tests::{history_grant, post, unhex};
use crate::{PairingRegistry, RunAdmission, RunProjectBinding, RunSession, hex::lower_hex};

async fn exchange_probe(
    app: axum::Router,
    credential: &str,
    stream: &mut UnixStream,
    request: &Value,
    response: &Value,
) -> Value {
    let credential = credential.to_owned();
    let body = request.to_string();
    let pending =
        tokio::spawn(
            async move { post(app, "/v1/phone-control/request", &credential, &body).await },
        );
    let probe = read_probe(stream).await;
    assert_eq!(probe["type"], "run_probe");
    assert_eq!(probe["controller_id"], response["controller_id"]);
    assert_eq!(probe["run_id"], response["run_id"]);
    write_probe(stream, response).await;
    if response["state"] == "found" {
        let confirmation = read_probe(stream).await;
        assert_eq!(confirmation["controller_id"], response["controller_id"]);
        assert_eq!(confirmation["run_id"], response["run_id"]);
        write_probe(stream, response).await;
    }
    pending.await.unwrap().1
}

async fn read_probe(stream: &mut UnixStream) -> Value {
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

async fn write_probe(stream: &mut UnixStream, response: &Value) {
    stream
        .write_all(format!("{response}\n").as_bytes())
        .await
        .unwrap();
}

#[expect(
    clippy::too_many_arguments,
    reason = "the signed fixture spells out each wire field"
)]
pub(super) fn signed_run(
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

#[expect(
    clippy::too_many_arguments,
    reason = "the signed fixture names both exact owner incarnations"
)]
pub(super) fn signed_run_session(
    fixture: &Value,
    grant: &StoredGrant,
    period: &str,
    controller: &str,
    subject: &str,
    run: &str,
    owner: &str,
    run_revision: u64,
    operation: u128,
) -> Value {
    let mut payload = vec![1];
    payload.extend(fixed::<16>(&run.replace('-', "")).unwrap());
    payload.extend(fixed::<16>(owner).unwrap());
    payload.extend(run_revision.to_be_bytes());
    payload.extend(0_u64.to_be_bytes());
    payload.extend(20_u16.to_be_bytes());
    let hash = lower_hex(&Sha256::digest(&payload));
    let operation = format!("{operation:032x}");
    let mut bytes = prefix(2);
    bytes.extend(fixed::<16>(&grant.grant_id).unwrap());
    bytes.extend(grant.grant_revision.to_be_bytes());
    bytes.extend(fixed::<16>(period).unwrap());
    bytes.extend(fixed::<32>(&grant.receiver_identity).unwrap());
    bytes.extend(fixed::<16>(controller).unwrap());
    bytes.extend(fixed::<16>(subject).unwrap());
    bytes.push(57);
    bytes.extend(fixed::<32>(&hash).unwrap());
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
        "receiver_identity":grant.receiver_identity,"controller_id":controller,"subject_id":subject,
        "action":57,"payload":lower_hex(&payload),"payload_hash":hash,"operation_id":operation,
        "expected_revision":0,"interaction":1,"signature":lower_hex(signature.as_ref())
    }})
}

fn session_page(request: &Value) -> Value {
    json!({"v":2,"type":"session_page","controller_id":request["controller_id"],
        "subject_id":request["subject_id"],"operation_id":request["operation_id"],
        "revision":1,"status":"ready","entries":[{"kind":"message","seq":1,
        "role":"assistant","text":"owned Session","truncated":false}],"next_before":0,
        "complete":true,"source":{"replay":"not_applicable","archive":"unavailable",
        "redaction":"unknown","missing":false,"truncated":false}})
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
        generated_work_max: 3,
        park_ttl_ms: 3_600_000,
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
        .set_test_integration(|| Err(super::super::source::PhoneSourceError::Unsupported));
    let unsupported_app = service.router();
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
    let unsupported = signed_run(&fixture, &grant, &period, 56, &subject, &[], 0, 921);
    let (_, unsupported) = post(
        unsupported_app,
        "/v1/phone-control/request",
        &phone.credential,
        &unsupported.to_string(),
    )
    .await;
    assert_eq!(unsupported["code"], "unsupported");
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
    let prepared_list = signed_run(
        &fixture,
        &grant,
        &period,
        52,
        &"00".repeat(16),
        &[0; 16],
        0,
        911,
    );
    let (_, prepared_list) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &prepared_list.to_string(),
    )
    .await;
    assert_eq!(prepared_list["items"], json!([]));
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
    let live = signed_run(&fixture, &grant, &period, 56, &subject, &[], 1, 912);
    let (_, live) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &live.to_string(),
    )
    .await;
    assert_eq!(live["type"], "run_live");
    assert_eq!(live["controller"], "absent");
    assert_eq!(live["run_id"], run);
    assert_eq!(live["durable_revision"], 1);
    let socket = root.path().join("live-controller.sock");
    let capability = root.path().join("live-capability");
    let server = super::super::socket::ControllerSocket::bind(
        &socket,
        &capability,
        service.controllers.clone(),
    )
    .await
    .unwrap();
    let token = std::fs::read_to_string(&capability).unwrap();
    let server_task = tokio::spawn(server.serve());
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    stream
        .write_all(format!("{}\n", json!({"v":1,"type":"attach","capability":token})).as_bytes())
        .await
        .unwrap();
    let mut attached = String::new();
    BufReader::new(&mut stream)
        .read_line(&mut attached)
        .await
        .unwrap();
    let mut controller = serde_json::from_str::<Value>(&attached).unwrap()["controller_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let subject_id = "44".repeat(16);
    let mut observed = json!({"v":1,"type":"run_probe","controller_id":controller,"run_id":run,
        "state":"found","run":{"run_id":run,"owner_id":"66".repeat(16),"registration_id":saved.id,
        "registration_revision":saved.revision,"admitted":true,"status":"active","phase":"working",
        "bead_index":1,"bead_total":3,"bead_id":"b-1","sessions":[{"subject_id":subject_id,
        "agent":"codex","status":"ready"}],"sessions_truncated":false}});
    let request = signed_run(&fixture, &grant, &period, 56, &subject, &[], 1, 913);
    let live = exchange_probe(
        app.clone(),
        &phone.credential,
        &mut stream,
        &request,
        &observed,
    )
    .await;
    assert_eq!(live["controller"], "connected");
    assert_eq!(live["live"]["controller_id"], controller);
    assert_eq!(live["live"]["sessions"][0]["subject_id"], subject_id);
    assert_eq!(live["live"]["phase"], "working");
    assert!(!live.to_string().contains(&saved.id));
    let owner_id = "66".repeat(16);
    let request = signed_run_session(
        &fixture,
        &grant,
        &period,
        &controller,
        &subject_id,
        run,
        &owner_id,
        1,
        924,
    );
    let linked = tokio::spawn({
        let app = app.clone();
        let credential = phone.credential.clone();
        async move {
            post(
                app,
                "/v1/phone-control/request",
                &credential,
                &request.to_string(),
            )
            .await
        }
    });
    let frame = tokio::time::timeout(std::time::Duration::from_secs(1), read_probe(&mut stream))
        .await
        .expect("linked Session read must reach the exact controller");
    assert_eq!(frame["type"], "read_run_session");
    write_probe(&mut stream, &session_page(&frame["request"])).await;
    assert_eq!(
        linked.await.unwrap().1["entries"][0]["text"],
        "owned Session"
    );
    let request = signed_run(&fixture, &grant, &period, 56, &subject, &[], 1, 923);
    let replacing = tokio::spawn({
        let app = app.clone();
        let credential = phone.credential.clone();
        async move {
            post(
                app,
                "/v1/phone-control/request",
                &credential,
                &request.to_string(),
            )
            .await
        }
    });
    assert_eq!(read_probe(&mut stream).await["run_id"], run);
    write_probe(&mut stream, &observed).await;
    assert_eq!(read_probe(&mut stream).await["run_id"], run);
    let mut replacement = observed.clone();
    replacement["run"]["owner_id"] = json!("77".repeat(16));
    write_probe(&mut stream, &replacement).await;
    assert_eq!(replacing.await.unwrap().1["controller"], "unreachable");
    let mut other = UnixStream::connect(&socket).await.unwrap();
    other
        .write_all(format!("{}\n", json!({"v":1,"type":"attach","capability":token})).as_bytes())
        .await
        .unwrap();
    let mut attached = String::new();
    BufReader::new(&mut other)
        .read_line(&mut attached)
        .await
        .unwrap();
    let other_id = serde_json::from_str::<Value>(&attached).unwrap()["controller_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let request = signed_run(&fixture, &grant, &period, 56, &subject, &[], 1, 920);
    let ambiguous = tokio::spawn({
        let app = app.clone();
        let credential = phone.credential.clone();
        async move {
            post(
                app,
                "/v1/phone-control/request",
                &credential,
                &request.to_string(),
            )
            .await
        }
    });
    assert_eq!(read_probe(&mut stream).await["controller_id"], controller);
    assert_eq!(read_probe(&mut other).await["controller_id"], other_id);
    write_probe(&mut stream, &observed).await;
    let mut other_observed = observed.clone();
    other_observed["controller_id"] = json!(other_id);
    write_probe(&mut other, &other_observed).await;
    assert_eq!(ambiguous.await.unwrap().1["controller"], "unreachable");
    drop(other);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while service.controllers.contains(&other_id) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    observed["run"]["registration_revision"] = json!(saved.revision + 1);
    let request = signed_run(&fixture, &grant, &period, 56, &subject, &[], 1, 914);
    let wrong = exchange_probe(
        app.clone(),
        &phone.credential,
        &mut stream,
        &request,
        &observed,
    )
    .await;
    assert_eq!(wrong["controller"], "unreachable");
    assert_eq!(wrong["live"], Value::Null);
    observed["run"]["registration_revision"] = json!(saved.revision);
    observed["run"]["sessions"][0]["agent"] = json!("foreign-agent");
    let request = signed_run(&fixture, &grant, &period, 56, &subject, &[], 1, 915);
    let wrong = exchange_probe(
        app.clone(),
        &phone.credential,
        &mut stream,
        &request,
        &observed,
    )
    .await;
    assert_eq!(wrong["controller"], "unreachable");
    observed["run"]["sessions"][0]["agent"] = json!("codex");
    let request = signed_run(&fixture, &grant, &period, 56, &subject, &[], 1, 919);
    let request: super::SignedRequest = serde_json::from_value(request).unwrap();
    let permit = service
        .authority
        .begin_read(&phone.device_id, &request.request, Instant::now())
        .unwrap();
    let registry = service.controllers.clone();
    let probing =
        tokio::spawn(async move { super::super::run_probe::collect(registry, run).await });
    assert_eq!(read_probe(&mut stream).await["controller_id"], controller);
    write_probe(&mut stream, &observed).await;
    assert_eq!(read_probe(&mut stream).await["controller_id"], controller);
    write_probe(&mut stream, &observed).await;
    let probe = probing.await.unwrap();
    drop(stream);
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while service.controllers.contains(&controller) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        service.authority.finish_live_owner_delivery(
            &permit,
            &pairing,
            &phone.credential,
            Some(&reader),
            &probe,
            &service.controllers,
        ),
        Err(super::Refusal::StaleSubject)
    );
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    stream
        .write_all(format!("{}\n", json!({"v":1,"type":"attach","capability":token})).as_bytes())
        .await
        .unwrap();
    let mut attached = String::new();
    BufReader::new(&mut stream)
        .read_line(&mut attached)
        .await
        .unwrap();
    let next_controller = serde_json::from_str::<Value>(&attached).unwrap()["controller_id"]
        .as_str()
        .unwrap()
        .to_owned();
    assert_ne!(controller, next_controller);
    controller = next_controller;
    observed["controller_id"] = json!(controller);
    observed["run"]["owner_id"] = json!("88".repeat(16));
    observed["run"]["sessions"][0]["subject_id"] = json!("55".repeat(16));
    let request = signed_run(&fixture, &grant, &period, 56, &subject, &[], 1, 916);
    let pending_live = tokio::spawn({
        let app = app.clone();
        let credential = phone.credential.clone();
        async move {
            post(
                app,
                "/v1/phone-control/request",
                &credential,
                &request.to_string(),
            )
            .await
        }
    });
    assert_eq!(read_probe(&mut stream).await["run_id"], run);
    runs.attach(RunSession {
        id: run.into(),
        session_id: "codex/acp".into(),
        agent: "codex".into(),
        acp_session_id: "acp".into(),
        working_dir: "/private/worker".into(),
        load_session: false,
    })
    .unwrap();
    write_probe(&mut stream, &observed).await;
    assert_eq!(read_probe(&mut stream).await["run_id"], run);
    write_probe(&mut stream, &observed).await;
    assert_eq!(pending_live.await.unwrap().1["code"], "stale_subject");
    let new_subject = "55".repeat(16);
    let new_owner = "88".repeat(16);
    let stale_link = signed_run_session(
        &fixture,
        &grant,
        &period,
        &controller,
        &new_subject,
        run,
        &new_owner,
        1,
        925,
    );
    let (_, stale_link) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        post(
            app.clone(),
            "/v1/phone-control/request",
            &phone.credential,
            &stale_link.to_string(),
        ),
    )
    .await
    .expect("stale Run must be refused before reading Session history");
    assert_eq!(stale_link["code"], "stale_subject");
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
    let no_read_live = signed_run(
        &fixture,
        &no_read,
        &no_read_period,
        56,
        &subject,
        &[],
        0,
        922,
    );
    let (_, no_read_live) = post(
        app.clone(),
        "/v1/phone-control/request",
        &no_read_phone.credential,
        &no_read_live.to_string(),
    )
    .await;
    assert_eq!(no_read_live["code"], "out_of_scope");
    let no_read_link = signed_run_session(
        &fixture,
        &no_read,
        &no_read_period,
        &controller,
        &new_subject,
        run,
        &new_owner,
        2,
        926,
    );
    let (_, no_read_link) = post(
        app.clone(),
        "/v1/phone-control/request",
        &no_read_phone.credential,
        &no_read_link.to_string(),
    )
    .await;
    assert_eq!(no_read_link["code"], "out_of_scope");
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
    let pending_link = signed_run_session(
        &fixture,
        &grant,
        &period,
        &controller,
        &new_subject,
        run,
        &new_owner,
        2,
        927,
    );
    let pending_link: super::SignedRequest = serde_json::from_value(pending_link).unwrap();
    let pending_link = service
        .authority
        .begin_read(&phone.device_id, &pending_link.request, Instant::now())
        .unwrap();
    let request = signed_run(&fixture, &grant, &period, 56, &subject, &[], 0, 917);
    let pending_live = tokio::spawn({
        let app = app.clone();
        let credential = phone.credential.clone();
        async move {
            post(
                app,
                "/v1/phone-control/request",
                &credential,
                &request.to_string(),
            )
            .await
        }
    });
    assert_eq!(read_probe(&mut stream).await["run_id"], run);
    runs.add_phone_contributor(run, "claude").unwrap();
    assert_eq!(
        service.authority.finish_run_session_delivery(
            &pending_link,
            &pairing,
            &phone.credential,
            Some(&reader),
            &service.controllers,
            true,
        ),
        Err(super::Refusal::StaleSubject),
    );
    let grown = signed_run_session(
        &fixture,
        &grant,
        &period,
        &controller,
        &new_subject,
        run,
        &new_owner,
        runs.run(run).unwrap().revision,
        929,
    );
    let grown: super::SignedRequest = serde_json::from_value(grown).unwrap();
    let grown = service
        .authority
        .begin_read(&phone.device_id, &grown.request, Instant::now())
        .unwrap();
    assert_eq!(
        service.authority.finish_run_session_delivery(
            &grown,
            &pairing,
            &phone.credential,
            Some(&reader),
            &service.controllers,
            true,
        ),
        Err(super::Refusal::OutOfScope)
    );
    let foreign = signed_run_session(
        &fixture,
        &grant,
        &period,
        &controller,
        &new_subject,
        run,
        &new_owner,
        runs.run(run).unwrap().revision,
        930,
    );
    let (_, foreign) = tokio::time::timeout(
        std::time::Duration::from_secs(1),
        post(
            app.clone(),
            "/v1/phone-control/request",
            &phone.credential,
            &foreign.to_string(),
        ),
    )
    .await
    .expect("foreign Run must be refused before reading Session history");
    assert_eq!(foreign["code"], "out_of_scope");
    write_probe(&mut stream, &observed).await;
    assert_eq!(read_probe(&mut stream).await["run_id"], run);
    write_probe(&mut stream, &observed).await;
    assert_eq!(pending_live.await.unwrap().1["code"], "out_of_scope");
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
    let pending_link = signed_run_session(
        &fixture,
        &grant,
        &period,
        &controller,
        &new_subject,
        run,
        &new_owner,
        runs.run(run).unwrap().revision,
        928,
    );
    let pending_link: super::SignedRequest = serde_json::from_value(pending_link).unwrap();
    let pending_link = service
        .authority
        .begin_read(&phone.device_id, &pending_link.request, Instant::now())
        .unwrap();
    let request = signed_run(&fixture, &grant, &period, 56, &subject, &[], 0, 918);
    let pending_live = tokio::spawn({
        let app = app.clone();
        let credential = phone.credential.clone();
        async move {
            post(
                app,
                "/v1/phone-control/request",
                &credential,
                &request.to_string(),
            )
            .await
        }
    });
    assert_eq!(read_probe(&mut stream).await["run_id"], run);
    service.authority.disable(&phone.device_id).unwrap();
    assert_eq!(
        service.authority.finish_run_session_delivery(
            &pending_link,
            &pairing,
            &phone.credential,
            Some(&reader),
            &service.controllers,
            true,
        ),
        Err(super::Refusal::Revoked),
    );
    write_probe(&mut stream, &observed).await;
    assert_eq!(read_probe(&mut stream).await["run_id"], run);
    write_probe(&mut stream, &observed).await;
    assert_eq!(pending_live.await.unwrap().1["code"], "revoked");
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
    server_task.abort();
}
