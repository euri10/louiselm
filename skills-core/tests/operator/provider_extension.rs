use super::*;
use louiselm_skills::broker::{
    operator::provider_extension,
    provider_extension::{
        Extension, ExtensionError, ExtensionOutcome, ExtensionRequest, OUTCOME_SCHEMA,
    },
    provider_requests::{HoldReason, ProviderHold},
};

#[test]
fn extension_reply_must_answer_the_exact_request_and_session() {
    for scenario in ["valid", "foreign", "altered", "refused"] {
        let root = private_root();
        let path = root.path().join("operator.sock");
        let uid = rustix::process::geteuid().as_raw();
        let server = OperatorServer::bind(&path, uid).unwrap();
        let request = ExtensionRequest {
            request_id: "ext-1".into(),
            additional_requests: 2,
            expires_at_ms: None,
        };
        let expected = request.clone();
        let worker = thread::spawn(move || {
            let mut outcome = ExtensionOutcome {
                schema: OUTCOME_SCHEMA.into(),
                session_id: "session".into(),
                extension: Extension {
                    run_id: "run".into(),
                    request: expected.clone(),
                    operator_uid: uid,
                    approved_at_ms: 100,
                    lifts: ProviderHold {
                        run_id: "run".into(),
                        reason: HoldReason::Exhausted,
                        held_at_ms: 90,
                    },
                },
                total_requests: 3,
                spent: 1,
                expires_at_ms: 1000,
            };
            match scenario {
                "foreign" => outcome.session_id = "foreign".into(),
                "altered" => outcome.extension.request.additional_requests = 9,
                _ => {}
            }
            server
                .serve_once(
                    |_, _| panic!("not dependencies"),
                    |_, _| panic!("not status"),
                    |_| panic!("not conformance"),
                    |_, _| panic!("not Skill control"),
                    |_, _| panic!("not Beads control"),
                    |_, _| panic!("not retention"),
                    |_, _, _| panic!("not waiver"),
                    |id, request, _| {
                        assert_eq!(id, "session");
                        assert_eq!(*request, expected);
                        if scenario == "refused" {
                            Err(ExtensionError::NotHeld)
                        } else {
                            Ok(outcome)
                        }
                    },
                )
                .unwrap();
        });
        let response = provider_extension(&path, uid, "session", &request, Duration::from_secs(2));
        match scenario {
            "valid" => assert!(matches!(response, Ok(Ok(_)))),
            "refused" => assert_eq!(response, Ok(Err(ExtensionError::NotHeld))),
            _ => assert!(response.is_err(), "{scenario}"),
        }
        worker.join().unwrap();
    }
}
