use super::*;
use louiselm_skills::{
    Digest,
    broker::{
        operator::conformance_waiver,
        waiver::{Outcome, Plan, Proposal, Receipt, Request},
    },
    conformance::admission::Condition,
};

#[test]
fn approval_reply_requires_the_exact_receipt_and_session() {
    for scenario in ["valid", "foreign", "missing", "contradictory", "wrong-plan"] {
        let root = private_root();
        let path = root.path().join("operator.sock");
        let uid = rustix::process::geteuid().as_raw();
        let server = OperatorServer::bind(&path, uid).unwrap();
        let plan = Plan {
            schema: "louiselm.conformance-waiver-plan/1".into(),
            session_id: "session".into(),
            run_id: "run".into(),
            envelope_revision: 1,
            operator_uid: uid,
            proposal: Proposal {
                request_id: "waiver-1".into(),
                condition: Condition::Missing,
                rationale: "Private operator explanation".into(),
                expires_at_ms: 200,
            },
            digest: Digest::of(b"preview").to_string(),
        };
        let request = Request::Apply {
            plan_digest: plan.digest.clone(),
        };
        let expected = request.clone();
        let worker = thread::spawn(move || {
            let mut outcome = Outcome {
                schema: "louiselm.conformance-waiver-outcome/1".into(),
                session_id: "session".into(),
                active: true,
                receipt: Some(Receipt {
                    digest: Digest::of(&serde_json::to_vec(&(&plan, 100_u64)).unwrap()).to_string(),
                    plan: plan.clone(),
                    approved_at_ms: 100,
                }),
                plan: Some(plan),
            };
            match scenario {
                "foreign" => outcome.session_id = "foreign".into(),
                "missing" => {
                    outcome.receipt = None;
                    outcome.active = false;
                }
                "contradictory" => {
                    outcome.receipt.as_mut().unwrap().plan.proposal.rationale =
                        "substituted".into();
                }
                "wrong-plan" => {
                    outcome.plan.as_mut().unwrap().digest = Digest::of(b"other").to_string();
                }
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
                    |id, request, _| {
                        assert_eq!(id, "session");
                        assert_eq!(*request, expected);
                        Ok(outcome)
                    },
                )
                .unwrap();
        });
        let response = conformance_waiver(&path, uid, "session", &request, Duration::from_secs(2));
        assert_eq!(response.is_ok(), scenario == "valid", "{scenario}");
        worker.join().unwrap();
    }
}
