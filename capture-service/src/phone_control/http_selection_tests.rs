//! Signed selection requests across the receiver and an attached owner.
#![allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]

use std::{sync::Arc, time::Duration};

use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};

use super::tests::{history_grant, post, signed_discovery, signed_discovery_payload};
use crate::{PairingRegistry, phone_control::Service};

async fn answer(
    app: axum::Router,
    credential: &str,
    request: Value,
    stream: &mut UnixStream,
    agents: Value,
) -> Value {
    let pending = tokio::spawn({
        let credential = credential.to_owned();
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
    let mut line = String::new();
    let read = tokio::time::timeout(
        Duration::from_secs(2),
        BufReader::new(&mut *stream).read_line(&mut line),
    )
    .await;
    assert!(
        read.is_ok(),
        "selection request was not dispatched to the attached owner"
    );
    let frame: Value = serde_json::from_str(&line).unwrap();
    assert_eq!(frame["type"], "list_selection");
    let response = json!({"v":1,"type":"selection_agents","controller_id":frame["controller_id"],
        "operation_id":frame["request"]["operation_id"],"agents":agents});
    stream
        .write_all(format!("{response}\n").as_bytes())
        .await
        .unwrap();
    pending.await.unwrap().1
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "one signed lifecycle covers scope, replacement and revocation"
)]
async fn selection_uses_current_project_identities_and_the_exact_owner() {
    let root = tempfile::tempdir().unwrap();
    let approved = root.path().join("approved");
    let removed = root.path().join("removed");
    std::fs::create_dir(&approved).unwrap();
    std::fs::create_dir(&removed).unwrap();
    let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
    let receiver = "01".repeat(32);
    let offer = pairing
        .issue("https://192.168.1.2:7391", &receiver, 1_000, 10_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "phone", 1_001).unwrap();
    let service = Service::open(&root.path().join("phone/grants"), &receiver, pairing).unwrap();
    let socket = root.path().join("controller.sock");
    let cap = root.path().join("capability");
    let server =
        super::super::socket::ControllerSocket::bind(&socket, &cap, service.controllers.clone())
            .await
            .unwrap();
    let token = std::fs::read_to_string(&cap).unwrap();
    let server_task = tokio::spawn(server.serve());
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
        vec!["read".into(), "operate_sessions".into()],
    )
    .await;
    let approved_id = service
        .authority
        .register_project(approved.to_str().unwrap(), &grant.project_root_ids)
        .unwrap();
    service
        .authority
        .register_project(removed.to_str().unwrap(), &grant.project_root_ids)
        .unwrap();
    let foreign = tempfile::tempdir().unwrap();
    let foreign_root = service
        .authority
        .register_root(foreign.path().to_str().unwrap())
        .unwrap();
    service
        .authority
        .register_project(foreign.path().to_str().unwrap(), &[foreign_root])
        .unwrap();
    std::fs::remove_dir(&removed).unwrap();
    let mut stream = UnixStream::connect(&socket).await.unwrap();
    stream
        .write_all(format!("{}\n", json!({"v":1,"type":"attach","capability":token})).as_bytes())
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
    let request = signed_discovery(&fixture, &grant, &period, &controller, 50, 1);
    let result = answer(
        app.clone(),
        &phone.credential,
        request,
        &mut stream,
        json!(["codex"]),
    )
    .await;
    assert_eq!(result["type"], "selection_list");
    assert_eq!(result["agents"], json!(["codex"]));
    assert_eq!(
        result["projects"],
        json!([{"id":approved_id,"path":approved.to_str().unwrap()}])
    );
    assert_eq!(result["operate_sessions"], true);

    let second = root.path().join("second-approved");
    std::fs::create_dir(&second).unwrap();
    let second_id = service
        .authority
        .register_project(second.to_str().unwrap(), &grant.project_root_ids)
        .unwrap();
    let mut payload = [0_u8; 17];
    payload[16] = 1;
    let request = signed_discovery_payload(&fixture, &grant, &period, &controller, 50, 7, payload);
    let first = answer(
        app.clone(),
        &phone.credential,
        request,
        &mut stream,
        json!(["codex"]),
    )
    .await;
    let first_id = first["projects"][0]["id"].as_str().unwrap();
    assert_eq!(first["projects"].as_array().unwrap().len(), 1);
    assert_eq!(first["next_after"], first_id);
    payload[..16].copy_from_slice(&super::super::fixed::<16>(first_id).unwrap());
    let request = signed_discovery_payload(&fixture, &grant, &period, &controller, 50, 8, payload);
    let next = answer(
        app.clone(),
        &phone.credential,
        request,
        &mut stream,
        json!(["codex"]),
    )
    .await;
    let next_id = next["projects"][0]["id"].as_str().unwrap();
    assert_eq!(next["projects"].as_array().unwrap().len(), 1);
    assert_eq!(next["next_after"], "");
    assert_eq!(
        [first_id, next_id]
            .into_iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [approved_id.as_str(), second_id.as_str()]
            .into_iter()
            .collect()
    );
    std::fs::remove_dir(&second).unwrap();

    let request = signed_discovery(&fixture, &grant, &period, &controller, 50, 2);
    let refused = answer(
        app.clone(),
        &phone.credential,
        request,
        &mut stream,
        json!(["unapproved"]),
    )
    .await;
    assert_eq!(refused["code"], "unavailable");

    std::fs::rename(&approved, root.path().join("moved")).unwrap();
    std::fs::create_dir(&approved).unwrap();
    let request = signed_discovery(&fixture, &grant, &period, &controller, 50, 3);
    let changed = answer(
        app.clone(),
        &phone.credential,
        request,
        &mut stream,
        json!(["codex"]),
    )
    .await;
    assert_eq!(changed["projects"], json!([]));

    service.authority.disable(&phone.device_id).unwrap();
    let (read_only, period) = history_grant(
        &service,
        &app,
        &phone,
        &fixture,
        root.path(),
        vec!["read".into()],
    )
    .await;
    let request = signed_discovery(&fixture, &read_only, &period, &controller, 50, 4);
    let read = answer(
        app.clone(),
        &phone.credential,
        request,
        &mut stream,
        json!(["codex"]),
    )
    .await;
    assert_eq!(read["operate_sessions"], false);
    let request = signed_discovery(&fixture, &read_only, &period, &controller, 50, 5);
    let pending = tokio::spawn({
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
    line.clear();
    tokio::time::timeout(
        Duration::from_secs(2),
        BufReader::new(&mut stream).read_line(&mut line),
    )
    .await
    .unwrap()
    .unwrap();
    let frame: Value = serde_json::from_str(&line).unwrap();
    service.authority.disable(&phone.device_id).unwrap();
    let response = json!({"v":1,"type":"selection_agents","controller_id":controller,
        "operation_id":frame["request"]["operation_id"],"agents":["codex"]});
    stream
        .write_all(format!("{response}\n").as_bytes())
        .await
        .unwrap();
    assert_eq!(pending.await.unwrap().1["code"], "revoked");
    let (no_read, period) = history_grant(
        &service,
        &app,
        &phone,
        &fixture,
        root.path(),
        vec!["operate_sessions".into()],
    )
    .await;
    let request = signed_discovery(&fixture, &no_read, &period, &controller, 50, 6);
    let (_, denied) = post(
        app,
        "/v1/phone-control/request",
        &phone.credential,
        &request.to_string(),
    )
    .await;
    assert_eq!(denied["code"], "out_of_scope");
    server_task.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires built capture, headless Neovim and the Android private integration gate"]
#[expect(
    clippy::too_many_lines,
    reason = "the isolated owner, receiver and Android view form one acceptance"
)]
async fn android_selection_reaches_a_real_owner_with_no_sessions() {
    use std::{
        io::Write,
        os::unix::fs::PermissionsExt,
        path::{Path, PathBuf},
        process::{Child, Command, Stdio},
    };

    use super::super::{authority::Scope, socket::ControllerRegistry};
    use super::tests::approve_scoped_grant;

    struct ChildGuard(Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            // Test cleanup is best effort only after a panic; it cannot grant authority.
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn stop(child: &mut ChildGuard) {
        child.0.stdin.take().unwrap().write_all(b"stop\n").unwrap();
        assert!(child.0.wait().unwrap().success());
    }
    fn spawn_owner(
        nvim: &Path,
        binary: &Path,
        source: &Path,
        root: &Path,
        wrapper: &Path,
        marker: &Path,
    ) -> ChildGuard {
        ChildGuard(
            Command::new(nvim)
                .args(["--headless", "--noplugin", "-u", "NONE", "-l"])
                .arg(source.join("tests/fixtures/phone_selection_headless.lua"))
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("HOME", root)
                .env("LANG", "C.UTF-8")
                .env("LOUISELM_CAPTURE_STATE_DIR", root)
                .env("LOUISELM_TEST_PROJECT", source)
                .env("LOUISELM_TEST_CAPTURE", binary)
                .env("LOUISELM_TEST_AGENT_WRAPPER", wrapper)
                .env("LOUISELM_TEST_AGENT_MARKER", marker)
                .stdin(Stdio::piped())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap(),
        )
    }
    async fn wait_owner(controllers: &ControllerRegistry, excluded: &str) -> String {
        tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                let ids = controllers.list_after("", 50).unwrap();
                if ids.len() == 1 && ids[0] != excluded {
                    return ids[0].clone();
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    let nvim = PathBuf::from(std::env::var_os("LOUISELM_TEST_NVIM").unwrap());
    let binary = PathBuf::from(std::env::var_os("LOUISELM_TEST_CAPTURE").unwrap());
    let source = std::fs::canonicalize(concat!(env!("CARGO_MANIFEST_DIR"), "/..")).unwrap();
    let root = tempfile::tempdir().unwrap();
    let allowed_root = root.path().join("approved-root");
    let project = allowed_root.join("approved-project");
    let removed = allowed_root.join("removed-project");
    let foreign = root.path().join("foreign-project");
    for path in [&project, &removed, &foreign] {
        std::fs::create_dir_all(path).unwrap();
    }
    let marker = root.path().join("agent-started");
    let wrapper = root.path().join("mock-acp");
    std::fs::write(
        &wrapper,
        "#!/bin/sh\n: > \"$LOUISELM_TEST_AGENT_MARKER\"\nexec \"$@\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o700)).unwrap();
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
    let grant = approve_scoped_grant(
        &service,
        &phone,
        &fixture,
        Scope {
            projects: vec![allowed_root.to_str().unwrap().into()],
            agents: vec!["codex".into(), "removed-agent".into()],
            groups: vec!["read".into(), "operate_sessions".into()],
        },
    );
    service
        .authority
        .register_project(project.to_str().unwrap(), &grant.project_root_ids)
        .unwrap();
    service
        .authority
        .register_project(removed.to_str().unwrap(), &grant.project_root_ids)
        .unwrap();
    std::fs::remove_dir(&removed).unwrap();
    let foreign_root = service
        .authority
        .register_root(foreign.to_str().unwrap())
        .unwrap();
    service
        .authority
        .register_project(foreign.to_str().unwrap(), &[foreign_root])
        .unwrap();
    let mut owner = spawn_owner(&nvim, &binary, &source, root.path(), &wrapper, &marker);
    let original = wait_owner(&service.controllers, "").await;
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
    let control = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = control.local_addr().unwrap().port();
    let (done, finished) = tokio::sync::oneshot::channel();
    let controllers = service.controllers.clone();
    let root_path = root.path().to_path_buf();
    let source_copy = source.clone();
    let marker_copy = marker.clone();
    let control_task = tokio::spawn(async move {
        let (mut socket, _) = control.accept().await.unwrap();
        let mut command = String::new();
        BufReader::new(&mut socket)
            .read_line(&mut command)
            .await
            .unwrap();
        assert_eq!(command, "replace\n");
        tokio::task::spawn_blocking(move || stop(&mut owner))
            .await
            .unwrap();
        let mut replacement = spawn_owner(
            &nvim,
            &binary,
            &source_copy,
            &root_path,
            &wrapper,
            &marker_copy,
        );
        wait_owner(&controllers, &original).await;
        socket.write_all(b"replaced\n").await.unwrap();
        let _ = finished.await;
        tokio::task::spawn_blocking(move || stop(&mut replacement))
            .await
            .unwrap();
    });
    let android = source.join("android");
    let url = format!("https://localhost:{}", address.port());
    let receiver = tls.public_key_sha256().to_owned();
    let project_path = project.clone();
    let status = tokio::task::spawn_blocking(move || {
        let status = Command::new(android.join("gradlew"))
            .current_dir(&android).args(["--no-daemon", "--max-workers=1", "--rerun-tasks", "-PlouiselmFirebase=false",
                "testDebugUnitTest", "--tests", "dev.louiselm.capture.PhoneSelectionEndToEndTest"])
            .env("LOUISELM_E2E_URL", url).env("LOUISELM_E2E_RECEIVER", receiver)
            .env("LOUISELM_E2E_CREDENTIAL", phone.credential).env("LOUISELM_E2E_DEVICE", phone.device_id)
            .env("LOUISELM_E2E_GRANT", grant.grant_id).env("LOUISELM_E2E_SELECTION_PORT", port.to_string())
            .env("LOUISELM_E2E_SELECTION_PROJECT", project_path).status().unwrap();
        let report = std::fs::read_to_string(android.join("app/build/test-results/testDebugUnitTest/TEST-dev.louiselm.capture.PhoneSelectionEndToEndTest.xml")).unwrap();
        assert!(report.contains("tests=\"1\"") && report.contains("skipped=\"0\"")
            && report.contains("failures=\"0\"") && report.contains("errors=\"0\""));
        status
    }).await.unwrap();
    assert!(status.success());
    done.send(()).unwrap();
    control_task.await.unwrap();
    assert!(!marker.exists(), "selection started the mock ACP Agent");
    https.abort();
    controller_task.abort();
}
