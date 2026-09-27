use super::*;
use louiselm_skills::{
    Digest,
    beads_mutation::{ApprovedBeadsMutations, BeadsEffect, BeadsRole},
    broker::{
        operator::{AuthorizationRequest, AuthorizationResponse, authorization},
        run_envelope::{RunAuthorization, RunEnvelope},
    },
    provider_request::{ApprovedProviderRequests, ReasoningEffort},
};

fn envelope(uid: u32) -> RunEnvelope {
    RunEnvelope {
        schema: "louiselm.broker.run-envelope/1".into(),
        run_id: "run".into(),
        envelope_id: "envelope".into(),
        envelope_revision: 1,
        controller_uid: uid,
        bead_scope: ApprovedBeadsMutations {
            project_digest: Digest::of(b"project").to_string(),
            role: BeadsRole::Coordinator,
            issue_ids: vec!["louiselm-a".into()],
            effects: vec![BeadsEffect::CommentAdd],
            max_mutations: 1,
            expires_at_ms: 10_000,
        },
        provider_requests: ApprovedProviderRequests {
            disclosure_profile: louiselm_skills::provider_request::disclosure::profile_digest(),
            provider: "openai".into(),
            upstream: "https://api.openai.com/v1/responses".into(),
            addresses: vec!["127.0.0.1".parse().unwrap()],
            max_run_requests: 1,
            models: vec!["gpt-5.6-luna".into()],
            max_effort: ReasoningEffort::High,
            expires_at_ms: 10_000,
        },
        commands: None,
        verification_plan_digest: Digest::of(b"plan").to_string(),
        max_sessions: 2,
        expires_at_ms: 10_000,
    }
}

#[test]
fn operator_authorization_checks_peer_identity_and_exact_run_digest() {
    for wrong_digest in [false, true] {
        let root = private_root();
        let path = root.path().join("inspect.sock");
        let uid = rustix::process::geteuid().as_raw();
        let server = OperatorServer::bind(&path, uid).unwrap();
        let approved = envelope(uid);
        let request = AuthorizationRequest::Run {
            envelope: Box::new(approved.clone()),
        };
        let worker = thread::spawn(move || {
            server
                .serve_once(
                    |request| {
                        let AuthorizationRequest::Run { envelope } = request else {
                            panic!("wrong request")
                        };
                        let bytes = if wrong_digest {
                            b"other".to_vec()
                        } else {
                            envelope.canonical_bytes()
                        };
                        Ok(AuthorizationResponse::Run {
                            receipt: RunAuthorization {
                                schema: "louiselm.broker.run-authorization/1".into(),
                                run_id: envelope.run_id.clone(),
                                envelope_revision: envelope.envelope_revision,
                                envelope_digest: Digest::of(&bytes).to_string(),
                            },
                        })
                    },
                    |_, _| panic!("not dependencies"),
                    |_, _| panic!("not inspection"),
                    |_| panic!("not conformance"),
                    |_, _| panic!("not skill"),
                    |_, _| panic!("not Beads"),
                    |_, _| panic!("not retention"),
                    |_, _, _| panic!("not waiver"),
                    |_, _, _| panic!("not extension"),
                )
                .unwrap();
        });
        let result = authorization(&path, uid, &request, Duration::from_secs(2));
        worker.join().unwrap();
        assert_eq!(result.is_ok(), !wrong_digest, "{result:?}");
    }
    let root = private_root();
    let path = root.path().join("inspect.sock");
    let uid = rustix::process::geteuid().as_raw();
    let server = OperatorServer::bind(&path, uid + 1).unwrap();
    let worker = thread::spawn(move || {
        server
            .serve_once(
                |_| panic!("foreign peer reached authorization"),
                |_, _| panic!("foreign peer reached dependencies"),
                |_, _| panic!("foreign peer reached inspection"),
                |_| panic!("foreign peer reached conformance"),
                |_, _| panic!("foreign peer reached skill"),
                |_, _| panic!("foreign peer reached Beads"),
                |_, _| panic!("foreign peer reached retention"),
                |_, _, _| panic!("foreign peer reached waiver"),
                |_, _, _| panic!("foreign peer reached extension"),
            )
            .unwrap();
    });
    assert!(matches!(
        authorization(
            &path,
            uid,
            &AuthorizationRequest::Run {
                envelope: Box::new(envelope(uid))
            },
            Duration::from_secs(2)
        ),
        Err(InspectError::AuthenticationRefused)
    ));
    worker.join().unwrap();
}
