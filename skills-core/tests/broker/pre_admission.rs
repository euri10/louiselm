//! Preparation is evidence, not a consumed authorization or a running Session.
use super::*;
use louiselm_skills::{
    broker::waiver::{Proposal, Request, WaiverError},
    conformance::{
        admission::{Attendance, Condition},
        preparation::Preparation,
    },
};

fn service(root: &Path) -> BrokerService {
    BrokerService::bind(
        &root.join("broker.sock"),
        AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap(),
        ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap()
}

fn observation(request: &LaunchRequest) -> Preparation {
    Preparation {
        schema: "louiselm.conformance-preparation/1".into(),
        request_digest: request.digest().to_string(),
        session_id: request.session_id.clone(),
        operator_uid: CONTROLLER_UID,
        policy_digest: Digest::of(b"installed policy").to_string(),
        boot_id: "00000000-0000-4000-8000-000000000001".into(),
        observed_at_ms: 1000,
        condition: Condition::Missing,
    }
}

fn plan_request() -> Request {
    Request::Plan {
        proposal: Proposal {
            request_id: "review".into(),
            condition: Condition::Missing,
            rationale: "Inspect this host".into(),
            expires_at_ms: 20_000,
        },
    }
}

#[test]
fn pre_admission_refuses_unattended_stale_foreign_and_consumed_authority() {
    let root = TempDir::new().unwrap();
    let service = service(root.path());
    let request = request("refusals");
    let observed = observation(&request);
    let mut grant = grant(&request);
    service.authorizations().authorize(&grant, 1000).unwrap();
    assert!(matches!(
        service.pre_admission_waiver(CONTROLLER_UID, &observed, &plan_request(), 1100),
        Err(BrokerError::Waiver(WaiverError::Unattended))
    ));
    service
        .authorizations()
        .consume(&request, CONTROLLER_UID, 1100)
        .unwrap();
    assert!(matches!(
        service.pre_admission_waiver(CONTROLLER_UID, &observed, &plan_request(), 1101),
        Err(BrokerError::Waiver(WaiverError::Unknown))
    ));
    let request = super::request("interactive");
    grant.request = request.clone();
    grant.conformance.attendance = Attendance::Interactive;
    service.authorizations().authorize(&grant, 1000).unwrap();
    let observed = observation(&request);
    let plan = service
        .pre_admission_waiver(CONTROLLER_UID, &observed, &plan_request(), 1100)
        .unwrap()
        .plan
        .unwrap();
    let apply = Request::Apply {
        plan_digest: plan.digest,
    };
    let mut changed = observed.clone();
    changed.policy_digest = Digest::of(b"changed policy").to_string();
    assert!(matches!(
        service.pre_admission_waiver(CONTROLLER_UID, &changed, &apply, 1101),
        Err(BrokerError::Waiver(WaiverError::StalePlan))
    ));
    changed = observed.clone();
    changed.observed_at_ms = 1200;
    assert!(matches!(
        service.pre_admission_waiver(CONTROLLER_UID, &changed, &apply, 1101),
        Err(BrokerError::Waiver(WaiverError::NotWaivable))
    ));
    changed = observed.clone();
    changed.condition = Condition::ContainmentFailure;
    assert!(matches!(
        service.pre_admission_waiver(CONTROLLER_UID, &changed, &plan_request(), 1101),
        Err(BrokerError::Waiver(WaiverError::NotWaivable))
    ));
    assert!(matches!(
        service.pre_admission_waiver(CONTROLLER_UID, &observed, &apply, 30_000),
        Err(BrokerError::Waiver(WaiverError::Expired))
    ));
}

#[test]
fn approved_preparation_reaches_the_single_use_launch_authorization() {
    let root = TempDir::new().unwrap();
    let service = service(root.path());
    let request = request("approved-launch");
    let mut grant = grant(&request);
    grant.conformance.attendance = Attendance::Interactive;
    service.authorizations().authorize(&grant, 1000).unwrap();
    let observed = observation(&request);
    let plan = service
        .pre_admission_waiver(CONTROLLER_UID, &observed, &plan_request(), 1100)
        .unwrap()
        .plan
        .unwrap();
    let receipt = service
        .pre_admission_waiver(
            CONTROLLER_UID,
            &observed,
            &Request::Apply {
                plan_digest: plan.digest,
            },
            1200,
        )
        .unwrap()
        .receipt
        .unwrap();
    let socket = root.path().join("broker.sock");
    let peer_request = request.clone();
    let peer = thread::spawn(move || {
        let (authorization, channel) = supervisor_authorization(&socket, &peer_request, 1300);
        channel.close();
        authorization
    });
    // The peer deliberately stops before any Agent preparation or signed receipt.
    assert!(matches!(
        service.serve_launch(1300, verify_fixture_signature),
        Err(BrokerError::Transport(_))
    ));
    let authorization = peer.join().unwrap();
    let waiver = authorization.conformance.waiver.unwrap();
    assert_eq!(waiver.preparation, Some(observed));
    assert_eq!(waiver.receipt_digest, receipt.digest);
    assert_eq!(waiver.expires_at_ms, 20_000);
    let consumed = service
        .authorizations()
        .consumed_for_session(&request.session_id)
        .unwrap()
        .unwrap();
    assert_eq!(consumed.conformance.waiver, Some(waiver));
    assert!(matches!(
        service
            .authorizations()
            .consume_for_launcher(&request, 1400),
        Err(BrokerError::UnknownAuthorization)
    ));
}

#[test]
fn pre_admission_approval_is_exact_durable_and_does_not_launch() {
    let root = TempDir::new().unwrap();
    let service = service(root.path());
    let request = request("before-launch");
    let mut grant = grant(&request);
    grant.conformance.attendance = Attendance::Interactive;
    service.authorizations().authorize(&grant, 1000).unwrap();
    let observed = observation(&request);
    let plan = service
        .pre_admission_waiver(CONTROLLER_UID, &observed, &plan_request(), 1100)
        .unwrap()
        .plan
        .unwrap();
    assert!(
        service
            .authorizations()
            .consumed_for_session(&request.session_id)
            .unwrap()
            .is_none()
    );
    let apply = Request::Apply {
        plan_digest: plan.digest,
    };
    let approved = service
        .pre_admission_waiver(CONTROLLER_UID, &observed, &apply, 1200)
        .unwrap();
    assert!(approved.active);
    assert!(
        service
            .receipts()
            .head(&request.session_id)
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        service.pre_admission_waiver(CONTROLLER_UID + 1, &observed, &apply, 1300),
        Err(BrokerError::Waiver(WaiverError::WrongOperator))
    ));
    let mut changed = observed.clone();
    changed.request_digest = Digest::of(b"another launch").to_string();
    assert!(matches!(
        service.pre_admission_waiver(CONTROLLER_UID, &changed, &apply, 1300),
        Err(BrokerError::Waiver(WaiverError::StalePlan))
    ));
    drop(service);
    fs::remove_file(root.path().join("broker.sock")).unwrap();
    let service = self::service(root.path());
    assert_eq!(
        service
            .pre_admission_waiver(CONTROLLER_UID, &observed, &apply, 1400)
            .unwrap(),
        approved
    );
    let historical = service
        .pending_waiver_history(
            CONTROLLER_UID,
            &request.session_id,
            &Request::Inspect,
            400_000,
        )
        .unwrap();
    assert!(!historical.active);
    assert_eq!(historical.receipt, approved.receipt);
    assert!(matches!(
        service.pending_waiver_history(
            CONTROLLER_UID + 1,
            &request.session_id,
            &Request::Inspect,
            400_000,
        ),
        Err(BrokerError::Waiver(WaiverError::WrongOperator))
    ));
    assert!(matches!(
        service.pending_waiver_history(CONTROLLER_UID, &request.session_id, &apply, 400_000,),
        Err(BrokerError::Waiver(WaiverError::InvalidRequest))
    ));
    service
        .pending_waiver_history(
            CONTROLLER_UID,
            &request.session_id,
            &Request::Revoke {
                receipt_digest: approved.receipt.unwrap().digest,
            },
            400_000,
        )
        .unwrap();
    assert!(
        !service
            .pre_admission_waiver(CONTROLLER_UID, &observed, &apply, 1600)
            .unwrap()
            .active
    );
}
