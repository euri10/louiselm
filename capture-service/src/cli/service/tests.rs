//! Capability startup checks against private stores and actual local sockets.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Tests fail directly on fixture setup or contract violations."
)]

use super::*;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    net::UnixStream,
};

fn paths(root: &std::path::Path) -> Paths {
    Paths {
        config: root.join("config"),
        data: root.join("data"),
        state: root.join("state"),
    }
}

fn attention_only() -> Features {
    Features {
        attention: true,
        runs: false,
        receiver: false,
        transcription: false,
        push: false,
        phone_control: false,
    }
}

#[tokio::test]
async fn attention_only_serves_snapshots_without_capture_run_or_delivery_resources() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let socket = paths.attention_socket();
    let prepared = prepare(paths.clone(), attention_only()).unwrap();
    let server = tokio::spawn(serve_prepared(prepared, attention_only(), None, None));
    let stream = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(stream) = UnixStream::connect(&socket).await {
                break stream;
            }
            assert!(
                !server.is_finished(),
                "Attention service stopped before readiness"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let mut lines = BufReader::new(stream).lines();
    let line = tokio::time::timeout(Duration::from_secs(5), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let snapshot: serde_json::Value = serde_json::from_str(&line).unwrap();
    assert_eq!(snapshot["type"], "snapshot");
    for absent in [
        paths.runs(),
        paths.run_socket(),
        paths.captures(),
        paths.pairing(),
        paths.tls(),
        paths.uploads(),
        paths.phone_control_store(),
        paths.phone_control_socket(),
        paths.phone_control_admin_socket(),
    ] {
        assert!(
            !absent.exists(),
            "unexpected optional resource: {}",
            absent.display()
        );
    }
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
}

#[tokio::test]
async fn phone_control_routes_exist_only_after_explicit_opt_in() {
    use axum::{
        body::{Body, to_bytes},
        http::{Request, StatusCode},
    };
    use tower::ServiceExt;

    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let mut features = attention_only();
    features.attention = false;
    features.receiver = true;
    let disabled = prepare(paths.clone(), features).unwrap();
    let disabled_app = disabled.receiver.as_ref().unwrap().0.router();
    let request = || {
        Request::post("/v1/phone-control/enrollment/offer")
            .header("content-type", "application/json")
            .body(Body::from(r#"{"v":1,"type":"enrollment_offer"}"#))
            .unwrap()
    };
    assert_eq!(
        disabled_app.oneshot(request()).await.unwrap().status(),
        StatusCode::NOT_FOUND
    );
    assert!(!paths.phone_control_store().exists());
    let evidence = paths
        .phone_control_store()
        .parent()
        .unwrap()
        .join("action-evidence");
    assert!(!evidence.exists());

    features.phone_control = true;
    let enabled = prepare(paths, features).unwrap();
    assert!(!evidence.exists());
    let enabled_app = enabled.receiver.as_ref().unwrap().0.router();
    let response = enabled_app.oneshot(request()).await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let body = to_bytes(response.into_body(), 1024).await.unwrap();
    let value: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(
        value,
        serde_json::json!({"v":1,"type":"error","code":"unauthenticated"})
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One service lifecycle covers corrupt grants, capture, revocation and explicit recovery."
)]
async fn damaged_phone_grants_leave_capture_and_revocation_available() {
    use serde::Serialize;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    #[derive(Serialize)]
    struct Scope {
        projects: Vec<String>,
        agents: Vec<String>,
        groups: Vec<String>,
    }

    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let identity = TlsIdentity::load_or_create(paths.tls()).unwrap();
    let receiver = identity.public_key_sha256();
    let pairing = PairingRegistry::open(paths.pairing()).unwrap();
    let offer = pairing
        .issue("https://localhost:7391", receiver, 1_000, 5_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "phone", 1_001).unwrap();
    let offer = pairing
        .issue("https://localhost:7391", receiver, 1_000, 5_000)
        .unwrap();
    let other = pairing.consume(&offer.token, "other phone", 1_001).unwrap();
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let enrolled_key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
    let enrollment_id = "11".repeat(16);
    let grant_id = "22".repeat(16);
    let scope = Scope {
        projects: vec![root.path().to_str().unwrap().to_owned()],
        agents: vec!["codex".into()],
        groups: vec!["read".into()],
    };
    let seal = crate::hex::lower_hex(&Sha256::digest(
        serde_json::to_vec(&(
            &phone.device_id,
            &enrollment_id,
            &grant_id,
            1_u64,
            enrolled_key,
            receiver,
            &scope,
        ))
        .unwrap(),
    ));
    let mut stored = json!({"grants":{},"revisions":{}});
    stored["grants"][&phone.device_id] = json!({
        "device_id":phone.device_id,"enrollment_id":enrollment_id,"grant_id":grant_id,
        "grant_revision":1,"enrolled_key":enrolled_key,"receiver_identity":receiver,
        "scope":scope,"grant_scope_sha256":seal
    });
    stored["revisions"][&phone.device_id] = json!(1);
    std::fs::create_dir_all(paths.phone_control_store().parent().unwrap()).unwrap();
    std::fs::write(
        paths.phone_control_store(),
        serde_json::to_vec(&stored).unwrap(),
    )
    .unwrap();
    let mut features = attention_only();
    features.attention = false;
    features.receiver = true;
    features.phone_control = true;
    drop(prepare(paths.clone(), features).unwrap());

    stored["grants"][&phone.device_id]["scope"]["agents"] = json!(["codex", "foreign"]);
    let damaged = serde_json::to_vec(&stored).unwrap();
    std::fs::write(paths.phone_control_store(), &damaged).unwrap();
    let mut prepared = prepare(paths.clone(), features).unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    prepared.receiver.as_mut().unwrap().1 = serde_json::from_value(json!({
        "schema_version":1,"profile":null,"bind":address.to_string(),"receiver_url":null
    }))
    .unwrap();
    let certificate = std::fs::read(identity.certificate_path()).unwrap();
    let client = reqwest::Client::builder()
        .tls_certs_only([reqwest::Certificate::from_pem(&certificate).unwrap()])
        .build()
        .unwrap();
    let server = tokio::spawn(serve_prepared(prepared, features, None, None));
    let base = format!("https://localhost:{}", address.port());
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if client.get(format!("{base}/v1/health")).send().await.is_ok() {
                break;
            }
            assert!(!server.is_finished(), "receiver stopped before readiness");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let response = client
        .post(format!("{base}/v1/phone-control/challenge"))
        .bearer_auth(&phone.credential)
        .json(&json!({"v":1,"type":"challenge","grant_id":grant_id,"grant_revision":1}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["code"],
        "unavailable"
    );
    let response = client
        .post(format!("{base}/v1/phone-control/enrollment/offer"))
        .bearer_auth(&other.credential)
        .json(&json!({"v":1,"type":"enrollment_offer"}))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), reqwest::StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(
        response.json::<serde_json::Value>().await.unwrap()["code"],
        "unavailable"
    );
    let error = super::super::phone_control_admin(
        &paths,
        super::super::PhoneControl::Start {
            device_id: other.device_id.clone(),
        },
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("unavailable"));

    let capture_id = uuid::Uuid::new_v4().to_string();
    let audio = b"isolated capture fixture";
    let digest = crate::hex::lower_hex(&Sha256::digest(audio));
    let upload = || {
        client
            .put(format!("{base}/v1/captures/{capture_id}"))
            .bearer_auth(&phone.credential)
            .header("content-type", "audio/mp4")
            .header("x-louiselm-source", "android")
            .header("x-louiselm-recorded-at-ms", "1765000000000")
            .header("x-louiselm-duration-ms", "4200")
            .header("x-louiselm-sha256", &digest)
            .body(audio.as_slice())
    };
    assert_eq!(
        upload().send().await.unwrap().status(),
        reqwest::StatusCode::CREATED
    );
    assert_eq!(
        Store::new(paths.captures())
            .unwrap()
            .capture(&capture_id)
            .unwrap()
            .record
            .sha256,
        digest
    );
    super::super::revoke_device(&paths, &phone.device_id)
        .await
        .unwrap();
    assert_eq!(
        upload().send().await.unwrap().status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    assert_eq!(std::fs::read(paths.phone_control_store()).unwrap(), damaged);
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());

    // An explicit operator move preserves the damaged record; it never restores its authority.
    let backup = root.path().join("damaged-grants.json");
    std::fs::rename(paths.phone_control_store(), &backup).unwrap();
    let recovered = prepare(paths.clone(), features).unwrap();
    let control = recovered.phone_control.as_ref().unwrap();
    let sockets = control
        .bind(
            &paths.phone_control_admin_socket(),
            &paths.phone_control_socket(),
            &paths.operator_capability(),
        )
        .await
        .unwrap();
    let admin = tokio::spawn(sockets.serve());
    let response = super::super::phone_control_admin(
        &paths,
        super::super::PhoneControl::Start {
            device_id: other.device_id,
        },
    )
    .await
    .unwrap();
    assert_eq!(response["type"], "enrollment_started");
    assert_eq!(std::fs::read(backup).unwrap(), damaged);
    admin.abort();
    assert!(admin.await.unwrap_err().is_cancelled());
}

#[test]
fn malformed_phone_grant_store_stays_sealed_and_untouched() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let mut features = attention_only();
    features.attention = false;
    features.receiver = true;
    features.phone_control = true;
    std::fs::create_dir_all(paths.phone_control_store().parent().unwrap()).unwrap();
    for damaged in [
        b"not json".as_slice(),
        br#"{"grants":{},"revisions":{"device":0}}"#.as_slice(),
    ] {
        std::fs::write(paths.phone_control_store(), damaged).unwrap();
        let prepared = prepare(paths.clone(), features).unwrap();
        assert!(!prepared.phone_control.unwrap().authority_available());
        assert_eq!(std::fs::read(paths.phone_control_store()).unwrap(), damaged);
    }
}

#[test]
fn disabling_runs_preserves_and_rejects_retained_obligations() {
    let root = tempfile::tempdir().unwrap();
    let paths = paths(root.path());
    let runs = RunStore::new(paths.runs()).unwrap();
    let id = "11111111-1111-4111-8111-111111111111";
    runs.admit(
        crate::RunAdmission {
            id: id.into(),
            generated_work_ceiling: 1,
            park_ttl_ms: 60000,
        },
        "test-token-at-least-sixteen-bytes",
    )
    .unwrap();
    let before = std::fs::read(paths.runs().join(format!("{id}.json"))).unwrap();
    let Err(error) = prepare(paths.clone(), attention_only()) else {
        panic!("retained Runs must block disabling")
    };
    assert!(error.to_string().contains("retained Runs"));
    assert_eq!(
        std::fs::read(paths.runs().join(format!("{id}.json"))).unwrap(),
        before
    );
    assert!(!paths.attention().exists());
}
