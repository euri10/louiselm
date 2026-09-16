//! Behavioral coverage for receiver.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use louiselm_capture::{
    AttentionCode, AttentionDraft, AttentionKind, AttentionStore, AttentionSubjectKind,
    CaptureDraft, CaptureSource, PairingRegistry, Receiver, Store,
};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis()
        .try_into()
        .expect("current timestamp fits u64")
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One end-to-end pairing/upload/retry scenario shares an authenticated device."
)]
async fn pairing_then_authenticated_upload_is_retry_safe() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path().join("captures")).expect("store");
    let pairing = Arc::new(PairingRegistry::open(temporary.path().join("state")).expect("pairing"));
    let offer = pairing
        .issue(
            "https://192.0.2.1:7391",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            now_ms(),
            60_000,
        )
        .expect("offer");
    let receiver = Receiver::new(
        store.clone(),
        pairing.clone(),
        temporary.path().join("uploads"),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .expect("receiver");
    let app = receiver.router();

    let health = app
        .clone()
        .oneshot(
            Request::get("/v1/health")
                .body(Body::empty())
                .expect("health request"),
        )
        .await
        .expect("health response");
    assert_eq!(health.status(), StatusCode::OK);
    let health: Value = serde_json::from_slice(
        &to_bytes(health.into_body(), 1024)
            .await
            .expect("health body"),
    )
    .expect("health JSON");
    assert_eq!(
        health,
        serde_json::json!({
            "status": "ok",
            "receiver_identity_sha256":
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        })
    );

    let response = app
        .clone()
        .oneshot(
            Request::post("/v1/pair")
                .header("content-type", "application/json")
                .body(Body::from(
                    serde_json::json!({"token": offer.token, "device_name": "garden-phone"})
                        .to_string(),
                ))
                .expect("pair request"),
        )
        .await
        .expect("pair response");
    assert_eq!(response.status(), StatusCode::OK);
    let paired: Value = serde_json::from_slice(
        &to_bytes(response.into_body(), 8 * 1024)
            .await
            .expect("pair body"),
    )
    .expect("pair JSON");
    let credential = paired["credential"].as_str().expect("credential");

    let id = uuid::Uuid::new_v4().to_string();
    let audio = b"garden idea";
    let digest = format!("{:x}", Sha256::digest(audio));
    let request = || {
        Request::put(format!("/v1/captures/{id}"))
            .header("authorization", format!("Bearer {credential}"))
            .header("content-type", "audio/mp4")
            .header("x-louiselm-source", "android")
            .header("x-louiselm-recorded-at-ms", "1765000000000")
            .header("x-louiselm-duration-ms", "4200")
            .header("x-louiselm-sha256", &digest)
            .body(Body::from(audio.as_slice()))
            .expect("upload request")
    };

    assert_eq!(
        app.clone()
            .oneshot(request())
            .await
            .expect("upload")
            .status(),
        StatusCode::CREATED
    );
    let first_delivery = pairing.status().expect("device status").devices[0]
        .last_delivery_at_ms
        .expect("first delivery");
    assert_eq!(
        app.clone()
            .oneshot(request())
            .await
            .expect("retry")
            .status(),
        StatusCode::OK
    );
    assert!(
        pairing.status().expect("retry status").devices[0]
            .last_delivery_at_ms
            .expect("retry delivery")
            >= first_delivery
    );
    assert_eq!(store.capture(&id).expect("capture").record.sha256, digest);
}

#[tokio::test]
async fn failed_uploads_do_not_record_device_delivery() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path().join("captures")).expect("store");
    let pairing = Arc::new(PairingRegistry::open(temporary.path().join("state")).expect("pairing"));
    let offer = pairing
        .issue(
            "https://192.0.2.1:7391",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            now_ms(),
            60_000,
        )
        .expect("offer");
    let credential = pairing
        .consume(&offer.token, "garden-phone", now_ms())
        .expect("pair")
        .credential;
    let receiver = Receiver::new(
        store.clone(),
        pairing.clone(),
        temporary.path().join("uploads"),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .expect("receiver");
    let app = receiver.router();
    let id = uuid::Uuid::new_v4().to_string();

    let unauthorized = Request::put(format!("/v1/captures/{id}"))
        .body(Body::from("speech"))
        .expect("request");
    assert_eq!(
        app.clone()
            .oneshot(unauthorized)
            .await
            .expect("unauthorized")
            .status(),
        StatusCode::UNAUTHORIZED
    );

    let mismatch = Request::put(format!("/v1/captures/{id}"))
        .header("authorization", format!("Bearer {credential}"))
        .header("content-type", "audio/mp4")
        .header("x-louiselm-source", "android")
        .header("x-louiselm-recorded-at-ms", "1765000000000")
        .header("x-louiselm-duration-ms", "4200")
        .header("x-louiselm-sha256", "0".repeat(64))
        .body(Body::from("speech"))
        .expect("request");
    assert_eq!(
        app.clone()
            .oneshot(mismatch)
            .await
            .expect("digest mismatch")
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        pairing.status().expect("device status").devices[0].last_delivery_at_ms,
        None
    );
    assert!(store.capture(&id).is_err());

    store
        .ingest(
            &CaptureDraft {
                id: id.clone(),
                source: CaptureSource::Android,
                recorded_at_ms: 1_765_000_000_000,
                duration_ms: 4_200,
                mime_type: "audio/mp4".to_owned(),
            },
            b"canonical".as_slice(),
        )
        .expect("canonical capture");
    let conflicting_audio = b"different";
    let conflict = Request::put(format!("/v1/captures/{id}"))
        .header("authorization", format!("Bearer {credential}"))
        .header("content-type", "audio/mp4")
        .header("x-louiselm-source", "android")
        .header("x-louiselm-recorded-at-ms", "1765000000000")
        .header("x-louiselm-duration-ms", "4200")
        .header(
            "x-louiselm-sha256",
            format!("{:x}", Sha256::digest(conflicting_audio)),
        )
        .body(Body::from(conflicting_audio.as_slice()))
        .expect("conflicting request");
    assert_eq!(
        app.oneshot(conflict)
            .await
            .expect("conflicting ingest")
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        pairing.status().expect("failed ingest status").devices[0].last_delivery_at_ms,
        None
    );
}

#[tokio::test]
async fn pairing_json_is_bounded_before_deserialization() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path().join("captures")).expect("store");
    let pairing =
        Arc::new(PairingRegistry::open(temporary.path().join("pairing")).expect("pairing"));
    let app = Receiver::new(
        store,
        pairing,
        temporary.path().join("uploads"),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .expect("receiver")
    .router();
    let request = Request::builder()
        .method("POST")
        .uri("/v1/pair")
        .header("content-type", "application/json")
        .body(Body::from("x".repeat(9 * 1024)))
        .expect("request");

    assert_eq!(
        app.oneshot(request).await.expect("response").status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One receiver conversation proves snapshot authentication, read-only access and revocation."
)]
async fn authenticated_attention_snapshot_is_read_only_and_revocable() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path().join("captures")).expect("store");
    let attention = AttentionStore::new(
        temporary.path().join("attention"),
        Some(louiselm_capture::RunStore::new(temporary.path().join("runs")).unwrap()),
    )
    .expect("attention");
    attention
        .upsert(AttentionDraft {
            subject_kind: AttentionSubjectKind::Run,
            subject_id: "11111111-2222-4333-8444-555555555555".to_owned(),
            kind: AttentionKind::SkillUnverified,
            source_operation_id: "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa".to_owned(),
            created_at_ms: 1_765_000_000_000,
            linked_run_id: None,
            stage: None,
            code: Some(AttentionCode::WitnessMissing),
        })
        .expect("attention");
    let pairing = Arc::new(PairingRegistry::open(temporary.path().join("state")).expect("pairing"));
    let offer = pairing
        .issue(
            "https://192.0.2.1:7391",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            now_ms(),
            60_000,
        )
        .expect("offer");
    let credential = pairing
        .consume(&offer.token, "phone", now_ms())
        .expect("pair")
        .credential;
    let receiver = Receiver::with_attention(
        store,
        attention,
        pairing.clone(),
        temporary.path().join("uploads"),
        "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
    )
    .expect("receiver");
    let app = receiver.router();

    let response = app
        .clone()
        .oneshot(
            Request::get("/v1/attention")
                .header("authorization", format!("Bearer {credential}"))
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response");
    assert_eq!(response.status(), StatusCode::OK);
    let snapshot: Value = serde_json::from_slice(
        &to_bytes(response.into_body(), 8 * 1024)
            .await
            .expect("body"),
    )
    .expect("snapshot JSON");
    assert_eq!(snapshot["generation"], 1);
    assert_eq!(snapshot["items"][0]["kind"], "skill_unverified");
    assert_eq!(snapshot["items"][0]["code"], "witness_missing");
    assert_eq!(snapshot["items"][0]["reason"], "Skill supply is unverified");
    assert!(snapshot["items"][0].get("arbitrary_text").is_none());

    assert_eq!(
        app.clone()
            .oneshot(
                Request::get("/v1/attention")
                    .body(Body::empty())
                    .expect("unauthorized request"),
            )
            .await
            .expect("unauthorized response")
            .status(),
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        app.clone()
            .oneshot(
                Request::post("/v1/attention")
                    .header("authorization", format!("Bearer {credential}"))
                    .body(Body::empty())
                    .expect("write request"),
            )
            .await
            .expect("write response")
            .status(),
        StatusCode::METHOD_NOT_ALLOWED
    );
    let device_id = pairing.status().expect("status").devices[0]
        .device_id
        .clone();
    pairing.revoke(&device_id).expect("revoke");
    assert_eq!(
        app.oneshot(
            Request::get("/v1/attention")
                .header("authorization", format!("Bearer {credential}"))
                .body(Body::empty())
                .expect("revoked request"),
        )
        .await
        .expect("revoked response")
        .status(),
        StatusCode::UNAUTHORIZED
    );
}
