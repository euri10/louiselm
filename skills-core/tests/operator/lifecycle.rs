//! Operator lifecycle authentication, exact request binding and truthful refusals.

use super::*;
use louiselm_skills::{
    Digest,
    broker::operator::{lifecycle, validate_lifecycle_request},
    launch::PROTOCOL_VERSION,
    launch_protocol::{
        ErrorCode, LIFECYCLE_REQUEST_SCHEMA, LifecycleAction, LifecycleRequest, ProtocolError,
    },
    launch_receipt::{
        Authorization, RECEIPT_SCHEMA, ReceiptAuthority, ReceiptOutcome, ReceiptPayload,
        SIGNED_RECEIPT_SCHEMA, SessionState, SignedReceipt,
    },
};
use std::time::Instant;

fn request(action: LifecycleAction) -> LifecycleRequest {
    LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "request".into(),
        session_id: "session".into(),
        run_id: "run".into(),
        authorization_id: "authorization".into(),
        action,
        expected_state: if action == LifecycleAction::Resume {
            SessionState::Parked
        } else {
            SessionState::Running
        },
        expected_receipt_sequence: Some(3),
        envelope_revision: 1,
    }
}

fn receipt(request: &LifecycleRequest) -> SignedReceipt {
    let authorization = Authorization {
        authorization_id: request.authorization_id.clone(),
        request_id: request.request_id.clone(),
        request_digest: request.digest().to_string(),
    };
    let (outcome, resulting_state) = match request.action {
        LifecycleAction::Park => (
            ReceiptOutcome::Park {
                authority: ReceiptAuthority::Authorized(authorization),
            },
            SessionState::Parked,
        ),
        LifecycleAction::Resume => (
            ReceiptOutcome::Resume { authorization },
            SessionState::Running,
        ),
        LifecycleAction::Disposal => (
            ReceiptOutcome::Disposal {
                authority: ReceiptAuthority::Authorized(authorization),
            },
            SessionState::Terminal,
        ),
        LifecycleAction::Interrupt => panic!("outside operator scope"),
    };
    SignedReceipt {
        schema: SIGNED_RECEIPT_SCHEMA.into(),
        signature: "fixture-signature".into(),
        payload: ReceiptPayload {
            schema: RECEIPT_SCHEMA.into(),
            session_id: request.session_id.clone(),
            run_id: request.run_id.clone(),
            request_id: request.request_id.clone(),
            envelope_revision: request.envelope_revision,
            sequence: 4,
            previous_receipt_digest: Some(Digest::of(b"previous").to_string()),
            release_id: Digest::of(b"release").to_string(),
            signing_key_id: Digest::of(b"launcher").to_string(),
            outcome,
            resulting_state,
        },
    }
}

fn serve(
    server: OperatorServer,
    handler: impl FnOnce(
        &LifecycleRequest,
        Instant,
    ) -> Result<Result<SignedReceipt, ProtocolError>, InspectError>
    + Send
    + 'static,
) -> thread::JoinHandle<()> {
    thread::spawn(move || {
        server
            .serve_once(
                handler,
                |_| panic!("not input staging"),
                |_, _| panic!("not verification"),
                |_| panic!("not authorization"),
                |_, _| panic!("not dependencies"),
                |_, _| panic!("not inspection"),
                |_| panic!("not conformance"),
                |_, _| panic!("not skill"),
                |_, _| panic!("not Beads"),
                |_, _| panic!("not retention"),
                |_, _, _| panic!("not waiver"),
                |_, _, _| panic!("not Provider"),
            )
            .unwrap();
    })
}

#[test]
fn operator_lifecycle_preserves_exact_cas_and_signed_outcome() {
    for action in [
        LifecycleAction::Park,
        LifecycleAction::Resume,
        LifecycleAction::Disposal,
    ] {
        let root = private_root();
        let path = root.path().join("operator.sock");
        let uid = rustix::process::geteuid().as_raw();
        let sent = request(action);
        let expected = receipt(&sent);
        expected.validate().unwrap();
        let copy = sent.clone();
        let returned = expected.clone();
        let worker = serve(
            OperatorServer::bind(&path, uid).unwrap(),
            move |received, deadline| {
                assert_eq!(received, &copy);
                assert!(Instant::now() < deadline);
                Ok(Ok(returned))
            },
        );
        assert_eq!(
            lifecycle(&path, uid, &sent, Duration::from_secs(2))
                .unwrap()
                .unwrap(),
            expected
        );
        worker.join().unwrap();
    }
}

#[test]
fn operator_lifecycle_rejects_foreign_peers_and_preserves_policy_refusal() {
    let root = private_root();
    let path = root.path().join("operator.sock");
    let uid = rustix::process::geteuid().as_raw();
    let worker = serve(OperatorServer::bind(&path, uid + 1).unwrap(), |_, _| {
        panic!("foreign operator reached lifecycle")
    });
    assert_eq!(
        lifecycle(
            &path,
            uid,
            &request(LifecycleAction::Resume),
            Duration::from_secs(2)
        ),
        Err(InspectError::AuthenticationRefused)
    );
    worker.join().unwrap();
    let refused = ProtocolError::new(
        ErrorCode::ReceiptSequenceMismatch,
        Some(SessionState::Parked),
        Some(8),
    );
    let copy = refused.clone();
    let worker = serve(OperatorServer::bind(&path, uid).unwrap(), move |_, _| {
        Ok(Err(copy))
    });
    assert_eq!(
        lifecycle(
            &path,
            uid,
            &request(LifecycleAction::Resume),
            Duration::from_secs(2)
        )
        .unwrap(),
        Err(refused)
    );
    worker.join().unwrap();
}

#[test]
fn lifecycle_client_refuses_mismatched_receipt_and_error_fields() {
    for damage in [
        "session",
        "run",
        "request",
        "sequence",
        "revision",
        "authorization",
        "digest",
        "action",
        "cause",
        "error",
    ] {
        let root = private_root();
        let path = root.path().join("operator.sock");
        let uid = rustix::process::geteuid().as_raw();
        let sent = request(LifecycleAction::Resume);
        let mut changed = receipt(&sent);
        match damage {
            "session" => changed.payload.session_id = "foreign".into(),
            "run" => changed.payload.run_id = "foreign".into(),
            "request" => changed.payload.request_id = "foreign".into(),
            "sequence" => changed.payload.sequence += 1,
            "revision" => changed.payload.envelope_revision += 1,
            "authorization" | "digest" => {
                let ReceiptOutcome::Resume { authorization } = &mut changed.payload.outcome else {
                    panic!("fixture")
                };
                if damage == "authorization" {
                    authorization.authorization_id = "foreign".into();
                } else {
                    authorization.request_digest = Digest::of(b"foreign").to_string();
                }
            }
            "action" => changed = receipt(&request(LifecycleAction::Disposal)),
            "cause" => {
                changed.payload.outcome = ReceiptOutcome::Park {
                    authority: ReceiptAuthority::Cause {
                        cause: louiselm_skills::launch_receipt::ReceiptCause::BrokerLost,
                    },
                };
                changed.payload.resulting_state = SessionState::Parked;
            }
            "error" => (),
            _ => panic!("fixture"),
        }
        let worker = serve(OperatorServer::bind(&path, uid).unwrap(), move |_, _| {
            if damage == "error" {
                let mut error = ProtocolError::new(ErrorCode::InvalidRequest, None, None);
                error.retryable = !error.retryable;
                Ok(Err(error))
            } else {
                Ok(Ok(changed))
            }
        });
        assert_eq!(
            lifecycle(&path, uid, &sent, Duration::from_secs(2)),
            Err(InspectError::StatusUnavailable),
            "{damage}"
        );
        worker.join().unwrap();
    }
}

#[test]
fn lifecycle_validation_precedes_endpoint_lookup() {
    let sent = request(LifecycleAction::Park);
    let mut invalid = vec![request(LifecycleAction::Interrupt)];
    let mut changed = sent.clone();
    changed.envelope_revision = 0;
    invalid.push(changed);
    let mut changed = sent.clone();
    changed.expected_receipt_sequence = None;
    invalid.push(changed);
    let mut changed = sent.clone();
    changed.session_id = "../foreign".into();
    invalid.push(changed);
    let mut changed = sent.clone();
    changed.schema = "foreign/1".into();
    invalid.push(changed);
    let mut changed = sent.clone();
    changed.protocol_version += 1;
    invalid.push(changed);
    for changed in invalid {
        assert_eq!(
            validate_lifecycle_request(&changed),
            Err(InspectError::InvalidRequest)
        );
        assert_eq!(
            lifecycle(
                std::path::Path::new("/missing/operator.sock"),
                0,
                &changed,
                Duration::from_secs(1)
            ),
            Err(InspectError::InvalidRequest)
        );
    }
}

#[test]
fn lifecycle_server_rejects_unknown_authority_before_dispatch() {
    use std::{io::Write, os::unix::net::UnixStream};
    let uid = rustix::process::geteuid().as_raw();
    let request = serde_json::to_value(request(LifecycleAction::Park)).unwrap();
    for (outer, field) in [
        (true, "uid"),
        (true, "caller"),
        (false, "operator_uid"),
        (false, "role"),
        (false, "expires_at_ms"),
    ] {
        let root = private_root();
        let path = root.path().join("operator.sock");
        let worker = serve(OperatorServer::bind(&path, uid).unwrap(), |_, _| {
            panic!("unknown authority reached lookup")
        });
        let mut envelope =
            serde_json::json!({"schema":"louiselm.operator-lifecycle/1", "request":request});
        if outer {
            envelope[field] = serde_json::json!(0);
        } else {
            envelope["request"][field] = serde_json::json!(0);
        }
        let bytes = serde_json::to_vec(&envelope).unwrap();
        let mut stream = UnixStream::connect(&path).unwrap();
        assert_eq!(read_frame(&mut stream), b"louiselm.operator/1");
        stream
            .write_all(&u32::try_from(bytes.len()).unwrap().to_be_bytes())
            .unwrap();
        stream.write_all(&bytes).unwrap();
        assert_eq!(
            read_frame(&mut stream),
            InspectError::InvalidRequest.canonical_bytes()
        );
        worker.join().unwrap();
    }
}
