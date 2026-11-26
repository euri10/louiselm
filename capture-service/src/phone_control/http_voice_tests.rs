//! Protected reads of synthetic, durably archived voice.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "isolated fixtures and assertions"
)]

use std::{io::Cursor, sync::Arc, time::Instant};

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    run_tests::signed_run,
    tests::{history_grant, history_period, post, scoped_grant, unhex},
};
use crate::{
    CaptureDraft, CaptureSource, PairingRegistry, Transcriber, TranscriptRequest,
    TranscriptionError, VoiceArchive, VoiceAssociation, VoiceTranscriptionWorker,
    hex::lower_hex,
    phone_control::{
        Request, Service,
        authority::{Refusal, Scope, StoredGrant},
        fixed, prefix,
    },
};

struct SyntheticVoice;
impl Transcriber for SyntheticVoice {
    fn name(&self) -> &'static str {
        "fixture"
    }
    fn model(&self) -> &'static str {
        "fixture"
    }
    fn transcribe(&self, _: TranscriptRequest<'_>) -> Result<String, TranscriptionError> {
        Ok("synthetic archived transcript".into())
    }
}

#[tokio::test]
async fn voice_history_page_fits_the_phone_response_bound() {
    let root = tempfile::tempdir().unwrap();
    let receiver = "01".repeat(32);
    let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
    let offer = pairing
        .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
    let voice = root.path().join("voice");
    let service = Service::open(&root.path().join("phone/grants"), &receiver, pairing)
        .unwrap()
        .with_voice_root(voice.clone());
    let app = service.router();
    let archive = VoiceArchive::new(voice).unwrap();
    let project = std::fs::canonicalize(root.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let unusual_project = format!("{project}/{}", "\u{1}".repeat(510 - project.len()));
    let agent = "\u{1}".repeat(512);
    let association = VoiceAssociation {
        receiver,
        controller: "11".repeat(16),
        subject: "22".repeat(16),
        project: unusual_project,
        agent: agent.clone(),
        draft: "33".repeat(32),
        revision: "44".repeat(32),
    };
    for number in 1..=50_u128 {
        admit(
            &archive,
            &uuid::Uuid::from_u128(number).to_string(),
            &association,
        );
    }
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let (grant, period) = scoped_grant(
        &service,
        &app,
        &phone,
        &fixture,
        Scope {
            projects: vec![project],
            agents: vec![agent],
            groups: vec!["read".into()],
        },
    )
    .await;
    let mut cursor = [0_u8; 18];
    cursor[17] = 50;
    let request = signed_run(
        &fixture,
        &grant,
        &period,
        81,
        &"00".repeat(16),
        &cursor,
        0,
        2000,
    );
    let (_, response) = post(
        app,
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(response["type"], "voice_history");
    assert!(serde_json::to_vec(&response).unwrap().len() <= 262_144);
    assert_eq!(response["records"].as_array().unwrap().len(), 20);
    assert_ne!(response["next_after"], "");
}

fn admit(archive: &VoiceArchive, id: &str, association: &VoiceAssociation) {
    archive
        .admit(
            &CaptureDraft {
                id: id.into(),
                source: CaptureSource::Android,
                recorded_at_ms: 10,
                duration_ms: 20,
                mime_type: "audio/mp4".into(),
            },
            association,
            Cursor::new(b"synthetic audio"),
        )
        .unwrap();
}

fn signed_result(
    fixture: &Value,
    grant: &StoredGrant,
    period: &str,
    association: &VoiceAssociation,
    id: &str,
    operation: u128,
) -> Value {
    let mut payload = uuid::Uuid::parse_str(id).unwrap().as_bytes().to_vec();
    payload.extend(0_u32.to_be_bytes());
    let hash = lower_hex(&Sha256::digest(&payload));
    let operation = format!("{operation:032x}");
    let mut bytes = prefix(2);
    bytes.extend(fixed::<16>(&grant.grant_id).unwrap());
    bytes.extend(grant.grant_revision.to_be_bytes());
    bytes.extend(fixed::<16>(period).unwrap());
    bytes.extend(fixed::<32>(&grant.receiver_identity).unwrap());
    bytes.extend(fixed::<16>(&association.controller).unwrap());
    bytes.extend(fixed::<16>(&association.subject).unwrap());
    bytes.push(82);
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
        "receiver_identity":grant.receiver_identity,"controller_id":association.controller,
        "subject_id":association.subject,"action":82,"payload":lower_hex(&payload),"payload_hash":hash,
        "operation_id":operation,"expected_revision":0,"interaction":1,"signature":lower_hex(signature.as_ref())
    }})
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "the same archive and grant prove pagination, restart and revocation in order"
)]
async fn voice_history_filters_before_delivery_and_survives_source_end() {
    let root = tempfile::tempdir().unwrap();
    let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
    let receiver = "01".repeat(32);
    let offer = pairing
        .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
    let service = Service::open(
        &root.path().join("phone/grants"),
        &receiver,
        pairing.clone(),
    )
    .unwrap()
    .with_voice_root(root.path().join("voice"));
    let app = service.router();
    let archive = VoiceArchive::new(root.path().join("voice")).unwrap();
    let project = std::fs::canonicalize(root.path())
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let association = VoiceAssociation {
        receiver: receiver.clone(),
        controller: "11".repeat(16),
        subject: "22".repeat(16),
        project,
        agent: "codex".into(),
        draft: "33".repeat(32),
        revision: "44".repeat(32),
    };
    let id = "11111111-1111-4111-8111-111111111111";
    let second = "44444444-4444-4444-8444-444444444444";
    admit(&archive, id, &association);
    admit(&archive, second, &association);
    let mut wrong_agent = association.clone();
    wrong_agent.agent = "other".into();
    admit(
        &archive,
        "22222222-2222-4222-8222-222222222222",
        &wrong_agent,
    );
    let mut wrong_receiver = association.clone();
    wrong_receiver.receiver = "ff".repeat(32);
    admit(
        &archive,
        "33333333-3333-4333-8333-333333333333",
        &wrong_receiver,
    );
    VoiceTranscriptionWorker::new(&archive, &SyntheticVoice)
        .process_ready(1_000)
        .unwrap();
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
    let mut payload = [0_u8; 18];
    payload[17] = 1;
    let request = signed_run(
        &fixture,
        &grant,
        &period,
        81,
        &"00".repeat(16),
        &payload,
        0,
        1001,
    );
    let (_, response) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(response["type"], "voice_history");
    assert_eq!(response["records"][0]["id"], id);
    assert_eq!(response["records"][0]["controller"], association.controller);
    assert_eq!(response["records"][0]["subject"], association.subject);
    assert_eq!(response["next_after"], id);
    payload[0] = 1;
    payload[1..17].copy_from_slice(uuid::Uuid::parse_str(id).unwrap().as_bytes());
    let next = signed_run(
        &fixture,
        &grant,
        &period,
        81,
        &"00".repeat(16),
        &payload,
        0,
        1002,
    );
    let (_, page) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &next.to_string(),
    )
    .await;
    assert_eq!(page["records"].as_array().unwrap().len(), 1);
    assert_eq!(page["records"][0]["id"], second);
    assert_eq!(page["next_after"], "");

    let result = signed_result(&fixture, &grant, &period, &association, id, 1003);
    let (_, response) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &result.to_string(),
    )
    .await;
    assert_eq!(response["text"], "synthetic archived transcript");
    assert_eq!(response["revision"], association.revision);
    let mut wrong_subject = association.clone();
    wrong_subject.subject = "aa".repeat(16);
    let wrong = signed_result(&fixture, &grant, &period, &wrong_subject, id, 1004);
    let (_, response) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &wrong.to_string(),
    )
    .await;
    assert_eq!(response["code"], "out_of_scope");
    let wrong = signed_result(
        &fixture,
        &grant,
        &period,
        &wrong_agent,
        "22222222-2222-4222-8222-222222222222",
        1005,
    );
    let (_, response) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &wrong.to_string(),
    )
    .await;
    assert_eq!(response["code"], "out_of_scope");

    // A receiver restart loses the working period but retains the archive and grant.
    drop(app);
    drop(service);
    let service = Service::open(
        &root.path().join("phone/grants"),
        &receiver,
        pairing.clone(),
    )
    .unwrap()
    .with_voice_root(root.path().join("voice"));
    let app = service.router();
    let restarted = history_period(&app, &phone, &fixture, &grant).await;
    let request = signed_result(&fixture, &grant, &restarted, &association, id, 1006);
    let (_, response) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(response["text"], "synthetic archived transcript");

    // A valid all-zero UUID must not be confused with the start-of-history cursor.
    let nil = "00000000-0000-0000-0000-000000000000";
    admit(&archive, nil, &association);
    let mut first = [0_u8; 18];
    first[17] = 1;
    let request = signed_run(
        &fixture,
        &grant,
        &restarted,
        81,
        &"00".repeat(16),
        &first,
        0,
        1008,
    );
    let (_, page) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(page["next_after"], nil);
    first[0] = 1;
    let request = signed_run(
        &fixture,
        &grant,
        &restarted,
        81,
        &"00".repeat(16),
        &first,
        0,
        1009,
    );
    let (_, page) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(page["records"][0]["id"], id);

    // Revocation between admission and delivery prevents the retained result from escaping.
    let request = signed_result(&fixture, &grant, &restarted, &association, id, 1007);
    let request: Request = serde_json::from_value(request["request"].clone()).unwrap();
    assert!(matches!(
        service.authority.begin_read(
            &phone.device_id,
            &request,
            Instant::now() + std::time::Duration::from_secs(301)
        ),
        Err(Refusal::Expired)
    ));
    let permit = service
        .authority
        .begin_read(&phone.device_id, &request, Instant::now())
        .unwrap();
    service.authority.disable(&phone.device_id).unwrap();
    assert!(matches!(
        service
            .authority
            .finish_voice_delivery(&permit, &pairing, &phone.credential, &archive),
        Err(Refusal::Revoked)
    ));
    assert_eq!(
        archive.transcript(id).unwrap().text,
        "synthetic archived transcript"
    );

    let (no_read, no_read_period) = history_grant(
        &service,
        &app,
        &phone,
        &fixture,
        root.path(),
        vec!["operate_sessions".into()],
    )
    .await;
    let request = signed_run(
        &fixture,
        &no_read,
        &no_read_period,
        81,
        &"00".repeat(16),
        &first,
        0,
        1010,
    );
    let (_, response) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(response["code"], "out_of_scope");
    let outside = tempfile::tempdir().unwrap();
    let (other_scope, other_period) = history_grant(
        &service,
        &app,
        &phone,
        &fixture,
        outside.path(),
        vec!["read".into()],
    )
    .await;
    let request = signed_result(
        &fixture,
        &other_scope,
        &other_period,
        &association,
        id,
        1011,
    );
    let (_, response) = post(
        app,
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(response["code"], "out_of_scope");
}

#[tokio::test]
#[ignore = "requires the pinned capture binary, headless Neovim and isolated Android fixture"]
#[expect(
    clippy::too_many_lines,
    reason = "one isolated fixture connects the real receiver, Session and Android review"
)]
async fn pinned_receiver_and_android_review_archived_voice() {
    use std::{
        io::{BufRead, BufReader},
        process::{Command, Stdio},
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            // Only this fixture's mock controller is stopped on failure.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    let nvim = std::env::var_os("LOUISELM_TEST_NVIM").unwrap();
    let binary = std::env::var_os("LOUISELM_TEST_CAPTURE").unwrap();
    let project = std::fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).unwrap();
    let root = tempfile::tempdir().unwrap();
    let tls = crate::TlsIdentity::load_or_create(root.path().join("louiselm/capture/tls")).unwrap();
    let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
    let offer = pairing
        .issue(
            "https://127.0.0.1:7391",
            tls.public_key_sha256(),
            1_000,
            10_000,
        )
        .unwrap();
    let phone = pairing
        .consume(&offer.token, "fixture phone", 1_001)
        .unwrap();
    let voice_root = root.path().join("voice");
    let service = Service::open(
        &root.path().join("phone/grants"),
        tls.public_key_sha256(),
        pairing.clone(),
    )
    .unwrap()
    .with_voice_root(voice_root.clone());
    let workflow = root.path().join("louiselm/workflow");
    let sockets = service
        .bind(
            &workflow.join("admin.sock"),
            &workflow.join("phone-control.sock"),
            &workflow.join("operator-capability"),
        )
        .await
        .unwrap();
    let controller_task = tokio::spawn(sockets.serve());
    let ordinary = crate::Store::new(root.path().join("captures")).unwrap();
    let ordinary_id = "33333333-3333-4333-8333-333333333333";
    ordinary
        .ingest(
            &CaptureDraft {
                id: ordinary_id.into(),
                source: CaptureSource::Android,
                recorded_at_ms: 10,
                duration_ms: 20,
                mime_type: "audio/mp4".into(),
            },
            Cursor::new(b"ordinary synthetic audio"),
        )
        .unwrap();
    let app = crate::Receiver::new(
        ordinary.clone(),
        pairing,
        root.path().join("uploads"),
        tls.public_key_sha256(),
    )
    .unwrap()
    .with_phone_control(service.router())
    .router();
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let (grant, _) = history_grant(
        &service,
        &app,
        &phone,
        &fixture,
        &project,
        vec!["read".into()],
    )
    .await;
    let mut child = ChildGuard(
        Command::new(nvim)
            .args(["--headless", "--noplugin", "-u", "NONE", "-l"])
            .arg(project.join("tests/fixtures/phone_control_headless.lua"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.path())
            .env("LANG", "C.UTF-8")
            .env("LOUISELM_CAPTURE_STATE_DIR", root.path())
            .env("LOUISELM_TEST_PROJECT", &project)
            .env("LOUISELM_TEST_USAGE", root.path().join("usage"))
            .env("LOUISELM_TEST_CAPTURE", binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let ready = tokio::task::spawn_blocking(move || {
        let mut line = String::new();
        BufReader::new(stdout).read_line(&mut line).unwrap();
        serde_json::from_str::<Value>(&line).unwrap()
    });
    let advertised = tokio::time::timeout(std::time::Duration::from_secs(20), ready)
        .await
        .unwrap()
        .unwrap();
    let controller = advertised["controller_id"].as_str().unwrap().to_owned();
    let subject = advertised["subject_id"].as_str().unwrap().to_owned();
    let other = advertised["other_subject_id"].as_str().unwrap().to_owned();

    let seeder = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let seed_port = seeder.local_addr().unwrap().port();
    let association = VoiceAssociation {
        receiver: tls.public_key_sha256().into(),
        controller: controller.clone(),
        subject: subject.clone(),
        project: project.to_str().unwrap().into(),
        agent: "codex".into(),
        draft: String::new(),
        revision: String::new(),
    };
    let seed_task = tokio::spawn(async move {
        let (mut socket, _) = seeder.accept().await.unwrap();
        let mut line = String::new();
        tokio::io::BufReader::new(&mut socket)
            .read_line(&mut line)
            .await
            .unwrap();
        let seed: Value = serde_json::from_str(&line).unwrap();
        let mut association = association;
        association.draft = seed["draft"].as_str().unwrap().into();
        association.revision = seed["revision"].as_str().unwrap().into();
        tokio::task::spawn_blocking(move || {
            let archive = VoiceArchive::new(voice_root).unwrap();
            admit(
                &archive,
                "11111111-1111-4111-8111-111111111111",
                &association,
            );
            admit(
                &archive,
                "22222222-2222-4222-8222-222222222222",
                &association,
            );
            VoiceTranscriptionWorker::new(&archive, &SyntheticVoice)
                .process_ready(1_000)
                .unwrap();
        })
        .await
        .unwrap();
        socket.write_all(b"ready\n").await.unwrap();
    });

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let config = axum_server::tls_rustls::RustlsConfig::from_pem_file(
        tls.certificate_path(),
        tls.private_key_path(),
    )
    .await
    .unwrap();
    let https = tokio::spawn(
        axum_server::from_tcp_rustls(listener, config)
            .unwrap()
            .serve(app.into_make_service()),
    );
    let android = project.join("android");
    let status = tokio::task::spawn_blocking(move || {
        let status = Command::new(android.join("gradlew")).current_dir(&android)
            .args(["--no-daemon", "--max-workers=1", "-PlouiselmFirebase=false", "--offline", "testDebugUnitTest",
                "--tests", "dev.louiselm.capture.PhoneVoiceArchiveEndToEndTest"])
            .env("LOUISELM_E2E_URL", format!("https://localhost:{}", address.port()))
            .env("LOUISELM_E2E_RECEIVER", tls.public_key_sha256()).env("LOUISELM_E2E_CREDENTIAL", phone.credential)
            .env("LOUISELM_E2E_DEVICE", phone.device_id).env("LOUISELM_E2E_GRANT", grant.grant_id)
            .env("LOUISELM_E2E_CONTROLLER", controller).env("LOUISELM_E2E_SUBJECT", subject)
            .env("LOUISELM_E2E_OTHER_SUBJECT", other).env("LOUISELM_E2E_SEED_PORT", seed_port.to_string())
            .status().unwrap();
        let report = std::fs::read_to_string(android.join("app/build/test-results/testDebugUnitTest/TEST-dev.louiselm.capture.PhoneVoiceArchiveEndToEndTest.xml")).unwrap();
        assert!(report.contains("tests=\"1\"") && report.contains("skipped=\"0\"") &&
            report.contains("failures=\"0\"") && report.contains("errors=\"0\""));
        status
    }).await.unwrap();
    assert!(status.success());
    seed_task.await.unwrap();
    assert_eq!(ordinary.list().unwrap().len(), 1);
    assert_eq!(
        std::fs::read(ordinary.capture(ordinary_id).unwrap().audio_path).unwrap(),
        b"ordinary synthetic audio"
    );
    https.abort();
    controller_task.abort();
}
