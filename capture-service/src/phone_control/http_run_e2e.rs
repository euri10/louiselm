//! Real local owner transport and optional Android pinned-HTTPS Run gate.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "isolated fixture setup and assertions"
)]

use std::{
    io::{BufRead, BufReader, Write},
    os::unix::fs::PermissionsExt,
    process::{Command, Stdio},
    sync::Arc,
};

use serde_json::Value;
use sha2::{Digest, Sha256};

use super::super::{
    Service,
    prepared_owner::{PreparedAdmission, Publication, admit_with, publish_with},
    prepared_owner_tests::selection,
    source::PhoneSourceStore,
    source_checkout_tests::{materialize, setup},
};
use super::{
    run_tests::{signed_run, signed_run_session},
    tests::{history_grant, post},
};
use crate::{PairingRegistry, RunSession, hex::lower_hex};

struct ChildGuard(std::process::Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        // This fixture owns only its own headless mock process.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
#[ignore = "requires the built capture binary and headless Neovim; Android is opt-in"]
#[expect(
    clippy::too_many_lines,
    reason = "one real transport sequence ties publication, Run, controller, Session and Android together"
)]
async fn real_run_owner_and_session_reach_phone() {
    let nvim = std::env::var_os("LOUISELM_TEST_NVIM").unwrap();
    let binary = std::env::var_os("LOUISELM_TEST_CAPTURE").unwrap();
    let project = std::fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).unwrap();
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
    let bytes = selection(root.path(), &checkout.head, "private prepared instruction");
    std::fs::write(&selection_path, &bytes).unwrap();
    std::fs::set_permissions(&selection_path, std::fs::Permissions::from_mode(0o600)).unwrap();
    let saved = publish_with(
        &sources,
        &prepared,
        &runs,
        &Publication {
            run_id: run,
            source_id: &source_id,
            checkout_id: &checkout.checkout_id,
            selection: &selection_path,
            previous: None,
        },
        |_| Ok(()),
    )
    .unwrap();
    let digest = lower_hex(&Sha256::digest(bytes.as_bytes()));
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
    let mut service = Service::open(
        &root.path().join("phone/grants"),
        tls.public_key_sha256(),
        pairing.clone(),
    )
    .unwrap()
    .with_runs(Some(runs.clone()));
    service
        .prepared
        .as_mut()
        .unwrap()
        .set_test_integration(|| Ok(()));
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
    let request = signed_run(
        &fixture,
        &grant,
        &period,
        53,
        &saved.id,
        &[],
        saved.revision,
        1101,
    );
    let (_, draft) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(
        draft["detail"]["beads"][0]["prompt"],
        "private prepared instruction"
    );
    assert!(runs.run(run).is_err());
    admit_with(
        &sources,
        &prepared,
        &runs,
        &PreparedAdmission {
            registration_id: &saved.id,
            revision: saved.revision,
            selection_sha256: &digest,
            generated_work_max: 3,
            park_ttl_ms: 3_600_000,
            token: "generate-token-1234",
        },
        |_| Ok(()),
    )
    .unwrap();
    let mut child = ChildGuard(
        Command::new(nvim)
            .args(["--headless", "--noplugin", "-u", "NONE", "-l"])
            .arg(project.join("tests/fixtures/phone_run_headless.lua"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.path())
            .env("LANG", "C.UTF-8")
            .env("LOUISELM_CAPTURE_STATE_DIR", root.path())
            .env("LOUISELM_TEST_PROJECT", &project)
            .env("LOUISELM_TEST_USAGE", root.path().join("usage"))
            .env("LOUISELM_TEST_CAPTURE", binary)
            .env("LOUISELM_TEST_RUN_ID", run)
            .env("LOUISELM_TEST_REGISTRATION", &saved.id)
            .env("LOUISELM_TEST_SELECTION_SHA256", &digest)
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
    let target = tokio::time::timeout(std::time::Duration::from_secs(20), ready)
        .await
        .unwrap()
        .unwrap();
    let controller = target["controller_id"].as_str().unwrap();
    let session = target["subject_id"].as_str().unwrap();
    let owner = target["owner_id"].as_str().unwrap();
    let acp = target["acp_session_id"].as_str().unwrap();
    runs.attach(RunSession {
        id: run.into(),
        session_id: format!("codex/{acp}"),
        agent: "codex".into(),
        acp_session_id: acp.into(),
        working_dir: project.to_string_lossy().into_owned(),
        load_session: false,
    })
    .unwrap();
    let revision = runs.run(run).unwrap().revision;
    let request = signed_run(&fixture, &grant, &period, 56, &subject, &[], revision, 1102);
    let (_, live) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(live["controller"], "connected");
    assert_eq!(live["live"]["controller_id"], controller);
    assert_eq!(live["live"]["owner_id"], owner);
    assert_eq!(live["live"]["sessions"][0]["subject_id"], session);
    let request = signed_run_session(
        &fixture, &grant, &period, controller, session, run, owner, revision, 1103,
    );
    let (_, page) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(page["type"], "session_page");
    assert!(
        page["entries"]
            .as_array()
            .unwrap()
            .iter()
            .any(|entry| entry["text"] == "dummy Run fixture request")
    );
    let wrong = signed_run_session(
        &fixture,
        &grant,
        &period,
        controller,
        session,
        run,
        &"ff".repeat(16),
        revision,
        1104,
    );
    let (_, wrong) = post(
        app.clone(),
        "/v1/phone-control/request",
        &phone.credential,
        &wrong.to_string(),
    )
    .await;
    assert_eq!(wrong["code"], "stale_subject");

    if std::env::var("LOUISELM_TEST_ANDROID").as_deref() == Ok("1") {
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
        let url = format!("https://localhost:{}", address.port());
        let receiver = tls.public_key_sha256().to_owned();
        let credential = phone.credential.clone();
        let device = phone.device_id.clone();
        let grant_id = grant.grant_id.clone();
        let status = tokio::task::spawn_blocking(move || {
            let status = Command::new(android.join("gradlew")).current_dir(&android)
                .args(["--no-daemon", "--max-workers=1", "--rerun-tasks", "-PlouiselmFirebase=false",
                    "testDebugUnitTest", "--tests", "dev.louiselm.capture.PhoneRunLiveEndToEndTest"])
                .env("LOUISELM_E2E_URL", url).env("LOUISELM_E2E_RECEIVER", receiver)
                .env("LOUISELM_E2E_CREDENTIAL", credential).env("LOUISELM_E2E_DEVICE", device)
                .env("LOUISELM_E2E_GRANT", grant_id).env("LOUISELM_E2E_RUN_ID", run)
                .status().unwrap();
            let report = std::fs::read_to_string(android.join("app/build/test-results/testDebugUnitTest/TEST-dev.louiselm.capture.PhoneRunLiveEndToEndTest.xml")).unwrap();
            assert!(report.contains("tests=\"1\"") && report.contains("skipped=\"0\"")
                && report.contains("failures=\"0\"") && report.contains("errors=\"0\""));
            status
        }).await.unwrap();
        assert!(status.success(), "Android Run integration failed");
        https.abort();
    }
    child.0.stdin.take().unwrap().write_all(b"stop\n").unwrap();
    let status = tokio::task::spawn_blocking(move || child.0.wait().unwrap())
        .await
        .unwrap();
    assert!(status.success());
    controller_task.abort();
}
