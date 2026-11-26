//! Signed voice-job operations against isolated grants and archives.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Fixtures abort on setup failure and assert exact outcomes."
)]

use std::{io::Cursor, sync::Arc};

use aws_lc_rs::{
    rand::SystemRandom,
    signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::tests::{history_grant, post, unhex};
use crate::{
    CaptureDraft, CaptureSource, PairingRegistry, Transcriber, TranscriptRequest,
    TranscriptionError, VoiceArchive, VoiceAssociation, VoiceStatus, VoiceTranscriptionWorker,
    hex::lower_hex,
    phone_control::{Service, authority::StoredGrant, fixed, prefix},
};

fn signed_job(
    fixture: &Value,
    grant: &StoredGrant,
    period: &str,
    association: &VoiceAssociation,
    action: u8,
    id: &str,
    operation: u128,
) -> Value {
    let mut payload = uuid::Uuid::parse_str(id).unwrap().as_bytes().to_vec();
    if action == 85 {
        payload.extend([7; 16]);
    }
    signed_payload(
        fixture,
        grant,
        period,
        association,
        action,
        &payload,
        operation,
    )
}

fn signed_payload(
    fixture: &Value,
    grant: &StoredGrant,
    period: &str,
    association: &VoiceAssociation,
    action: u8,
    payload: &[u8],
    operation: u128,
) -> Value {
    let hash = lower_hex(&Sha256::digest(payload));
    let operation = format!("{operation:032x}");
    let mut bytes = prefix(2);
    bytes.extend(fixed::<16>(&grant.grant_id).unwrap());
    bytes.extend(grant.grant_revision.to_be_bytes());
    bytes.extend(fixed::<16>(period).unwrap());
    bytes.extend(fixed::<32>(&grant.receiver_identity).unwrap());
    bytes.extend(fixed::<16>(&association.controller).unwrap());
    bytes.extend(fixed::<16>(&association.subject).unwrap());
    bytes.push(action);
    bytes.extend(fixed::<32>(&hash).unwrap());
    bytes.extend(fixed::<16>(&operation).unwrap());
    bytes.extend(0_u64.to_be_bytes());
    bytes.push(0);
    let key = EcdsaKeyPair::from_pkcs8(
        &ECDSA_P256_SHA256_ASN1_SIGNING,
        &unhex(fixture["keys"]["working"]["pkcs8"].as_str().unwrap()),
    )
    .unwrap();
    let signature = key.sign(&SystemRandom::new(), &bytes).unwrap();
    json!({"v":1,"type":"request","request":{
        "grant_id":grant.grant_id,"grant_revision":grant.grant_revision,"period_id":period,
        "receiver_identity":grant.receiver_identity,"controller_id":association.controller,
        "subject_id":association.subject,"action":action,"payload":lower_hex(payload),
        "payload_hash":hash,"operation_id":operation,"expected_revision":0,"interaction":0,
        "signature":lower_hex(signature.as_ref())}})
}

fn admission_payload(id: &str, audio: &[u8], association: &VoiceAssociation) -> Vec<u8> {
    let mut payload = vec![1];
    payload.extend(uuid::Uuid::parse_str(id).unwrap().as_bytes());
    payload.extend(Sha256::digest(audio));
    payload.extend(100_u64.to_be_bytes());
    payload.extend(50_u64.to_be_bytes());
    payload.extend((audio.len() as u64).to_be_bytes());
    payload.extend(fixed::<32>(&association.draft).unwrap());
    payload.extend(fixed::<32>(&association.revision).unwrap());
    payload.extend(
        fixed::<32>(
            &crate::phone_control::voice_job::voice_source_digest(
                &association.project,
                &association.agent,
            )
            .unwrap(),
        )
        .unwrap(),
    );
    payload
}

#[tokio::test]
async fn admission_refuses_disabled_wrong_group_and_stale_controller() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;
    for (enabled, group, expected) in [
        (false, "operate_sessions", "unsupported"),
        (true, "read", "out_of_scope"),
        (true, "operate_sessions", "stale_subject"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let receiver = "01".repeat(32);
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let offer = pairing
            .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
            .unwrap();
        let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
        let service = Service::open(&root.path().join("phone/grants"), &receiver, pairing)
            .unwrap()
            .with_voice_root(root.path().join("voice"))
            .with_voice_transcription_enabled(enabled);
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
        let id = uuid::Uuid::new_v4().to_string();
        let association = VoiceAssociation {
            receiver: receiver.clone(),
            controller: "11".repeat(16),
            subject: "22".repeat(16),
            project: root.path().to_str().unwrap().into(),
            agent: "codex".into(),
            draft: "33".repeat(32),
            revision: "44".repeat(32),
        };
        let audio = b"dummy voice";
        let payload = admission_payload(&id, audio, &association);
        let request = signed_payload(&fixture, &grant, &period, &association, 83, &payload, 1);
        let response = app
            .oneshot(
                Request::put("/v1/phone-control/voice")
                    .header("authorization", format!("Bearer {}", phone.credential))
                    .header("content-type", "audio/mp4")
                    .header("x-louiselm-voice-request", request.to_string())
                    .body(Body::from(audio.as_slice()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_ne!(
            response.status(),
            axum::http::StatusCode::NOT_FOUND,
            "voice admission route is absent"
        );
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        assert_eq!(body["code"], expected);
        assert!(!root.path().join("voice").join(id).exists());
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One owner connection proves both publication and revocation before publication."
)]
async fn admitted_audio_uses_the_owner_scope_and_revocation_fences_publication() {
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tokio::{
        io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
        net::UnixStream,
        sync::oneshot,
    };
    use tower::ServiceExt;
    for scenario in ["accepted", "revoked", "stale", "tampered", "oversized"] {
        let revoke_before_commit = scenario == "revoked";
        let root = tempfile::tempdir().unwrap();
        let receiver = "01".repeat(32);
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let offer = pairing
            .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
            .unwrap();
        let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
        let service = Service::open(&root.path().join("phone/grants"), &receiver, pairing)
            .unwrap()
            .with_voice_root(root.path().join("voice"))
            .with_voice_transcription_enabled(true);
        let socket = root.path().join("controller.sock");
        let cap = root.path().join("capability");
        let server = super::super::socket::ControllerSocket::bind(
            &socket,
            &cap,
            service.controllers.clone(),
        )
        .await
        .unwrap();
        let token = std::fs::read_to_string(&cap).unwrap();
        let server_task = tokio::spawn(server.serve());
        let mut stream = UnixStream::connect(&socket).await.unwrap();
        stream
            .write_all(
                format!("{}\n", json!({"v":1,"type":"attach","capability":token})).as_bytes(),
            )
            .await
            .unwrap();
        let mut line = String::new();
        BufReader::new(&mut stream)
            .read_line(&mut line)
            .await
            .unwrap();
        let controller = serde_json::from_str::<Value>(&line).unwrap()["controller_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let project = std::fs::canonicalize(root.path())
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let (third_tx, third_rx) = oneshot::channel();
        let (continue_tx, continue_rx) = oneshot::channel();
        let (release_tx, release_rx) = oneshot::channel();
        let owner_project = project.clone();
        let owner = tokio::spawn(async move {
            let mut reader = BufReader::new(stream);
            let mut third_tx = Some(third_tx);
            let mut continue_rx = Some(continue_rx);
            let probes = if matches!(scenario, "tampered" | "oversized") {
                1
            } else {
                3
            };
            for index in 0..probes {
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                let request: Value = serde_json::from_str(&line).unwrap();
                assert_eq!(request["type"], "action_scope");
                if index == 2 && revoke_before_commit {
                    third_tx.take().unwrap().send(()).unwrap();
                    continue_rx.take().unwrap().await.unwrap();
                }
                let reply = if index == 2 && scenario == "stale" {
                    json!({"v":1,"type":"error","code":"stale_subject"})
                } else {
                    json!({"v":1,"type":"action_scope","controller_id":request["controller_id"],
                    "subject_id":request["subject_id"],"operation_id":request["operation_id"],
                    "project":owner_project,"agent":"codex"})
                };
                reader
                    .get_mut()
                    .write_all(format!("{reply}\n").as_bytes())
                    .await
                    .unwrap();
            }
            release_rx.await.unwrap();
        });
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
        let id = uuid::Uuid::new_v4().to_string();
        let association = VoiceAssociation {
            receiver: receiver.clone(),
            controller,
            subject: "22".repeat(16),
            project,
            agent: "codex".into(),
            draft: "33".repeat(32),
            revision: "44".repeat(32),
        };
        let audio = b"dummy voice";
        let request = signed_payload(
            &fixture,
            &grant,
            &period,
            &association,
            83,
            &admission_payload(&id, audio, &association),
            1,
        );
        let transmitted = match scenario {
            "tampered" => b"dummy poice".to_vec(),
            "oversized" => vec![0; 20 * 1024 * 1024 + 1],
            _ => audio.to_vec(),
        };
        let upload = tokio::spawn(
            app.oneshot(
                Request::put("/v1/phone-control/voice")
                    .header("authorization", format!("Bearer {}", phone.credential))
                    .header("content-type", "audio/mp4")
                    .header("x-louiselm-voice-request", request.to_string())
                    .body(Body::from(transmitted))
                    .unwrap(),
            ),
        );
        if revoke_before_commit {
            tokio::time::timeout(std::time::Duration::from_secs(10), third_rx)
                .await
                .unwrap()
                .unwrap();
            service.authority.disable(&phone.device_id).unwrap();
            continue_tx.send(()).unwrap();
        }
        let response = tokio::time::timeout(std::time::Duration::from_secs(10), upload)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let body: Value =
            serde_json::from_slice(&to_bytes(response.into_body(), 4096).await.unwrap()).unwrap();
        let archive = VoiceArchive::new(root.path().join("voice")).unwrap();
        if scenario == "accepted" {
            assert_eq!(body["type"], "voice_job");
            assert_eq!(body["state"], "pending");
            assert_eq!(archive.job(&id).unwrap().association, association);
            assert_eq!(
                std::fs::read(archive.capture(&id).unwrap().audio_path).unwrap(),
                audio
            );
        } else {
            assert_eq!(
                body["code"],
                match scenario {
                    "revoked" => "revoked",
                    "stale" => "stale_subject",
                    "tampered" => "malformed",
                    "oversized" => "too_large",
                    _ => unreachable!("known fixture scenario"),
                }
            );
            assert!(
                !root
                    .path()
                    .join("voice")
                    .join(&id)
                    .join("voice-job.json")
                    .exists()
            );
        }
        release_tx.send(()).unwrap();
        owner.await.unwrap();
        server_task.abort();
    }
}

struct RejectFixture;
impl Transcriber for RejectFixture {
    fn name(&self) -> &'static str {
        "fixture"
    }
    fn model(&self) -> &'static str {
        "fixture"
    }
    fn transcribe(&self, _: TranscriptRequest<'_>) -> Result<String, TranscriptionError> {
        Err(TranscriptionError::permanent("fixture failure"))
    }
}

struct SyntheticFixture;
impl Transcriber for SyntheticFixture {
    fn name(&self) -> &'static str {
        "fixture"
    }
    fn model(&self) -> &'static str {
        "fixture"
    }
    fn transcribe(&self, _: TranscriptRequest<'_>) -> Result<String, TranscriptionError> {
        Ok("synthetic voice transcript".into())
    }
}

#[tokio::test]
#[ignore = "requires pinned capture/Neovim binaries and the isolated Android fixture"]
#[expect(
    clippy::too_many_lines,
    reason = "One isolated test connects the actual Android, receiver, owner and fake worker."
)]
async fn pinned_android_recording_reaches_authorized_receiver_and_editable_draft() {
    use std::{
        io::{BufRead, BufReader, Write},
        process::{Command, Stdio},
    };
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
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
    .with_voice_root(voice_root.clone())
    .with_voice_transcription_enabled(true);
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
        vec!["read".into(), "operate_sessions".into()],
    )
    .await;
    let mut child = ChildGuard(
        Command::new(nvim)
            .args(["--headless", "--noplugin", "-u", "NONE", "-l"])
            .arg(project.join("tests/fixtures/phone_voice_headless.lua"))
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
    let owner = tokio::time::timeout(std::time::Duration::from_secs(20), ready)
        .await
        .unwrap()
        .unwrap();
    let controller = owner["controller_id"].as_str().unwrap().to_owned();
    let subject = owner["subject_id"].as_str().unwrap().to_owned();
    let worker = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let worker_port = worker.local_addr().unwrap().port();
    let retained_root = voice_root.clone();
    let worker_task = tokio::spawn(async move {
        let (mut socket, _) = worker.accept().await.unwrap();
        let mut id = String::new();
        tokio::io::BufReader::new(&mut socket)
            .read_line(&mut id)
            .await
            .unwrap();
        let id = id.trim().to_owned();
        let worker_id = id.clone();
        tokio::task::spawn_blocking(move || {
            let archive = VoiceArchive::new(voice_root).unwrap();
            assert_eq!(
                archive.job(&worker_id).unwrap().status,
                VoiceStatus::Pending
            );
            assert_eq!(
                std::fs::read(archive.capture(&worker_id).unwrap().audio_path).unwrap(),
                b"isolated dummy audio"
            );
            assert_eq!(
                VoiceTranscriptionWorker::new(&archive, &SyntheticFixture)
                    .process_ready(1_000)
                    .unwrap(),
                1
            );
            assert_eq!(
                archive.transcript(&worker_id).unwrap().text,
                "synthetic voice transcript"
            );
        })
        .await
        .unwrap();
        socket.write_all(b"ready\n").await.unwrap();
        id
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
                "--tests", "dev.louiselm.capture.PhoneVoiceJobEndToEndTest"])
            .env("LOUISELM_E2E_URL", format!("https://localhost:{}", address.port()))
            .env("LOUISELM_E2E_RECEIVER", tls.public_key_sha256()).env("LOUISELM_E2E_CREDENTIAL", phone.credential)
            .env("LOUISELM_E2E_DEVICE", phone.device_id).env("LOUISELM_E2E_GRANT", grant.grant_id)
            .env("LOUISELM_E2E_CONTROLLER", controller).env("LOUISELM_E2E_SUBJECT", subject)
            .env("LOUISELM_VOICE_WORKER_PORT", worker_port.to_string()).status().unwrap();
        let report = std::fs::read_to_string(android.join("app/build/test-results/testDebugUnitTest/TEST-dev.louiselm.capture.PhoneVoiceJobEndToEndTest.xml")).unwrap();
        assert!(report.contains("tests=\"1\"") && report.contains("skipped=\"0\"") &&
            report.contains("failures=\"0\"") && report.contains("errors=\"0\""));
        status
    }).await.unwrap();
    assert!(status.success());
    let id = worker_task.await.unwrap();
    let retained = VoiceArchive::new(retained_root).unwrap();
    assert_eq!(
        retained.transcript(&id).unwrap().text,
        "synthetic voice transcript"
    );
    assert_eq!(ordinary.list().unwrap().len(), 0);
    child.0.stdin.take().unwrap().write_all(b"stop\n").unwrap();
    assert!(
        child.0.wait().unwrap().success(),
        "the fixture owner must observe zero Session prompts"
    );
    https.abort();
    controller_task.abort();
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One fixture proves retry gating, the failure and its durable duplicate."
)]
async fn retry_requires_enabled_transcription_and_operate_authority() {
    for enabled in [false, true] {
        let root = tempfile::tempdir().unwrap();
        let receiver = "01".repeat(32);
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let offer = pairing
            .issue("https://127.0.0.1:7391", &receiver, 1_000, 10_000)
            .unwrap();
        let phone = pairing.consume(&offer.token, "fixture", 1_001).unwrap();
        let service = Service::open(&root.path().join("phone/grants"), &receiver, pairing)
            .unwrap()
            .with_voice_root(root.path().join("voice"))
            .with_voice_transcription_enabled(enabled);
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
            vec!["operate_sessions".into(), "read".into()],
        )
        .await;
        let archive = VoiceArchive::new(root.path().join("voice")).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let association = VoiceAssociation {
            receiver: receiver.clone(),
            controller: "11".repeat(16),
            subject: "22".repeat(16),
            project: std::fs::canonicalize(root.path())
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            agent: "codex".into(),
            draft: "33".repeat(32),
            revision: "44".repeat(32),
        };
        archive
            .admit(
                &CaptureDraft {
                    id: id.clone(),
                    source: CaptureSource::Android,
                    recorded_at_ms: 1,
                    duration_ms: 1,
                    mime_type: "audio/mp4".into(),
                },
                &association,
                Cursor::new(b"dummy audio"),
            )
            .unwrap();
        assert_eq!(
            VoiceTranscriptionWorker::new(&archive, &RejectFixture)
                .process_ready(1_000)
                .unwrap(),
            1
        );
        assert_eq!(archive.job(&id).unwrap().status, VoiceStatus::Failed);
        let mut cursor = [0_u8; 18];
        cursor[17] = 20;
        let history = super::run_tests::signed_run(
            &fixture,
            &grant,
            &period,
            81,
            &"00".repeat(16),
            &cursor,
            0,
            3,
        );
        let (_, history_reply) = post(
            app.clone(),
            "/v1/phone-control/request",
            &phone.credential,
            &history.to_string(),
        )
        .await;
        assert_eq!(history_reply["records"][0]["failure"], "fixture failure");
        let request = signed_job(&fixture, &grant, &period, &association, 85, &id, 1);
        let (_, reply) = post(
            app,
            "/v1/phone-control/request",
            &phone.credential,
            &request.to_string(),
        )
        .await;
        if enabled {
            assert_eq!(reply["type"], "voice_job");
            assert_eq!(reply["state"], "pending");
            assert_eq!(archive.job(&id).unwrap().status, VoiceStatus::Pending);
            assert_eq!(
                VoiceTranscriptionWorker::new(&archive, &RejectFixture)
                    .process_ready(2_000)
                    .unwrap(),
                1
            );
            let duplicate = signed_job(&fixture, &grant, &period, &association, 85, &id, 2);
            let (_, duplicate_reply) = post(
                service.router(),
                "/v1/phone-control/request",
                &phone.credential,
                &duplicate.to_string(),
            )
            .await;
            assert_eq!(duplicate_reply["state"], "failed");
            assert_eq!(archive.job(&id).unwrap().cycle, 1);
        } else {
            assert_eq!(reply["code"], "unsupported");
            assert_eq!(archive.job(&id).unwrap().status, VoiceStatus::Failed);
        }
    }
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "The same grant proves read-only denial, replay, wrong subject and revocation."
)]
async fn only_current_operate_authority_can_cancel_an_exact_voice_job() {
    for operate in [false, true] {
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
        .unwrap()
        .with_voice_root(root.path().join("voice"));
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
            vec![if operate { "operate_sessions" } else { "read" }.into()],
        )
        .await;
        let archive = VoiceArchive::new(root.path().join("voice")).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let association = VoiceAssociation {
            receiver: receiver.clone(),
            controller: "11".repeat(16),
            subject: "22".repeat(16),
            project: std::fs::canonicalize(root.path())
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            agent: "codex".into(),
            draft: "33".repeat(32),
            revision: "44".repeat(32),
        };
        archive
            .admit(
                &CaptureDraft {
                    id: id.clone(),
                    source: CaptureSource::Android,
                    recorded_at_ms: 1,
                    duration_ms: 1,
                    mime_type: "audio/mp4".into(),
                },
                &association,
                Cursor::new(b"dummy audio"),
            )
            .unwrap();
        if operate {
            let mut wrong = association.clone();
            wrong.subject = "23".repeat(16);
            let request = signed_job(&fixture, &grant, &period, &wrong, 84, &id, 1);
            let (_, reply) = post(
                app.clone(),
                "/v1/phone-control/request",
                &phone.credential,
                &request.to_string(),
            )
            .await;
            assert_eq!(reply["code"], "out_of_scope");
            assert_eq!(archive.job(&id).unwrap().status, VoiceStatus::Pending);
            let mut tampered = signed_job(&fixture, &grant, &period, &association, 84, &id, 2);
            tampered["request"]["payload"] = json!("ff".repeat(16));
            let (_, reply) = post(
                app.clone(),
                "/v1/phone-control/request",
                &phone.credential,
                &tampered.to_string(),
            )
            .await;
            assert_eq!(reply["code"], "unauthenticated");
            assert_eq!(archive.job(&id).unwrap().status, VoiceStatus::Pending);
            for (number, project, agent) in [
                (10, "/unapproved-project", "codex"),
                (11, association.project.as_str(), "unapproved-agent"),
            ] {
                let isolated_id = uuid::Uuid::new_v4().to_string();
                let mut outside = association.clone();
                outside.project = project.into();
                outside.agent = agent.into();
                archive
                    .admit(
                        &CaptureDraft {
                            id: isolated_id.clone(),
                            source: CaptureSource::Android,
                            recorded_at_ms: 1,
                            duration_ms: 1,
                            mime_type: "audio/mp4".into(),
                        },
                        &outside,
                        Cursor::new(b"dummy audio"),
                    )
                    .unwrap();
                let request = signed_job(
                    &fixture,
                    &grant,
                    &period,
                    &outside,
                    84,
                    &isolated_id,
                    number,
                );
                let (_, denied) = post(
                    app.clone(),
                    "/v1/phone-control/request",
                    &phone.credential,
                    &request.to_string(),
                )
                .await;
                assert_eq!(denied["code"], "out_of_scope");
                assert_eq!(
                    archive.job(&isolated_id).unwrap().status,
                    VoiceStatus::Pending
                );
            }
        }
        let request = signed_job(&fixture, &grant, &period, &association, 84, &id, 3);
        let (_, reply) = post(
            app.clone(),
            "/v1/phone-control/request",
            &phone.credential,
            &request.to_string(),
        )
        .await;
        if operate {
            assert_eq!(reply["type"], "voice_job");
            assert_eq!(reply["state"], "cancelled");
            assert_eq!(archive.job(&id).unwrap().status, VoiceStatus::Cancelled);
            let (_, duplicate_response) = post(
                app.clone(),
                "/v1/phone-control/request",
                &phone.credential,
                &request.to_string(),
            )
            .await;
            assert_eq!(duplicate_response["code"], "unauthenticated");
            let second = uuid::Uuid::new_v4().to_string();
            archive
                .admit(
                    &CaptureDraft {
                        id: second.clone(),
                        source: CaptureSource::Android,
                        recorded_at_ms: 1,
                        duration_ms: 1,
                        mime_type: "audio/mp4".into(),
                    },
                    &association,
                    Cursor::new(b"dummy audio"),
                )
                .unwrap();
            service.authority.disable(&phone.device_id).unwrap();
            let stale = signed_job(&fixture, &grant, &period, &association, 84, &second, 4);
            let (_, rejected) = post(
                app,
                "/v1/phone-control/request",
                &phone.credential,
                &stale.to_string(),
            )
            .await;
            assert_eq!(rejected["code"], "revoked");
            assert_eq!(archive.job(&second).unwrap().status, VoiceStatus::Pending);
        } else {
            assert_eq!(reply["code"], "out_of_scope");
            assert_eq!(archive.job(&id).unwrap().status, VoiceStatus::Pending);
        }
    }
}
