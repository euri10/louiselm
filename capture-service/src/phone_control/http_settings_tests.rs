//! Pinned Android reads of real headless Session settings.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "isolated fixtures and assertions"
)]

use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::Arc,
};

use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use super::tests::scoped_grant;
use crate::{
    PairingRegistry,
    phone_control::{Service, authority::Scope},
};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        // This process and both mock Agents belong only to this fixture.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
#[ignore = "requires the pinned capture binary, headless Neovim and isolated Android fixture"]
#[expect(
    clippy::too_many_lines,
    reason = "one grant and Activity must observe the ordered desktop change"
)]
async fn pinned_android_displays_acknowledged_session_settings() {
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
    let service = Service::open(
        &root.path().join("grants/state"),
        tls.public_key_sha256(),
        pairing.clone(),
    )
    .unwrap();
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
    let app = crate::Receiver::new(
        crate::Store::new(root.path().join("captures")).unwrap(),
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
    let (grant, _) = scoped_grant(
        &service,
        &app,
        &phone,
        &fixture,
        Scope {
            projects: vec![project.to_str().unwrap().into()],
            agents: vec!["codex".into(), "missing".into(), "queue".into()],
            groups: vec!["read".into()],
        },
    )
    .await;
    let mut child = ChildGuard(
        Command::new(std::env::var_os("LOUISELM_TEST_NVIM").unwrap())
            .args(["--headless", "--noplugin", "-u", "NONE", "-l"])
            .arg(project.join("tests/fixtures/phone_settings_headless.lua"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.path())
            .env("LANG", "C.UTF-8")
            .env("LOUISELM_CAPTURE_STATE_DIR", root.path())
            .env("LOUISELM_TEST_PROJECT", &project)
            .env("LOUISELM_TEST_USAGE", root.path().join("usage"))
            .env(
                "LOUISELM_TEST_CAPTURE",
                std::env::var_os("LOUISELM_TEST_CAPTURE").unwrap(),
            )
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut output = BufReader::new(child.0.stdout.take().unwrap());
    let (target, output) = tokio::time::timeout(
        std::time::Duration::from_secs(20),
        tokio::task::spawn_blocking(move || {
            let mut line = String::new();
            output.read_line(&mut line).unwrap();
            (serde_json::from_str::<Value>(&line).unwrap(), output)
        }),
    )
    .await
    .unwrap()
    .unwrap();
    let mut input = child.0.stdin.take().unwrap();
    let coordinator = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = coordinator.local_addr().unwrap().port();
    let control = tokio::spawn(async move {
        let (mut socket, _) =
            tokio::time::timeout(std::time::Duration::from_secs(180), coordinator.accept())
                .await
                .unwrap()
                .unwrap();
        let mut request = [0; 7];
        socket.read_exact(&mut request).await.unwrap();
        assert_eq!(&request, b"change\n");
        let (input, line) = tokio::task::spawn_blocking(move || {
            input.write_all(b"change\n").unwrap();
            input.flush().unwrap();
            let mut line = String::new();
            let mut output = output;
            output.read_line(&mut line).unwrap();
            (input, line)
        })
        .await
        .unwrap();
        assert_eq!(line, "acknowledged\n");
        socket.write_all(b"acknowledged\n").await.unwrap();
        input
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
        let result = Command::new(android.join("gradlew")).current_dir(&android)
            .args(["--no-daemon", "--max-workers=1", "--rerun-tasks", "-PlouiselmFirebase=false",
                "testDebugUnitTest", "--tests", "dev.louiselm.capture.PhoneSettingsEndToEndTest"])
            .env("LOUISELM_E2E_URL", format!("https://localhost:{}", address.port()))
            .env("LOUISELM_E2E_RECEIVER", tls.public_key_sha256())
            .env("LOUISELM_E2E_CREDENTIAL", phone.credential).env("LOUISELM_E2E_DEVICE", phone.device_id)
            .env("LOUISELM_E2E_GRANT", grant.grant_id).env("LOUISELM_E2E_SETTINGS_PORT", port.to_string())
            .env("LOUISELM_E2E_SUBJECT", target["subject_id"].as_str().unwrap())
            .env("LOUISELM_E2E_OTHER_SUBJECT", target["other_subject_id"].as_str().unwrap())
            .env("LOUISELM_E2E_QUEUE_SUBJECT", target["queue_subject_id"].as_str().unwrap())
            .status().unwrap();
        let report = std::fs::read_to_string(android.join("app/build/test-results/testDebugUnitTest/TEST-dev.louiselm.capture.PhoneSettingsEndToEndTest.xml")).unwrap();
        assert!(report.contains("tests=\"1\"") && report.contains("skipped=\"0\"")
            && report.contains("failures=\"0\"") && report.contains("errors=\"0\""));
        result
    }).await.unwrap();
    assert!(status.success(), "Android settings integration failed");
    println!("PHONE_QUEUE_PINNED_ANDROID_PASSED");
    let mut input = control.await.unwrap();
    input.write_all(b"stop\n").unwrap();
    assert!(
        tokio::task::spawn_blocking(move || child.0.wait().unwrap())
            .await
            .unwrap()
            .success()
    );
    https.abort();
    controller_task.abort();
}
