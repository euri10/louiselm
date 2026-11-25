//! Private HTTP wire boundary for phone-control requests.

use std::{sync::Arc, time::Instant};

use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State, rejection::BytesRejection},
    http::{HeaderMap, StatusCode, Uri, header},
    routing::post,
};
use serde::{Deserialize, de::DeserializeOwned};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{
    ActionEvidence, ActionEvidenceError, ActionEvidenceStore, ActionOutcome, MAX_FRAME,
    MAX_INTEGER, Proof, Request, fixed, valid_string,
};
use super::{
    authority::{Authority, EnrollmentStatus, History, Refusal},
    socket::ControllerRegistry,
};
use crate::{PairingRegistry, hex::lower_hex, time::now_ms};

#[derive(Clone)]
struct HttpState {
    authority: Arc<Authority>,
    pairing: Arc<PairingRegistry>,
    controllers: Arc<ControllerRegistry>,
    evidence: Arc<Result<ActionEvidenceStore, ActionEvidenceError>>,
}

pub(super) fn router(
    authority: Arc<Authority>,
    pairing: Arc<PairingRegistry>,
    controllers: Arc<ControllerRegistry>,
    evidence: Arc<Result<ActionEvidenceStore, ActionEvidenceError>>,
) -> Router {
    Router::new()
        .route("/v1/phone-control/enrollment/offer", post(handle))
        .route("/v1/phone-control/enrollment/submit", post(handle))
        .route("/v1/phone-control/enrollment/status", post(handle))
        .route("/v1/phone-control/challenge", post(handle))
        .route("/v1/phone-control/unlock", post(handle))
        .route("/v1/phone-control/request", post(handle))
        .layer(DefaultBodyLimit::max(MAX_FRAME))
        .with_state(HttpState {
            authority,
            pairing,
            controllers,
            evidence,
        })
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {
    #[serde(rename = "v")]
    _version: u64,
    #[serde(rename = "type")]
    _kind: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Submit {
    #[serde(rename = "v")]
    _version: u64,
    #[serde(rename = "type")]
    _kind: String,
    enrollment_id: String,
    matching_code: String,
    enrolled_key: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Status {
    #[serde(rename = "v")]
    _version: u64,
    #[serde(rename = "type")]
    _kind: String,
    enrollment_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ChallengeRequest {
    #[serde(rename = "v")]
    _version: u64,
    #[serde(rename = "type")]
    _kind: String,
    grant_id: String,
    grant_revision: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnlockRequest {
    #[serde(rename = "v")]
    _version: u64,
    #[serde(rename = "type")]
    _kind: String,
    proof: Proof,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SignedRequest {
    #[serde(rename = "v")]
    _version: u64,
    #[serde(rename = "type")]
    _kind: String,
    request: Request,
}

fn decode<T: DeserializeOwned>(body: &[u8], kind: &str) -> Result<T, Refusal> {
    let value: Value = serde_json::from_slice(body).map_err(|_| Refusal::Malformed)?;
    match value.get("v").and_then(Value::as_u64) {
        Some(version) if version > MAX_INTEGER => return Err(Refusal::Malformed),
        Some(1) => {}
        Some(_) => return Err(Refusal::Unsupported),
        None => return Err(Refusal::Malformed),
    }
    match value.get("type").and_then(Value::as_str) {
        Some(actual) if !valid_string(actual) => return Err(Refusal::Malformed),
        Some(actual) if actual == kind => {}
        Some(_) => return Err(Refusal::Unsupported),
        None => return Err(Refusal::Malformed),
    }
    serde_json::from_slice(body).map_err(|_| Refusal::Malformed)
}

async fn handle(
    State(state): State<HttpState>,
    uri: Uri,
    headers: HeaderMap,
    body: Result<Bytes, BytesRejection>,
) -> (StatusCode, Json<Value>) {
    let credential = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .filter(|value| !value.is_empty())
        .map(str::to_owned);
    let Some(credential) = credential else {
        return outcome(Err(Refusal::Unauthenticated));
    };
    let body = match body {
        Ok(body) if body.len() <= MAX_FRAME => body,
        _ => return outcome(Err(Refusal::Malformed)),
    };
    let path = uri.path().to_owned();
    let result = tokio::task::spawn_blocking(move || {
        let device = state
            .pairing
            .authenticate_device(&credential)
            .map_err(|_| Refusal::Unavailable)?
            .ok_or(Refusal::Unauthenticated)?;
        process(
            &state.authority,
            &state.controllers,
            &state.evidence,
            &device,
            &path,
            &body,
        )
    })
    .await
    .unwrap_or(Err(Refusal::Unavailable));
    outcome(result)
}

fn process(
    authority: &Authority,
    controllers: &ControllerRegistry,
    evidence: &Result<ActionEvidenceStore, ActionEvidenceError>,
    device: &str,
    path: &str,
    body: &[u8],
) -> Result<Value, Refusal> {
    match path {
        "/v1/phone-control/enrollment/offer" => {
            let _: Empty = decode(body, "enrollment_offer")?;
            let EnrollmentStatus::Pending { offer, .. } =
                authority.enrollment(device, None, Instant::now())?
            else {
                return Err(Refusal::Unauthenticated);
            };
            Ok(
                json!({"v":1, "type":"enrollment_offer", "enrollment_id":offer.enrollment_id,
                "matching_code":offer.matching_code, "receiver_identity":authority.receiver_identity(),
                "expires_at_ms":offer.expires_at_ms}),
            )
        }
        "/v1/phone-control/enrollment/submit" => {
            let request: Submit = decode(body, "enrollment_submit")?;
            fixed::<16>(&request.enrollment_id).map_err(|_| Refusal::Malformed)?;
            authority.submit(
                device,
                &request.enrollment_id,
                &request.matching_code,
                &request.enrolled_key,
                Instant::now(),
            )?;
            Ok(json!({"v":1, "type":"enrollment_pending", "enrollment_id":request.enrollment_id}))
        }
        "/v1/phone-control/enrollment/status" => {
            let request: Status = decode(body, "enrollment_status")?;
            fixed::<16>(&request.enrollment_id).map_err(|_| Refusal::Malformed)?;
            match authority.enrollment(device, Some(&request.enrollment_id), Instant::now())? {
                EnrollmentStatus::Pending { .. } => Ok(
                    json!({"v":1,"type":"enrollment_pending", "enrollment_id":request.enrollment_id}),
                ),
                EnrollmentStatus::Granted(grant) => {
                    let key = fixed::<65>(&grant.enrolled_key).map_err(|_| Refusal::Unavailable)?;
                    Ok(
                        json!({"v":1,"type":"enrollment_granted", "enrollment_id":grant.enrollment_id,
                        "grant_id":grant.grant_id, "grant_revision":grant.grant_revision,
                        "enrolled_key_hash":lower_hex(&Sha256::digest(key)), "receiver_identity":grant.receiver_identity}),
                    )
                }
            }
        }
        "/v1/phone-control/challenge" => {
            let request: ChallengeRequest = decode(body, "challenge")?;
            fixed::<16>(&request.grant_id).map_err(|_| Refusal::Malformed)?;
            if request.grant_revision == 0 || request.grant_revision > MAX_INTEGER {
                return Err(Refusal::Malformed);
            }
            let challenge = authority.challenge(
                device,
                &request.grant_id,
                request.grant_revision,
                Instant::now(),
                now_ms(),
            )?;
            Ok(
                json!({"v":1,"type":"challenge", "grant_id":challenge.grant_id,
                "grant_revision":challenge.grant_revision, "receiver_identity":challenge.receiver_identity,
                "challenge_id":challenge.challenge_id, "challenge":challenge.challenge, "expires_at_ms":challenge.expires_at_ms}),
            )
        }
        "/v1/phone-control/unlock" => {
            let request: UnlockRequest = decode(body, "unlock")?;
            let lease = authority.unlock(device, &request.proof, Instant::now(), now_ms())?;
            Ok(
                json!({"v":1,"type":"unlocked", "grant_id":request.proof.grant_id,
                "grant_revision":request.proof.grant_revision, "period_id":lease.period_id,
                "idle_expires_at_ms":lease.idle_expires_at_ms, "absolute_expires_at_ms":lease.absolute_expires_at_ms}),
            )
        }
        "/v1/phone-control/request" => {
            let request: SignedRequest = decode(body, "request")?;
            if request.request.action == 2 {
                let store = evidence.as_ref().map_err(|_| Refusal::Unavailable)?;
                let record =
                    authority.read_history(store, device, &request.request, Instant::now())?;
                return Ok(history_response(&record, &request.request.operation_id));
            }
            let permit = authority.begin_read(device, &request.request, Instant::now())?;
            if !controllers.contains(&request.request.controller_id) {
                return match authority.finish_read(&permit, false, Instant::now(), now_ms()) {
                    Err(Refusal::Unavailable) => Err(Refusal::StaleSubject),
                    Err(error) => Err(error),
                    Ok(()) => Err(Refusal::Unavailable),
                };
            }
            // The integration slice owns dispatch to a real controller; no
            // protected response is delivered without that owner.
            authority.finish_read(&permit, false, Instant::now(), now_ms())?;
            Err(Refusal::Unavailable)
        }
        _ => Err(Refusal::Unsupported),
    }
}

fn history_response(history: &History, request_id: &str) -> Value {
    match history {
        History::Record(record) => json!({"v":1,"type":"action_history",
            "request_id":request_id,"record":history_record(record)}),
        History::Page { records, next } => json!({"v":1,"type":"action_history_page",
            "request_id":request_id,"records":records.iter().map(history_record).collect::<Vec<_>>(),
            "next_after":next.as_deref().unwrap_or("")}),
    }
}

fn history_record(record: &ActionEvidence) -> Value {
    let (state, evidence) = match &record.outcome {
        ActionOutcome::Unknown => ("unknown", ""),
        ActionOutcome::Accepted { evidence } => ("accepted", evidence.as_str()),
        ActionOutcome::Rejected { evidence } => ("rejected", evidence.as_str()),
        ActionOutcome::FencedNoEffect { evidence } => ("fenced_no_effect", evidence.as_str()),
    };
    let identity = &record.identity;
    json!({"operation_id":identity.operation_id,
        "receiver_identity":identity.receiver_identity,"controller_id":identity.controller_id,
        "subject_id":identity.subject_id,"action":identity.action,"payload_hash":identity.payload_hash,
        "grant_id":identity.grant_id,"grant_revision":identity.grant_revision,
        "expected_revision":identity.expected_revision,"recorded_at_ms":identity.recorded_at_ms,
        "updated_at_ms":record.updated_at_ms,"state":state,"evidence":evidence})
}

fn outcome(result: Result<Value, Refusal>) -> (StatusCode, Json<Value>) {
    match result {
        Ok(value) => (StatusCode::OK, Json(value)),
        Err(error) => {
            let (status, code) = match error {
                Refusal::Unauthenticated => (StatusCode::UNAUTHORIZED, "unauthenticated"),
                Refusal::Expired => (StatusCode::GONE, "expired"),
                Refusal::Revoked => (StatusCode::FORBIDDEN, "revoked"),
                Refusal::OutOfScope => (StatusCode::FORBIDDEN, "out_of_scope"),
                Refusal::StaleSubject => (StatusCode::GONE, "stale_subject"),
                Refusal::Malformed => (StatusCode::BAD_REQUEST, "malformed"),
                Refusal::Unsupported => (StatusCode::BAD_REQUEST, "unsupported"),
                Refusal::Unavailable => (StatusCode::SERVICE_UNAVAILABLE, "unavailable"),
            };
            (status, Json(json!({"v":1,"type":"error","code":code})))
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    clippy::panic,
    clippy::unwrap_used,
    reason = "test fixtures and assertions"
)]
mod tests {
    use std::{sync::Arc, time::Instant};

    use aws_lc_rs::{
        rand::SystemRandom,
        signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair},
    };
    use axum::{
        body::Body,
        http::{Request, StatusCode},
    };
    use http_body_util::BodyExt;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use tower::ServiceExt;

    use super::super::{
        ActionIdentity, ActionOutcome, Service,
        authority::{Authority, Scope, StoredGrant},
        fixed, prefix,
    };
    use crate::{PairingRegistry, hex::lower_hex};

    fn unhex(text: &str) -> Vec<u8> {
        text.as_bytes()
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    fn signed_unlock(fixture: &Value, challenge: &Value) -> Value {
        let field = |name: &str| challenge[name].as_str().unwrap();
        let enrolled_hash = fixture["proof"]["enrolled_key_hash"].as_str().unwrap();
        let working_key = fixture["keys"]["working"]["sec1"].as_str().unwrap();
        let revision = challenge["grant_revision"].as_u64().unwrap();
        let mut bytes = prefix(1);
        bytes.extend(fixed::<16>(field("grant_id")).unwrap());
        bytes.extend(revision.to_be_bytes());
        bytes.extend(fixed::<32>(enrolled_hash).unwrap());
        bytes.extend(fixed::<32>(field("receiver_identity")).unwrap());
        bytes.extend(fixed::<16>(field("challenge_id")).unwrap());
        bytes.extend(fixed::<32>(field("challenge")).unwrap());
        bytes.extend(fixed::<65>(working_key).unwrap());
        let key = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_ASN1_SIGNING,
            &unhex(fixture["keys"]["enrolled"]["pkcs8"].as_str().unwrap()),
        )
        .unwrap();
        let signature = key.sign(&SystemRandom::new(), &bytes).unwrap();
        json!({"v":1,"type":"unlock","proof":{
            "grant_id":field("grant_id"),"grant_revision":revision,"enrolled_key_hash":enrolled_hash,
            "receiver_identity":field("receiver_identity"),"challenge_id":field("challenge_id"),
            "challenge":field("challenge"),"working_key":working_key,"signature":lower_hex(signature.as_ref())
        }})
    }

    fn signed_history(
        fixture: &Value,
        grant: &StoredGrant,
        period: &str,
        original: &ActionIdentity,
        lookup: u128,
    ) -> Value {
        let mut payload = vec![0];
        payload.extend(fixed::<16>(&original.operation_id).unwrap());
        sign_history_payload(
            fixture,
            grant,
            period,
            (&original.controller_id, &original.subject_id),
            &payload,
            lookup,
        )
    }

    fn signed_history_page(
        fixture: &Value,
        grant: &StoredGrant,
        period: &str,
        after: Option<&str>,
        limit: u8,
        lookup: u128,
    ) -> Value {
        let mut payload = vec![if after.is_some() { 2 } else { 1 }];
        if let Some(after) = after {
            payload.extend(fixed::<16>(after).unwrap());
        }
        payload.push(limit);
        let zero = "00".repeat(16);
        sign_history_payload(fixture, grant, period, (&zero, &zero), &payload, lookup)
    }

    fn sign_history_payload(
        fixture: &Value,
        grant: &StoredGrant,
        period: &str,
        owner: (&str, &str),
        payload: &[u8],
        lookup: u128,
    ) -> Value {
        let operation = format!("{lookup:032x}");
        let hash = lower_hex(&Sha256::digest(payload));
        let mut bytes = prefix(2);
        bytes.extend(fixed::<16>(&grant.grant_id).unwrap());
        bytes.extend(grant.grant_revision.to_be_bytes());
        bytes.extend(fixed::<16>(period).unwrap());
        bytes.extend(fixed::<32>(&grant.receiver_identity).unwrap());
        bytes.extend(fixed::<16>(owner.0).unwrap());
        bytes.extend(fixed::<16>(owner.1).unwrap());
        bytes.push(2);
        bytes.extend(fixed::<32>(&hash).unwrap());
        bytes.extend(fixed::<16>(&operation).unwrap());
        bytes.extend(0_u64.to_be_bytes());
        bytes.push(0);
        let key = EcdsaKeyPair::from_pkcs8(
            &ECDSA_P256_SHA256_ASN1_SIGNING,
            &unhex(fixture["keys"]["working"]["pkcs8"].as_str().unwrap()),
        )
        .unwrap();
        let signature = key.sign(&SystemRandom::new(), &bytes).unwrap();
        json!({"v":1,"type":"request","request":{
            "grant_id":grant.grant_id,"grant_revision":grant.grant_revision,"period_id":period,
            "receiver_identity":grant.receiver_identity,"controller_id":owner.0,
            "subject_id":owner.1,"action":2,"payload":lower_hex(payload),
            "payload_hash":hash,"operation_id":operation,"expected_revision":0,"interaction":0,
            "signature":lower_hex(signature.as_ref())
        }})
    }

    async fn history_grant(
        service: &Service,
        app: &axum::Router,
        phone: &crate::DeviceCredential,
        fixture: &Value,
        project: &std::path::Path,
        groups: Vec<String>,
    ) -> (StoredGrant, String) {
        let now = Instant::now();
        let offer = service
            .authority
            .start(&phone.device_id, now, crate::time::now_ms())
            .unwrap();
        let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
        service
            .authority
            .submit(
                &phone.device_id,
                &offer.enrollment_id,
                &offer.matching_code,
                key,
                now,
            )
            .unwrap();
        let grant = service
            .authority
            .approve(
                &phone.device_id,
                &offer.enrollment_id,
                key,
                Scope {
                    projects: vec![project.to_str().unwrap().into()],
                    agents: vec!["codex".into()],
                    groups,
                },
                now,
            )
            .unwrap();
        let period = history_period(app, phone, fixture, &grant).await;
        (grant, period)
    }

    async fn history_period(
        app: &axum::Router,
        phone: &crate::DeviceCredential,
        fixture: &Value,
        grant: &StoredGrant,
    ) -> String {
        let body = json!({"v":1,"type":"challenge","grant_id":grant.grant_id,"grant_revision":grant.grant_revision});
        let (_, challenge) = post(
            app.clone(),
            "/v1/phone-control/challenge",
            &phone.credential,
            &body.to_string(),
        )
        .await;
        let (_, unlocked) = post(
            app.clone(),
            "/v1/phone-control/unlock",
            &phone.credential,
            &signed_unlock(fixture, &challenge).to_string(),
        )
        .await;
        unlocked["period_id"].as_str().unwrap().to_owned()
    }

    async fn post(
        router: axum::Router,
        path: &str,
        credential: &str,
        body: &str,
    ) -> (StatusCode, Value) {
        let response = router
            .oneshot(
                Request::post(path)
                    .header("authorization", format!("Bearer {credential}"))
                    .header("content-type", "application/json")
                    .body(Body::from(body.to_owned()))
                    .unwrap(),
            )
            .await
            .unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    #[tokio::test]
    async fn pairing_alone_cannot_grant_a_challenge_and_enrollment_is_bounded() {
        let root = tempfile::tempdir().unwrap();
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let receiver = "01".repeat(32);
        let offer = pairing
            .issue("https://192.168.1.2:7391", &receiver, 1_000, 10_000)
            .unwrap();
        let phone = pairing
            .consume(&offer.token, "fixture phone", 1_001)
            .unwrap();
        let authority =
            Arc::new(Authority::open(&root.path().join("phone/grants"), &receiver).unwrap());
        let evidence = super::super::ActionEvidenceStore::open(root.path().join("evidence"));
        let app = super::router(
            authority.clone(),
            pairing,
            Arc::default(),
            Arc::new(evidence),
        );
        let (_, denied) = post(
            app.clone(),
            "/v1/phone-control/challenge",
            &phone.credential,
            &json!({"v":1,"type":"challenge","grant_id":"00".repeat(16),"grant_revision":1})
                .to_string(),
        )
        .await;
        assert_eq!(denied, json!({"v":1,"type":"error","code":"revoked"}));
        let local = authority
            .start(&phone.device_id, Instant::now(), 1_000)
            .unwrap();
        let (_, offered) = post(
            app.clone(),
            "/v1/phone-control/enrollment/offer",
            &phone.credential,
            &json!({"v":1,"type":"enrollment_offer"}).to_string(),
        )
        .await;
        assert_eq!(offered["matching_code"], local.matching_code);
        assert_eq!(offered["enrollment_id"], local.enrollment_id);
        let (_, duplicate) = post(
            app.clone(),
            "/v1/phone-control/enrollment/offer",
            &phone.credential,
            "{\"v\":1,\"type\":\"enrollment_offer\",\"v\":1}",
        )
        .await;
        assert_eq!(duplicate, json!({"v":1,"type":"error","code":"malformed"}));
        let (_, denied) = post(app, "/v1/phone-control/enrollment/offer", "wrong", "{}").await;
        assert_eq!(
            denied,
            json!({"v":1,"type":"error","code":"unauthenticated"})
        );
    }

    #[tokio::test]
    async fn paired_phone_enrolls_only_after_operator_approval_and_unlock_proof_is_single_use() {
        let root = tempfile::tempdir().unwrap();
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let receiver = "01".repeat(32);
        let paired = pairing
            .issue("https://192.168.1.2:7391", &receiver, 1_000, 10_000)
            .unwrap();
        let phone = pairing
            .consume(&paired.token, "fixture phone", 1_001)
            .unwrap();
        let authority =
            Arc::new(Authority::open(&root.path().join("phone/grants"), &receiver).unwrap());
        let evidence = super::super::ActionEvidenceStore::open(root.path().join("evidence"));
        let app = super::router(
            authority.clone(),
            pairing,
            Arc::default(),
            Arc::new(evidence),
        );
        let now = Instant::now();
        let offer = authority
            .start(&phone.device_id, now, crate::time::now_ms())
            .unwrap();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../android/app/src/test/resources/phone-control-v1.json"
        ))
        .unwrap();
        let key = fixture["keys"]["enrolled"]["sec1"].as_str().unwrap();
        let submit = json!({"v":1,"type":"enrollment_submit","enrollment_id":offer.enrollment_id,
            "matching_code":offer.matching_code,"enrolled_key":key});
        let (_, pending) = post(
            app.clone(),
            "/v1/phone-control/enrollment/submit",
            &phone.credential,
            &submit.to_string(),
        )
        .await;
        assert_eq!(
            pending,
            json!({"v":1,"type":"enrollment_pending","enrollment_id":offer.enrollment_id})
        );
        let grant = authority
            .approve(
                &phone.device_id,
                &offer.enrollment_id,
                key,
                Scope {
                    projects: vec![root.path().to_str().unwrap().to_owned()],
                    agents: vec!["codex".into()],
                    groups: vec!["read".into()],
                },
                now,
            )
            .unwrap();
        let status = json!({"v":1,"type":"enrollment_status","enrollment_id":offer.enrollment_id});
        let (_, granted) = post(
            app.clone(),
            "/v1/phone-control/enrollment/status",
            &phone.credential,
            &status.to_string(),
        )
        .await;
        assert_eq!(granted["type"], "enrollment_granted");
        assert_eq!(granted["grant_id"], grant.grant_id);
        let request = json!({"v":1,"type":"challenge","grant_id":grant.grant_id,"grant_revision":grant.grant_revision});
        let (_, challenge) = post(
            app.clone(),
            "/v1/phone-control/challenge",
            &phone.credential,
            &request.to_string(),
        )
        .await;
        assert_eq!(challenge["type"], "challenge");
        let proof = signed_unlock(&fixture, &challenge);
        let (_, unlocked) = post(
            app.clone(),
            "/v1/phone-control/unlock",
            &phone.credential,
            &proof.to_string(),
        )
        .await;
        assert_eq!(unlocked["type"], "unlocked");
        assert_eq!(unlocked["grant_id"], grant.grant_id);
        let (_, replay) = post(
            app,
            "/v1/phone-control/unlock",
            &phone.credential,
            &proof.to_string(),
        )
        .await;
        assert_eq!(
            replay,
            json!({"v":1,"type":"error","code":"unauthenticated"})
        );
    }

    #[tokio::test]
    #[expect(
        clippy::too_many_lines,
        reason = "one history scenario follows replacement, revocation, and receiver restart"
    )]
    async fn history_uses_current_read_scope_without_a_controller_or_old_grant() {
        let root = tempfile::tempdir().unwrap();
        let pairing = Arc::new(PairingRegistry::open(root.path().join("pairing")).unwrap());
        let receiver = "01".repeat(32);
        let create_phone = |name: &str| {
            let offer = pairing
                .issue("https://192.168.1.2:7391", &receiver, 1_000, 10_000)
                .unwrap();
            pairing.consume(&offer.token, name, 1_001).unwrap()
        };
        let old = create_phone("old phone");
        let replacement = create_phone("replacement phone");
        let service = Service::open(
            &root.path().join("phone/grants"),
            &receiver,
            pairing.clone(),
        )
        .unwrap();
        let app = service.router();
        let fixture: Value = serde_json::from_str(include_str!(
            "../../../android/app/src/test/resources/phone-control-v1.json"
        ))
        .unwrap();
        let (old_grant, old_period) = history_grant(
            &service,
            &app,
            &old,
            &fixture,
            root.path(),
            vec!["read".into()],
        )
        .await;
        let original = ActionIdentity {
            operation_id: "10".repeat(16),
            receiver_identity: receiver.clone(),
            controller_id: "11".repeat(16),
            subject_id: "12".repeat(16),
            action: 3,
            payload_hash: "13".repeat(32),
            grant_id: old_grant.grant_id.clone(),
            grant_revision: old_grant.grant_revision,
            period_id: old_period.clone(),
            expected_revision: 9,
            project: std::fs::canonicalize(root.path())
                .unwrap()
                .to_str()
                .unwrap()
                .into(),
            agent: "codex".into(),
            recorded_at_ms: 1_000,
        };
        let store = service.evidence.as_ref().as_ref().unwrap();
        store.reserve(&original).unwrap();
        store
            .report(
                &original,
                &ActionOutcome::Accepted {
                    evidence: "14".repeat(32),
                },
                2_000,
            )
            .unwrap();
        let mut hidden = original.clone();
        hidden.operation_id = "15".repeat(16);
        hidden.agent = "other".into();
        store.reserve(&hidden).unwrap();
        let mut second = original.clone();
        second.operation_id = "20".repeat(16);
        store.reserve(&second).unwrap();
        let (grant, period) = history_grant(
            &service,
            &app,
            &replacement,
            &fixture,
            root.path(),
            vec!["read".into()],
        )
        .await;
        let lookup = signed_history(&fixture, &grant, &period, &original, 100);
        let (status, history) = post(
            app.clone(),
            "/v1/phone-control/request",
            &replacement.credential,
            &lookup.to_string(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(history["type"], "action_history");
        assert_eq!(history["request_id"], format!("{:032x}", 100));
        assert_eq!(history["record"]["operation_id"], original.operation_id);
        assert_eq!(history["record"]["state"], "accepted");
        assert_eq!(history["record"]["evidence"], "14".repeat(32));
        assert!(history["record"].get("payload").is_none());

        let page = signed_history_page(&fixture, &grant, &period, None, 1, 108);
        let (_, first) = post(
            app.clone(),
            "/v1/phone-control/request",
            &replacement.credential,
            &page.to_string(),
        )
        .await;
        assert_eq!(first["type"], "action_history_page");
        assert_eq!(first["records"].as_array().unwrap().len(), 1);
        assert_eq!(first["records"][0]["operation_id"], original.operation_id);
        assert_eq!(first["next_after"], original.operation_id);
        let page = signed_history_page(
            &fixture,
            &grant,
            &period,
            Some(&original.operation_id),
            1,
            109,
        );
        let (_, next) = post(
            app.clone(),
            "/v1/phone-control/request",
            &replacement.credential,
            &page.to_string(),
        )
        .await;
        assert_eq!(next["records"].as_array().unwrap().len(), 1);
        assert_eq!(next["records"][0]["operation_id"], second.operation_id);
        assert_eq!(next["records"][0]["state"], "unknown");
        assert_eq!(next["next_after"], "");
        let invalid_page = signed_history_page(&fixture, &grant, &period, None, 0, 110);
        let (_, denied) = post(
            app.clone(),
            "/v1/phone-control/request",
            &replacement.credential,
            &invalid_page.to_string(),
        )
        .await;
        assert_eq!(denied["code"], "malformed");
        let (_, replay) = post(
            app.clone(),
            "/v1/phone-control/request",
            &replacement.credential,
            &lookup.to_string(),
        )
        .await;
        assert_eq!(replay["code"], "unauthenticated");

        let mut tampered = signed_history(&fixture, &grant, &period, &original, 103);
        tampered["request"]["payload"] = json!(format!("00{}", "99".repeat(16)));
        let (_, denied) = post(
            app.clone(),
            "/v1/phone-control/request",
            &replacement.credential,
            &tampered.to_string(),
        )
        .await;
        assert_eq!(denied["code"], "unauthenticated");
        let mut absent = original.clone();
        absent.operation_id = "98".repeat(16);
        let missing = signed_history(&fixture, &grant, &period, &absent, 104);
        let (_, denied) = post(
            app.clone(),
            "/v1/phone-control/request",
            &replacement.credential,
            &missing.to_string(),
        )
        .await;
        assert_eq!(denied["code"], "unavailable");
        let mut forged_interaction = signed_history(&fixture, &grant, &period, &original, 105);
        forged_interaction["request"]["interaction"] = json!(1);
        let (_, denied) = post(
            app.clone(),
            "/v1/phone-control/request",
            &replacement.credential,
            &forged_interaction.to_string(),
        )
        .await;
        assert_eq!(denied["code"], "malformed");

        service.authority.disable(&old.device_id).unwrap();
        let old_lookup = signed_history(&fixture, &old_grant, &old_period, &original, 101);
        let (_, denied) = post(
            app.clone(),
            "/v1/phone-control/request",
            &old.credential,
            &old_lookup.to_string(),
        )
        .await;
        assert_eq!(denied["code"], "revoked");
        drop(app);
        drop(service);
        let service = Service::open(&root.path().join("phone/grants"), &receiver, pairing).unwrap();
        let app = service.router();
        let period = history_period(&app, &replacement, &fixture, &grant).await;
        let resumed = signed_history(&fixture, &grant, &period, &original, 106);
        let (_, history) = post(
            app.clone(),
            "/v1/phone-control/request",
            &replacement.credential,
            &resumed.to_string(),
        )
        .await;
        assert_eq!(history["record"]["state"], "accepted");
        let other_root = tempfile::tempdir().unwrap();
        let (out_of_scope, next_period) = history_grant(
            &service,
            &app,
            &replacement,
            &fixture,
            other_root.path(),
            vec!["read".into()],
        )
        .await;
        let request = signed_history(&fixture, &out_of_scope, &next_period, &original, 102);
        let (_, denied) = post(
            app.clone(),
            "/v1/phone-control/request",
            &replacement.credential,
            &request.to_string(),
        )
        .await;
        assert_eq!(denied["code"], "out_of_scope");
        let (no_read, next_period) = history_grant(
            &service,
            &app,
            &replacement,
            &fixture,
            root.path(),
            vec!["operate_sessions".into()],
        )
        .await;
        let request = signed_history(&fixture, &no_read, &next_period, &original, 107);
        let (_, denied) = post(
            app,
            "/v1/phone-control/request",
            &replacement.credential,
            &request.to_string(),
        )
        .await;
        assert_eq!(denied["code"], "out_of_scope");
    }
}
