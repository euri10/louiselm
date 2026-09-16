#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Tests use disposable signing keys and inspect fake HTTP exchanges."
)]

use aws_lc_rs::{
    encoding::{AsDer, Pkcs8V1Der},
    rsa::KeySize,
    signature::{KeyPair, RSA_PKCS1_2048_8192_SHA256, RsaKeyPair, UnparsedPublicKey},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};

use super::*;

fn credentials() -> (Value, RsaKeyPair) {
    let key = RsaKeyPair::generate(KeySize::Rsa2048).unwrap();
    let der: Pkcs8V1Der<'_> = key.as_der().unwrap();
    let pem = pem::encode(&pem::Pem::new("PRIVATE KEY", der.as_ref()));
    (
        json!({
            "type": "service_account", "project_id": "test-project", "private_key_id": "test-key",
            "private_key": pem, "client_email": "sender@test-project.iam.gserviceaccount.com",
            "token_uri": TOKEN_URL, "universe_domain": "googleapis.com"
        }),
        key,
    )
}

fn reply(status: StatusCode, body: &Value) -> Reply {
    Reply {
        status,
        retry_after: None,
        body: body.to_string().into_bytes(),
    }
}

fn accepted() -> Reply {
    reply(
        StatusCode::OK,
        &json!({"name": "projects/test-project/messages/opaque-id"}),
    )
}

#[test]
fn exact_oauth_and_fcm_requests_use_verified_assertions_and_short_lived_tokens() {
    let (document, key) = credentials();
    let mut sender =
        FcmSender::new(Credentials::parse(document.to_string().as_bytes()).unwrap()).unwrap();
    let mut calls = Vec::new();
    let now = 1_700_000_000_000;
    let mut transport = |request: Request| {
        assert_eq!(request.method(), reqwest::Method::POST);
        calls.push(request.url().as_str().to_owned());
        let bytes = request.body().unwrap().as_bytes().unwrap();
        if request.url().as_str() == TOKEN_URL {
            assert_eq!(
                request.headers()[header::CONTENT_TYPE],
                "application/x-www-form-urlencoded"
            );
            assert!(request.headers().get(header::AUTHORIZATION).is_none());
            let form = url::form_urlencoded::parse(bytes)
                .into_owned()
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(form.len(), 2);
            assert_eq!(
                form["grant_type"],
                "urn:ietf:params:oauth:grant-type:jwt-bearer"
            );
            let jwt = &form["assertion"];
            let parts = jwt.split('.').collect::<Vec<_>>();
            assert_eq!(parts.len(), 3);
            let header: Value =
                serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
            assert_eq!(
                header,
                json!({"alg": "RS256", "typ": "JWT", "kid": "test-key"})
            );
            let claims: Value =
                serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
            assert_eq!(claims["iss"], "sender@test-project.iam.gserviceaccount.com");
            assert_eq!(claims["scope"], auth::SCOPE);
            assert_eq!(claims["aud"], TOKEN_URL);
            assert_eq!(
                claims["exp"].as_u64().unwrap() - claims["iat"].as_u64().unwrap(),
                3600
            );
            UnparsedPublicKey::new(&RSA_PKCS1_2048_8192_SHA256, key.public_key())
                .verify(
                    format!("{}.{}", parts[0], parts[1]).as_bytes(),
                    &URL_SAFE_NO_PAD.decode(parts[2]).unwrap(),
                )
                .unwrap();
            Ok(reply(
                StatusCode::OK,
                &json!({"access_token": "ephemeral-token", "token_type": "Bearer", "expires_in": 3600}),
            ))
        } else {
            assert_eq!(
                request.url().as_str(),
                "https://fcm.googleapis.com/v1/projects/test-project/messages:send"
            );
            assert_eq!(
                request.headers()[header::AUTHORIZATION],
                "Bearer ephemeral-token"
            );
            let body: Value = serde_json::from_slice(bytes).unwrap();
            assert_eq!(
                body,
                json!({"message": {
                    "token": "device-token",
                    "data": {"generation": "42"},
                    "android": {"collapse_key": "louiselm-attention", "priority": "high"}
                }})
            );
            Ok(accepted())
        }
    };
    sender
        .send_with("device-token", 42, now, &mut transport)
        .unwrap();
    sender
        .send_with("device-token", 42, now + 1_000, &mut transport)
        .unwrap();
    sender
        .send_with("device-token", 42, now + 3_550_000, &mut transport)
        .unwrap();
    assert_eq!(
        calls.iter().filter(|url| url.as_str() == TOKEN_URL).count(),
        2
    );
    assert_eq!(calls.len(), 5);
}

#[test]
fn credentials_refuse_alternate_endpoints_bad_keys_and_unsafe_file_permissions() {
    let (document, _) = credentials();
    for (field, value) in [
        ("type", "authorized_user"),
        ("project_id", "../other"),
        ("token_uri", "https://attacker.invalid/token"),
        ("private_key", "malformed secret"),
        ("universe_domain", "attacker.invalid"),
        ("client_email", "not-an-email"),
    ] {
        let mut invalid = document.clone();
        invalid[field] = value.into();
        assert!(matches!(
            Credentials::parse(invalid.to_string().as_bytes()),
            Err(NotificationFailure::Configuration)
        ));
    }
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("credentials.json");
    std::fs::write(&path, document.to_string()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Credentials::load(&path).is_err());
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    assert!(Credentials::load(&path).is_ok());
    std::fs::write(&path, vec![b'x'; 65537]).unwrap();
    assert!(Credentials::load(&path).is_err());
}

#[test]
fn failure_classes_use_typed_fcm_details_and_honor_retry_after() {
    let now = 1_700_000_000_000;
    let invalid = reply(
        StatusCode::NOT_FOUND,
        &json!({"error": {"message": "hostile secret text", "details": [{
            "@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError", "errorCode": "UNREGISTERED"
        }]}}),
    );
    assert_eq!(
        check_reply(&invalid, now, false),
        Err(NotificationFailure::InvalidToken)
    );
    assert_eq!(
        check_reply(&reply(StatusCode::NOT_FOUND, &json!({})), now, false),
        Err(NotificationFailure::Configuration)
    );
    let bad_request = reply(
        StatusCode::BAD_REQUEST,
        &json!({"error": {"details": [{"@type": "type.googleapis.com/google.rpc.BadRequest"}]}}),
    );
    assert_eq!(
        check_reply(&bad_request, now, false),
        Err(NotificationFailure::Configuration)
    );
    for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
        assert_eq!(
            check_reply(&reply(status, &json!({})), now, false),
            Err(NotificationFailure::Authentication)
        );
    }
    for status in [
        StatusCode::TOO_MANY_REQUESTS,
        StatusCode::SERVICE_UNAVAILABLE,
        StatusCode::REQUEST_TIMEOUT,
    ] {
        let mut failed = reply(status, &json!({"secret": "never in diagnostics"}));
        failed.retry_after = Some("120".to_owned());
        assert_eq!(
            check_reply(&failed, now, false),
            Err(NotificationFailure::Transient {
                retry_after_ms: 120_000
            })
        );
        failed.retry_after = Some(httpdate::fmt_http_date(
            UNIX_EPOCH + Duration::from_millis(now + 180_000),
        ));
        assert_eq!(
            check_reply(&failed, now, true),
            Err(NotificationFailure::Transient {
                retry_after_ms: 180_000
            })
        );
    }
    assert_eq!(
        check_reply(
            &reply(StatusCode::BAD_REQUEST, &json!({"error": "invalid_grant"})),
            now,
            true
        ),
        Err(NotificationFailure::Authentication)
    );
}

#[test]
fn oauth_failure_never_sends_a_notification_and_bad_tokens_are_not_cached() {
    let (document, _) = credentials();
    let mut sender =
        FcmSender::new(Credentials::parse(document.to_string().as_bytes()).unwrap()).unwrap();
    for body in [
        json!({"access_token": "secret", "token_type": "Basic", "expires_in": 3600}),
        json!({"access_token": "bad\nheader", "token_type": "Bearer", "expires_in": 3600}),
        json!({"access_token": "secret", "token_type": "Bearer", "expires_in": 7200}),
    ] {
        let result = sender.send_with("token", 1, 1_700_000_000_000, |request| {
            assert_eq!(request.url().as_str(), TOKEN_URL);
            Ok(reply(StatusCode::OK, &body))
        });
        assert_eq!(result, Err(NotificationFailure::Configuration));
        assert!(sender.access.is_none());
    }
}
