//! Paired-device push registration and durable delivery behavior.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert behavior."
)]

use std::sync::Arc;

use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use louiselm_capture::{
    AttentionStore, DeviceCredential, NotificationFailure, NotificationHealth, PairingRegistry,
    Receiver, Store,
};
use serde_json::json;
use tower::ServiceExt;

fn pair(registry: &PairingRegistry, name: &str) -> DeviceCredential {
    let offer = registry
        .issue("https://192.0.2.1:7391", &"a".repeat(64), 1_000, 60_000)
        .unwrap();
    registry.consume(&offer.token, name, 2_000).unwrap()
}

#[tokio::test]
async fn token_registration_is_authenticated_bounded_and_revocable() {
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
    let device = pair(&registry, "phone");
    let other = pair(&registry, "tablet");
    let app = Receiver::with_attention(
        Store::new(root.path().join("captures")).unwrap(),
        AttentionStore::new(
            root.path().join("attention"),
            Some(louiselm_capture::RunStore::new(root.path().join("runs")).unwrap()),
        )
        .unwrap(),
        registry.clone(),
        root.path().join("uploads"),
        &"a".repeat(64),
    )
    .unwrap()
    .router();
    let request = |credential: &str, body: serde_json::Value| {
        Request::put("/v1/attention/installation")
            .header("authorization", format!("Bearer {credential}"))
            .header("content-type", "application/json")
            .body(Body::from(body.to_string()))
            .unwrap()
    };
    let response = app
        .clone()
        .oneshot(request(
            &device.credential,
            json!({"fid": "cphonetoken00000000000"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    assert!(
        to_bytes(response.into_body(), 1024)
            .await
            .unwrap()
            .is_empty()
    );
    for body in [
        json!({"token": "old-fcm-token"}),
        json!({"fid": ""}),
        json!({"fid": "x".repeat(21)}),
        json!({"fid": "x".repeat(23)}),
        json!({"fid": "!".repeat(22)}),
        json!({"fid": "contains whitespace"}),
        json!({"fid": "x".repeat(4097)}),
        json!({"fid": "x".repeat(9000)}),
        json!({"fid": "cphonetoken00000000000", "device_id": other.device_id}),
    ] {
        let response = app
            .clone()
            .oneshot(request(&device.credential, body))
            .await
            .unwrap();
        assert!(response.status().is_client_error());
        let bytes = to_bytes(response.into_body(), 8192).await.unwrap();
        assert!(!String::from_utf8_lossy(&bytes).contains("cphonetoken00000000000"));
    }
    registry.revoke(&device.device_id).unwrap();
    let response = app
        .clone()
        .oneshot(request(
            &device.credential,
            json!({"fid": "crotatedtoken000000000"}),
        ))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let response = app
        .oneshot(
            Request::get("/v1/attention")
                .header("authorization", format!("Bearer {}", device.credential))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    let persisted = std::fs::read_to_string(root.path().join("pairing/pairing.json")).unwrap();
    assert!(!persisted.contains("cphonetoken00000000000"));
    assert!(!persisted.contains("crotatedtoken000000000"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(root.path().join("pairing/pairing.json"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn interrupted_submission_keeps_a_durable_delay_and_retries_only_latest_state() {
    let root = tempfile::tempdir().unwrap();
    let registry = PairingRegistry::open(root.path()).unwrap();
    let device = pair(&registry, "phone");
    registry
        .register_notification_installation(&device.credential, "cphonetoken00000000000")
        .unwrap();
    registry
        .configure_notifications(NotificationHealth::Ready)
        .unwrap();
    let interrupted = std::panic::catch_unwind(|| {
        registry.deliver_notifications(
            || Ok(Some(2)),
            3_000,
            |_, _| panic!("simulated interruption after remote effect"),
        )
    });
    assert!(interrupted.is_err());
    let restarted = PairingRegistry::open(root.path()).unwrap();
    restarted
        .deliver_notifications(|| Ok(Some(3)), 4_000, |_, _| panic!("retry too soon"))
        .unwrap();
    assert_eq!(
        restarted
            .deliver_notifications(
                || Ok(Some(5)),
                100_000,
                |_, generation| {
                    assert_eq!(generation, 5);
                    Ok(())
                }
            )
            .unwrap(),
        1
    );
}

#[test]
fn revocation_waits_for_current_submission_and_removes_all_future_authority() {
    use std::{sync::mpsc, thread, time::Duration};
    let root = tempfile::tempdir().unwrap();
    let registry = Arc::new(PairingRegistry::open(root.path()).unwrap());
    let device = pair(&registry, "phone");
    registry
        .register_notification_installation(&device.credential, "cphonetoken00000000000")
        .unwrap();
    registry
        .configure_notifications(NotificationHealth::Ready)
        .unwrap();
    let (entered, receive_entered) = mpsc::channel();
    let (release, receive_release) = mpsc::channel();
    let sender_registry = registry.clone();
    let delivery = thread::spawn(move || {
        sender_registry.deliver_notifications(
            || Ok(Some(1)),
            3_000,
            |_, _| {
                entered.send(()).unwrap();
                receive_release
                    .recv_timeout(Duration::from_secs(5))
                    .unwrap();
                Ok(())
            },
        )
    });
    receive_entered
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    let revoke_registry = registry.clone();
    let revoked_id = device.device_id.clone();
    let (revoked, receive_revoked) = mpsc::channel();
    let revocation = thread::spawn(move || {
        revoke_registry.revoke(&revoked_id).unwrap();
        revoked.send(()).unwrap();
    });
    assert!(
        receive_revoked
            .recv_timeout(Duration::from_millis(30))
            .is_err()
    );
    release.send(()).unwrap();
    assert_eq!(delivery.join().unwrap().unwrap(), 1);
    revocation.join().unwrap();
    receive_revoked
        .recv_timeout(Duration::from_secs(5))
        .unwrap();
    registry
        .deliver_notifications(|| Ok(Some(2)), 4_000, |_, _| panic!("revoked"))
        .unwrap();
    assert!(
        registry
            .register_notification_installation(&device.credential, "cnewtoken0000000000000")
            .is_err()
    );
    assert!(registry.notification_status().unwrap().devices.is_empty());
}

#[test]
fn malformed_persisted_registration_is_rejected_without_exposing_its_token() {
    let root = tempfile::tempdir().unwrap();
    let registry = PairingRegistry::open(root.path()).unwrap();
    let device = pair(&registry, "phone");
    registry
        .register_notification_installation(&device.credential, "cvalidtoken00000000000")
        .unwrap();
    let path = root.path().join("pairing.json");
    let mut state: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    state["devices"][0]["notification"]["fid"] = "bad secret token".into();
    std::fs::write(&path, state.to_string()).unwrap();
    match PairingRegistry::open(root.path()) {
        Ok(_) => panic!("malformed stored token accepted"),
        Err(error) => assert!(!error.to_string().contains("bad secret token")),
    }
}

#[test]
fn cli_status_and_explicit_retry_expose_only_safe_delivery_state() {
    let root = tempfile::tempdir().unwrap();
    let registry = PairingRegistry::open(root.path().join("louiselm/capture/pairing")).unwrap();
    let device = pair(&registry, "phone");
    registry
        .register_notification_installation(&device.credential, "cprivatetoken000000000")
        .unwrap();
    let command = |argument: &str| {
        std::process::Command::new(env!("CARGO_BIN_EXE_louiselm-capture"))
            .env_clear()
            .env("LOUISELM_CAPTURE_CONFIG_DIR", root.path())
            .env("LOUISELM_CAPTURE_STATE_DIR", root.path())
            .env("LOUISELM_CAPTURE_DATA_DIR", root.path())
            .arg(argument)
            .output()
            .unwrap()
    };
    assert!(!command("retry-notifications").status.success());
    registry
        .configure_notifications(NotificationHealth::ConfigurationError)
        .unwrap();
    let output = command("status");
    assert!(output.status.success());
    let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(status["notifications"]["schema_version"], 1);
    assert_eq!(status["notifications"]["health"], "configuration_error");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("cprivatetoken000000000"));
    assert!(!String::from_utf8_lossy(&output.stdout).contains(&device.credential));
    assert!(command("retry-notifications").status.success());
    assert_eq!(
        registry.notification_status().unwrap().health,
        NotificationHealth::Ready
    );
}

#[test]
fn each_device_refreshes_generation_and_observes_clears_before_submission() {
    use std::cell::Cell;
    let root = tempfile::tempdir().unwrap();
    let registry = PairingRegistry::open(root.path()).unwrap();
    for name in ["phone", "tablet"] {
        let device = pair(&registry, name);
        registry
            .register_notification_installation(&device.credential, &format!("{name:0<22}"))
            .unwrap();
    }
    registry
        .configure_notifications(NotificationHealth::Ready)
        .unwrap();
    let latest = Cell::new(Some(2));
    let mut generations = Vec::new();
    registry
        .deliver_notifications(
            || Ok(latest.get()),
            3_000,
            |_, generation| {
                generations.push(generation);
                latest.set(Some(7));
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(generations, [2, 7]);
    latest.set(Some(8));
    generations.clear();
    registry
        .deliver_notifications(
            || Ok(latest.get()),
            4_000,
            |_, generation| {
                generations.push(generation);
                latest.set(None);
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(generations, [8]);
}

#[test]
fn delivery_survives_restart_collapses_pending_generations_and_does_not_remind() {
    let root = tempfile::tempdir().unwrap();
    let registry = PairingRegistry::open(root.path()).unwrap();
    let phone = pair(&registry, "phone");
    let tablet = pair(&registry, "tablet");
    registry
        .register_notification_installation(&phone.credential, "cphonetoken00000000000")
        .unwrap();
    registry
        .register_notification_installation(&tablet.credential, "ctablettoken0000000000")
        .unwrap();
    assert_eq!(
        registry
            .deliver_notifications(|| Ok(Some(2)), 3_000, |_, _| panic!("unconfigured"))
            .unwrap(),
        0
    );
    registry
        .configure_notifications(NotificationHealth::Ready)
        .unwrap();
    let mut sent = Vec::new();
    registry
        .deliver_notifications(
            || Ok(Some(2)),
            3_000,
            |token, generation| {
                sent.push((token.to_owned(), generation));
                if token == "ctablettoken0000000000" {
                    Err(NotificationFailure::Transient {
                        retry_after_ms: 120_000,
                    })
                } else {
                    Ok(())
                }
            },
        )
        .unwrap();
    assert_eq!(sent.len(), 2);
    let restarted = PairingRegistry::open(root.path()).unwrap();
    restarted
        .configure_notifications(NotificationHealth::Ready)
        .unwrap();
    restarted
        .deliver_notifications(|| Ok(Some(2)), 4_000, |_, _| panic!("sent or backing off"))
        .unwrap();
    restarted
        .deliver_notifications(
            || Ok(None),
            200_000,
            |_, _| panic!("no eligible conditions"),
        )
        .unwrap();
    sent.clear();
    restarted
        .deliver_notifications(
            || Ok(Some(8)),
            200_000,
            |token, generation| {
                sent.push((token.to_owned(), generation));
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(sent.len(), 2);
    assert!(sent.iter().all(|(_, generation)| *generation == 8));
    restarted
        .register_notification_installation(&phone.credential, "cphonerotated000000000")
        .unwrap();
    restarted
        .deliver_notifications(
            || Ok(Some(8)),
            300_000,
            |_, _| panic!("rotation must not remind"),
        )
        .unwrap();
    restarted.revoke(&tablet.device_id).unwrap();
    sent.clear();
    restarted
        .deliver_notifications(
            || Ok(Some(9)),
            300_000,
            |token, generation| {
                sent.push((token.to_owned(), generation));
                Ok(())
            },
        )
        .unwrap();
    assert_eq!(sent, vec![("cphonerotated000000000".to_owned(), 9)]);
    let public = serde_json::to_string(&restarted.notification_status().unwrap()).unwrap();
    assert!(!public.contains("cphonerotated000000000"));
    assert!(!public.contains(&phone.credential));
}

#[test]
fn invalid_installations_disable_only_their_device_and_permanent_failures_stop_after_restart() {
    let root = tempfile::tempdir().unwrap();
    let registry = PairingRegistry::open(root.path()).unwrap();
    let phone = pair(&registry, "phone");
    let tablet = pair(&registry, "tablet");
    registry
        .register_notification_installation(&phone.credential, "cphonetoken00000000000")
        .unwrap();
    assert!(
        registry
            .register_notification_installation(&tablet.credential, "cphonetoken00000000000")
            .is_err()
    );
    registry
        .register_notification_installation(&tablet.credential, "ctablettoken0000000000")
        .unwrap();
    registry
        .configure_notifications(NotificationHealth::Ready)
        .unwrap();
    assert_eq!(
        registry
            .deliver_notifications(
                || Ok(Some(2)),
                3_000,
                |token, _| {
                    if token == "cphonetoken00000000000" {
                        Err(NotificationFailure::InvalidInstallation)
                    } else {
                        Ok(())
                    }
                }
            )
            .unwrap(),
        1
    );
    registry
        .deliver_notifications(
            || Ok(Some(3)),
            90_000,
            |token, _| {
                assert_eq!(token, "ctablettoken0000000000");
                Err(NotificationFailure::Authentication)
            },
        )
        .unwrap();
    let restarted = PairingRegistry::open(root.path()).unwrap();
    restarted
        .configure_notifications(NotificationHealth::Ready)
        .unwrap();
    assert_eq!(
        restarted.notification_status().unwrap().health,
        NotificationHealth::AuthenticationError
    );
    restarted
        .deliver_notifications(
            || Ok(Some(8)),
            500_000,
            |_, _| panic!("permanent failure survived restart"),
        )
        .unwrap();
    restarted.retry_notifications().unwrap();
    restarted
        .register_notification_installation(&phone.credential, "cnewphonetoken00000000")
        .unwrap();
    assert_eq!(
        restarted
            .deliver_notifications(|| Ok(Some(8)), 500_000, |_, _| Ok(()))
            .unwrap(),
        2
    );
}

#[test]
fn renewed_same_fid_reenables_delivery_without_reminding_confirmed_generations() {
    let root = tempfile::tempdir().unwrap();
    let registry = PairingRegistry::open(root.path()).unwrap();
    let phone = pair(&registry, "phone");
    let fid = "c123456789012345678901";
    registry
        .register_notification_installation(&phone.credential, fid)
        .unwrap();
    registry
        .configure_notifications(NotificationHealth::Ready)
        .unwrap();
    registry
        .deliver_notifications(|| Ok(Some(2)), 3_000, |_, _| Ok(()))
        .unwrap();
    registry
        .deliver_notifications(
            || Ok(Some(3)),
            4_000,
            |_, _| Err(NotificationFailure::InvalidInstallation),
        )
        .unwrap();
    assert!(!registry.notification_status().unwrap().devices[0].enabled);
    registry
        .register_notification_installation(&phone.credential, fid)
        .unwrap();
    assert!(registry.notification_status().unwrap().devices[0].enabled);
    assert_eq!(
        registry
            .deliver_notifications(|| Ok(Some(2)), 5_000, |_, _| panic!("reminder"))
            .unwrap(),
        0
    );
    assert_eq!(
        registry
            .deliver_notifications(
                || Ok(Some(3)),
                5_000,
                |target, _| {
                    assert_eq!(target, fid);
                    Ok(())
                }
            )
            .unwrap(),
        1
    );
}

#[test]
fn token_registry_upgrade_preserves_pairing_and_progress_but_never_sends_tokens() {
    let root = tempfile::tempdir().unwrap();
    let registry = PairingRegistry::open(root.path()).unwrap();
    let phone = pair(&registry, "phone");
    let path = root.path().join("pairing.json");
    let mut old: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    old["schema_version"] = 2.into();
    old["notifications"]["health"] = "ready".into();
    old["devices"][0]["notification"] = json!({
        "token": "old-private-fcm-token", "enabled": true,
        "submitted_generation": 7, "retry_at_ms": 0, "attempts": 0
    });
    std::fs::write(&path, old.to_string()).unwrap();
    let upgraded = PairingRegistry::open(root.path()).unwrap();
    assert_eq!(
        upgraded.authenticate_device(&phone.credential).unwrap(),
        Some(phone.device_id)
    );
    assert_eq!(
        upgraded
            .deliver_notifications(|| Ok(Some(8)), 5_000, |_, _| panic!("legacy token sent"))
            .unwrap(),
        0
    );
    let persisted = std::fs::read_to_string(&path).unwrap();
    assert!(!persisted.contains("old-private-fcm-token"));
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&persisted).unwrap()["schema_version"],
        3
    );
    upgraded
        .register_notification_installation(&phone.credential, "c123456789012345678901")
        .unwrap();
    let restarted = PairingRegistry::open(root.path()).unwrap();
    assert_eq!(
        restarted
            .deliver_notifications(|| Ok(Some(7)), 6_000, |_, _| panic!("upgrade reminder"))
            .unwrap(),
        0
    );
    assert_eq!(
        restarted
            .deliver_notifications(
                || Ok(Some(8)),
                6_000,
                |fid, _| {
                    assert_eq!(fid, "c123456789012345678901");
                    Ok(())
                }
            )
            .unwrap(),
        1
    );
}

#[test]
fn malformed_legacy_registry_is_rejected_without_rewrite_or_panic() {
    for devices in [json!([42]), json!([{"notification": {"token": 42}}])] {
        let root = tempfile::tempdir().unwrap();
        PairingRegistry::open(root.path()).unwrap();
        let path = root.path().join("pairing.json");
        let bytes = json!({"schema_version": 2, "pending": [], "devices": devices}).to_string();
        std::fs::write(&path, &bytes).unwrap();
        assert!(PairingRegistry::open(root.path()).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), bytes);
    }
}
