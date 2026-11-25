//! Desktop recovery of damaged per-phone project confirmations.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::{path::Path, sync::Arc, time::Instant};

use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    net::UnixStream,
};
use tower::ServiceExt;

use super::{
    Service,
    authority::{Refusal, Scope},
};
use crate::{PairingRegistry, Receiver, Store, hex::lower_hex};

async fn exchange(path: &Path, message: &Value) -> Value {
    let mut stream = UnixStream::connect(path).await.unwrap();
    stream
        .write_all(message.to_string().as_bytes())
        .await
        .unwrap();
    stream.write_all(b"\n").await.unwrap();
    let mut line = String::new();
    BufReader::new(stream).read_line(&mut line).await.unwrap();
    serde_json::from_str(&line).unwrap()
}

#[tokio::test]
#[expect(
    clippy::too_many_lines,
    reason = "One paired grant exercises corrupted confirmation recovery and its security boundaries."
)]
async fn desktop_recovers_only_an_explicit_project_from_damaged_confirmations() {
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    let project = workspace.join("project");
    let second_project = workspace.join("second");
    let foreign = root.path().join("foreign");
    for directory in [&project, &second_project, &foreign] {
        std::fs::create_dir_all(directory).unwrap();
    }
    let identity = "01".repeat(32);
    let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
    let offer = pairing
        .issue("https://localhost:7391", &identity, 1_000, 10_000)
        .unwrap();
    let phone = pairing.consume(&offer.token, "phone", 1_001).unwrap();
    let grants = root.path().join("phone/grants.json");
    let service = Service::open(&grants, &identity, pairing.clone()).unwrap();
    let authority = &service.authority;
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../android/app/src/test/resources/phone-control-v1.json"
    ))
    .unwrap();
    let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
    let now = Instant::now();
    let offer = authority.start(&phone.device_id, now, 1_000).unwrap();
    authority
        .submit(
            &phone.device_id,
            &offer.enrollment_id,
            &offer.matching_code,
            key,
            now,
        )
        .unwrap();
    let scope = Scope {
        projects: vec![workspace.to_str().unwrap().into()],
        agents: vec!["codex".into()],
        groups: vec!["read".into()],
    };
    let grant = authority
        .approve(&phone.device_id, &offer.enrollment_id, key, scope, &[], now)
        .unwrap();
    let offer = pairing
        .issue("https://localhost:7391", &identity, 1_000, 10_000)
        .unwrap();
    let other = pairing.consume(&offer.token, "other", 1_001).unwrap();
    let offer = pairing
        .issue("https://localhost:7391", &identity, 1_000, 10_000)
        .unwrap();
    let untouched = pairing.consume(&offer.token, "untouched", 1_001).unwrap();
    for device in [&other.device_id, &untouched.device_id] {
        let offer = authority.start(device, now, 1_000).unwrap();
        authority
            .submit(device, &offer.enrollment_id, &offer.matching_code, key, now)
            .unwrap();
        authority
            .approve(
                device,
                &offer.enrollment_id,
                key,
                grant.scope.clone(),
                &grant.project_root_ids,
                now,
            )
            .unwrap();
    }
    let project_id = authority
        .register_project(project.to_str().unwrap(), &grant.project_root_ids)
        .unwrap();
    let second_id = authority
        .register_project(second_project.to_str().unwrap(), &grant.project_root_ids)
        .unwrap();
    let foreign_root = authority.register_root(foreign.to_str().unwrap()).unwrap();
    let foreign_id = authority
        .register_project(foreign.to_str().unwrap(), &[foreign_root])
        .unwrap();
    drop(service);

    let mut stored: Value = serde_json::from_slice(&std::fs::read(&grants).unwrap()).unwrap();
    let legacy = stored["grants"][&phone.device_id].as_object_mut().unwrap();
    legacy.remove("project_root_ids");
    legacy.remove("project_roots_sha256");
    legacy.insert(
        "legacy_scope_approval_sha256".into(),
        legacy["grant_scope_sha256"].clone(),
    );
    legacy.insert("confirmed_project_ids".into(), json!([foreign_id]));
    legacy.insert("legacy_confirmations_sha256".into(), json!("ff".repeat(32)));
    let damaged = serde_json::to_vec_pretty(&stored).unwrap();
    std::fs::write(&grants, &damaged).unwrap();

    let service = Service::open(&grants, &identity, pairing.clone()).unwrap();
    let app = Receiver::new(
        Store::new(root.path().join("captures")).unwrap(),
        pairing.clone(),
        root.path().join("uploads"),
        &identity,
    )
    .unwrap()
    .with_phone_control(service.router())
    .router();
    let admin_path = root.path().join("admin.sock");
    let capability = root.path().join("capability");
    let sockets = service
        .bind(
            &admin_path,
            &root.path().join("controller.sock"),
            &capability,
        )
        .await
        .unwrap();
    let token = std::fs::read_to_string(capability).unwrap();
    let server = tokio::spawn(sockets.serve());
    let review = exchange(
        &admin_path,
        &json!({"v":2,"type":"inspect_grant","capability":token,"device_id":phone.device_id}),
    )
    .await;
    assert_eq!(review["scope_approved"], true);
    assert_eq!(review["project_confirmations_valid"], false);
    let challenge = Request::post("/v1/phone-control/challenge")
        .header("authorization", format!("Bearer {}", phone.credential))
        .header("content-type", "application/json")
        .body(Body::from(json!({"v":1,"type":"challenge","grant_id":review["grant_id"],"grant_revision":review["grant_revision"]}).to_string())).unwrap();
    assert_eq!(
        app.clone().oneshot(challenge).await.unwrap().status(),
        StatusCode::OK
    );
    let audio = b"capture remains available";
    let upload = Request::put(format!("/v1/captures/{}", uuid::Uuid::new_v4()))
        .header("authorization", format!("Bearer {}", phone.credential))
        .header("content-type", "audio/mp4")
        .header("x-louiselm-source", "android")
        .header("x-louiselm-recorded-at-ms", "1765000000000")
        .header("x-louiselm-duration-ms", "4200")
        .header("x-louiselm-sha256", lower_hex(&Sha256::digest(audio)))
        .body(Body::from(audio.as_slice()))
        .unwrap();
    assert_eq!(
        app.oneshot(upload).await.unwrap().status(),
        StatusCode::CREATED
    );

    let mut request = json!({"v":1,"type":"confirm_legacy_project","capability":token,
        "device_id":phone.device_id,"grant_id":review["grant_id"],"grant_revision":review["grant_revision"],
        "scope_digest":review["scope_digest"],"project_id":project_id});
    assert_eq!(exchange(&admin_path, &request).await["code"], "unavailable");
    assert_eq!(std::fs::read(&grants).unwrap(), damaged);
    request["type"] = json!("recover_legacy_project");
    let mut invalid = request.clone();
    invalid["capability"] = json!("wrong");
    assert_eq!(
        exchange(&admin_path, &invalid).await["code"],
        "unauthenticated"
    );
    invalid = request.clone();
    invalid["v"] = json!(2);
    assert_eq!(exchange(&admin_path, &invalid).await["code"], "unsupported");
    invalid = request.clone();
    invalid["unexpected"] = json!(true);
    assert_eq!(exchange(&admin_path, &invalid).await["code"], "malformed");
    invalid = request.clone();
    invalid["scope_digest"] = json!("00".repeat(32));
    assert_eq!(
        exchange(&admin_path, &invalid).await["code"],
        "stale_subject"
    );
    invalid = request.clone();
    invalid["grant_id"] = json!("00".repeat(16));
    assert_eq!(
        exchange(&admin_path, &invalid).await["code"],
        "stale_subject"
    );
    invalid = request.clone();
    invalid["grant_revision"] = json!(2);
    assert_eq!(
        exchange(&admin_path, &invalid).await["code"],
        "stale_subject"
    );
    invalid = request.clone();
    invalid["project_id"] = json!(foreign_id);
    assert_eq!(
        exchange(&admin_path, &invalid).await["code"],
        "out_of_scope"
    );
    assert_eq!(std::fs::read(&grants).unwrap(), damaged);

    // A failed snapshot must refuse the unrelated write while leaving authority usable.
    let displaced = root.path().join("temporarily-displaced-grants");
    std::fs::rename(&grants, &displaced).unwrap();
    assert_eq!(
        service.authority.disable(&other.device_id),
        Err(Refusal::Unavailable)
    );
    assert_eq!(std::fs::read(&displaced).unwrap(), damaged);
    std::fs::rename(&displaced, &grants).unwrap();
    assert!(
        service
            .authority
            .challenge(
                &phone.device_id,
                review["grant_id"].as_str().unwrap(),
                review["grant_revision"].as_u64().unwrap(),
                now,
                2_000,
            )
            .is_ok()
    );
    service.authority.disable(&other.device_id).unwrap();
    let snapshot = std::fs::read_dir(grants.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .find(|entry| {
            entry.path().is_file()
                && std::fs::read(entry.path()).is_ok_and(|bytes| bytes == damaged)
        })
        .expect("unrelated grant changes must retain the original damaged bytes");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            snapshot.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let response = exchange(&admin_path, &request).await;
    assert_eq!(response["type"], "legacy_project_recovered");
    let after: Value = serde_json::from_slice(&std::fs::read(&grants).unwrap()).unwrap();
    assert_eq!(
        after["grants"][&phone.device_id]["confirmed_project_ids"],
        json!([project_id])
    );
    assert_eq!(
        after["grants"][&phone.device_id]["scope"],
        stored["grants"][&phone.device_id]["scope"]
    );
    assert_eq!(
        after["grants"][&untouched.device_id],
        stored["grants"][&untouched.device_id]
    );
    assert_eq!(exchange(&admin_path, &request).await["code"], "unsupported");
    request["type"] = json!("confirm_legacy_project");
    request["project_id"] = json!(second_id);
    assert_eq!(
        exchange(&admin_path, &request).await["type"],
        "legacy_project_confirmed"
    );
    request["type"] = json!("recover_legacy_project");
    assert_eq!(exchange(&admin_path, &request).await["code"], "unsupported");
    server.abort();
    assert!(server.await.unwrap_err().is_cancelled());
    drop(service);

    let reopened = Service::open(&grants, &identity, pairing.clone()).unwrap();
    let review_after = reopened
        .authority
        .inspect_grant(&pairing, &phone.device_id)
        .unwrap();
    assert!(review_after.scope_approved && review_after.project_confirmations_valid);
    assert_eq!(
        review_after.scope_digest,
        review["scope_digest"].as_str().unwrap()
    );
    reopened.authority.disable(&phone.device_id).unwrap();
    assert_eq!(
        reopened.authority.confirm_legacy_project(
            &pairing,
            &phone.device_id,
            review["grant_id"].as_str().unwrap(),
            review["grant_revision"].as_u64().unwrap(),
            review["scope_digest"].as_str().unwrap(),
            &project_id,
            true,
        ),
        Err(Refusal::Revoked)
    );
    assert!(
        pairing
            .authenticate_device(&phone.credential)
            .unwrap()
            .is_some()
    );
    pairing.revoke(&untouched.device_id).unwrap();
    assert_eq!(
        reopened.authority.confirm_legacy_project(
            &pairing,
            &untouched.device_id,
            review["grant_id"].as_str().unwrap(),
            review["grant_revision"].as_u64().unwrap(),
            review["scope_digest"].as_str().unwrap(),
            &project_id,
            true,
        ),
        Err(Refusal::Revoked)
    );
}
