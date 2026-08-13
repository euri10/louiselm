use std::{
    sync::Arc,
    time::{SystemTime, UNIX_EPOCH},
};

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use louiselm_capture::{PairingRegistry, Receiver, Store};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tower::ServiceExt;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock")
        .as_millis() as u64
}

#[tokio::test]
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
    let receiver =
        Receiver::new(store.clone(), pairing, temporary.path().join("uploads")).expect("receiver");
    let app = receiver.router();

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
    assert_eq!(
        app.clone()
            .oneshot(request())
            .await
            .expect("retry")
            .status(),
        StatusCode::OK
    );
    assert_eq!(store.capture(&id).expect("capture").record.sha256, digest);
}

#[tokio::test]
async fn upload_rejects_missing_auth_and_digest_mismatch_without_a_capture() {
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
    let receiver =
        Receiver::new(store.clone(), pairing, temporary.path().join("uploads")).expect("receiver");
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
        app.oneshot(mismatch)
            .await
            .expect("digest mismatch")
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert!(store.capture(&id).is_err());
}

#[tokio::test]
async fn pairing_json_is_bounded_before_deserialization() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let store = Store::new(temporary.path().join("captures")).expect("store");
    let pairing =
        Arc::new(PairingRegistry::open(temporary.path().join("pairing")).expect("pairing"));
    let app = Receiver::new(store, pairing, temporary.path().join("uploads"))
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
