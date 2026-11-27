//! Actual Android voice review and Send through a disposable paired receiver.

use super::*;
use std::{
    io::{BufRead, BufReader, Write},
    path::Path,
    process::{Child, Command, Stdio},
    sync::mpsc,
    time::Duration,
};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt};

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn wait_for(path: &Path) {
    tokio::time::timeout(Duration::from_secs(300), async {
        while !path.is_file() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("missing Android checkpoint {}", path.display()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "requires pinned capture/Neovim binaries and the isolated Android fixture"]
#[expect(
    clippy::too_many_lines,
    reason = "The acceptance observes the complete Android, receiver, worker and Session lifecycle."
)]
async fn reviewed_voice_reaches_only_the_selected_native_session() {
    let nvim = std::env::var_os("LOUISELM_TEST_NVIM").unwrap();
    let binary = std::env::var_os("LOUISELM_TEST_CAPTURE").unwrap();
    let project = std::fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).unwrap();
    let root = tempfile::tempdir_in("/tmp").unwrap();
    let capture = root.path().join("louiselm/capture");
    let tls = crate::TlsIdentity::load_or_create(capture.join("tls")).unwrap();
    let pairing = Arc::new(PairingRegistry::open(capture.join("pairing")).unwrap());
    pairing
        .with_personal_receiver(|unbound| {
            assert!(unbound);
            Ok::<_, crate::PairingError>(((), Some(tls.public_key_sha256().to_owned())))
        })
        .unwrap();
    crate::phone_control::personal_startup::prepare(&capture, tls.public_key_sha256(), &pairing)
        .unwrap();
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
        &capture.join("phone-control/grants.json"),
        tls.public_key_sha256(),
        pairing.clone(),
    )
    .unwrap()
    .with_voice_root(voice_root.clone())
    .with_voice_transcription_enabled(true);
    let workflow = root.path().join("louiselm/workflow");
    let sockets = service
        .bind(
            &workflow.join("phone-control-admin.sock"),
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
    let receipts = root.path().join("receipts");
    std::fs::create_dir(&receipts).unwrap();
    let mut child = ChildGuard(
        Command::new(nvim)
            .args(["--headless", "--noplugin", "-u", "NONE", "-l"])
            .arg(project.join("tests/fixtures/phone_voice_send_headless.lua"))
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", root.path())
            .env("LANG", "C.UTF-8")
            .env("LOUISELM_CAPTURE_STATE_DIR", root.path())
            .env("LOUISELM_TEST_PROJECT", &project)
            .env("LOUISELM_TEST_RECEIPTS", &receipts)
            .env("LOUISELM_VOICE_SEND_SYNC", root.path())
            .env("LOUISELM_TEST_CAPTURE", binary)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let stdout = child.0.stdout.take().unwrap();
    let (sender, reports) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines().map_while(Result::ok) {
            if sender.send(line).is_err() {
                return;
            }
        }
    });
    let report = || -> Value {
        serde_json::from_str(&reports.recv_timeout(Duration::from_secs(30)).unwrap()).unwrap()
    };
    let owner = report();
    assert_ne!(owner["subject_id"], owner["other_subject_id"]);
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
    let sync = root.path().to_owned();
    let mut status = tokio::task::spawn_blocking(move || {
        let status = Command::new(android.join("gradlew"))
            .current_dir(&android)
            .args([
                "--no-daemon",
                "--max-workers=1",
                "-PlouiselmFirebase=false",
                "--offline",
                "testDebugUnitTest",
                "--rerun",
                "--tests",
                "dev.louiselm.capture.PhoneVoiceSendEndToEndTest",
            ])
            .env(
                "LOUISELM_E2E_URL",
                format!("https://localhost:{}", address.port()),
            )
            .env("LOUISELM_E2E_RECEIVER", tls.public_key_sha256())
            .env("LOUISELM_E2E_CREDENTIAL", phone.credential)
            .env("LOUISELM_E2E_DEVICE", phone.device_id)
            .env("LOUISELM_E2E_GRANT", grant.grant_id)
            .env("LOUISELM_E2E_CONTROLLER", controller)
            .env("LOUISELM_E2E_SUBJECT", subject)
            .env("LOUISELM_VOICE_SEND_SYNC", sync)
            .env("LOUISELM_VOICE_WORKER_PORT", worker_port.to_string())
            .status()
            .unwrap();
        let xml = std::fs::read_to_string(android.join("app/build/test-results/testDebugUnitTest/TEST-dev.louiselm.capture.PhoneVoiceSendEndToEndTest.xml")).unwrap();
        for count in [
            "tests=\"1\"",
            "skipped=\"0\"",
            "failures=\"0\"",
            "errors=\"0\"",
        ] {
            assert!(
                xml.contains(count),
                "missing Android execution receipt: {count}"
            );
        }
        status
    });
    for (step, expected, expected_other) in [
        ("unlocked", 1, 0),
        ("recording", 1, 0),
        ("stopped", 1, 0),
        ("transcribed", 1, 0),
        ("reunlock", 1, 0),
        ("reconnected", 1, 0),
        ("review", 1, 0),
        ("sent", 2, 0),
        ("finished", 2, 0),
        ("other-before", 2, 0),
        ("other-review", 2, 0),
        ("other-sent", 2, 1),
        ("other-finished", 2, 1),
    ] {
        let checkpoint = root.path().join(step);
        tokio::select! {
            () = wait_for(&checkpoint) => {}
            result = &mut status => panic!("Android exited before {step}: {result:?}"),
        }
        writeln!(
            child.0.stdin.as_mut().unwrap(),
            "check {expected} {expected_other}"
        )
        .unwrap();
        let observation = report();
        assert_eq!(observation["selected"], expected, "checkpoint {step}");
        assert_eq!(observation["other"], expected_other, "checkpoint {step}");
        assert_eq!(observation["draft_preserved"], true);
        if step == "unlocked" || step == "reunlock" || step == "other-before" {
            std::fs::write(root.path().join("target.json"), observation.to_string()).unwrap();
        }
        std::fs::write(root.path().join(format!("{step}.done")), []).unwrap();
    }
    assert!(status.await.unwrap().success());
    let id = worker_task.await.unwrap();
    let retained = VoiceArchive::new(retained_root).unwrap();
    assert_eq!(
        retained.transcript(&id).unwrap().text,
        "synthetic voice transcript"
    );
    assert_eq!(
        std::fs::read(retained.capture(&id).unwrap().audio_path).unwrap(),
        b"isolated dummy audio"
    );
    assert_eq!(ordinary.list().unwrap().len(), 0);
    child.0.stdin.take().unwrap().write_all(b"stop\n").unwrap();
    assert!(child.0.wait().unwrap().success());
    https.abort();
    controller_task.abort();
}
