//! Authenticated guard effects reach posture without a Provider request or probe.

use super::*;
use louiselm_skills::{
    broker::{BrokerSession, lifecycle::LifecycleCaller},
    launch_protocol::{
        ChannelState, GuardEnrollment, GuardScope, PendingAction, PendingOperation, PendingPhase,
        SessionStatus, SupervisorStatus,
    },
    posture::{DimensionName, DimensionState, FailureCode},
};
use std::{
    fs::File,
    net::TcpListener,
    os::{fd::AsFd, unix::fs::MetadataExt},
};

struct Fixture {
    service: BrokerService,
    session: BrokerSession,
    peer: SeqpacketChannel,
    status: SupervisorStatus,
    enrollment: GuardEnrollment,
    root: TempDir,
}

fn fixture() -> Fixture {
    fixture_with_guard(true)
}

fn fixture_with_guard(required: bool) -> Fixture {
    fixture_at(required, 0)
}

fn fixture_at(required: bool, origin_ms: u64) -> Fixture {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("broker.sock");
    let request = request("network-posture");
    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
    let mut approval = grant(&request);
    approval.provider_requests = Some(provider_requests::approval(5));
    approval.expires_at_ms += origin_ms;
    approval.provider_requests.as_mut().unwrap().expires_at_ms += origin_ms;
    approval.commands.as_mut().unwrap().expires_at_ms += origin_ms;
    approval.conformance.attendance =
        louiselm_skills::conformance::admission::Attendance::Interactive;
    authorizations
        .authorize(&approval, origin_ms + 1000)
        .unwrap();
    let service = BrokerService::bind(
        &socket,
        authorizations,
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    let worker = thread::spawn(move || {
        let (authorization, peer) = supervisor_authorization(&socket, &request, origin_ms + 2000);
        let launch = launch_receipt(&authorization);
        settle(|done| peer.send(launch.canonical_bytes(), done));
        expect_acknowledgement(&peer);
        let mut start = start_receipt(&authorization, &launch).payload;
        let ReceiptOutcome::Start { evidence, .. } = &mut start.outcome else {
            unreachable!()
        };
        evidence.sender_guard_required = required;
        settle(|done| peer.send(signed(start).canonical_bytes(), done));
        expect_acknowledgement(&peer);
        (authorization, peer)
    });
    let mut session = service
        .serve_launch(origin_ms + 2000, verify_fixture_signature)
        .unwrap();
    let (authorization, peer) = worker.join().unwrap();
    let mut status = lifecycle::status(&authorization);
    status.launcher_head = service.receipts().head(&authorization.session_id).unwrap();
    status.broker_head.clone_from(&status.launcher_head);
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let pins = File::open("/proc/self/ns/mnt").unwrap();
    let network = File::open("/proc/self/ns/net").unwrap();
    let now = rustix::time::clock_gettime(rustix::time::ClockId::Monotonic);
    let enrollment = GuardEnrollment {
        scope: GuardScope {
            session_id: authorization.session_id,
            run_id: authorization.run_id,
            envelope_revision: authorization.envelope_revision,
            revision: 1,
            deadline_ns: u64::try_from(now.tv_sec).unwrap() * 1_000_000_000
                + u64::try_from(now.tv_nsec).unwrap()
                + 20_000_000_000,
        },
        guard_id: pins.metadata().unwrap().ino(),
        runtime_pid: 123,
        broker_pid: std::process::id(),
        address: listener.local_addr().unwrap(),
        listener_cookie: rustix::net::sockopt::socket_cookie(&listener).unwrap(),
        network_id: network.metadata().unwrap().ino().try_into().unwrap(),
    };
    let message = response(ResponseResult::SenderGuardEnrolled {
        enrollment: enrollment.clone(),
    });
    settle(|done| {
        peer.send_descriptors(
            message.canonical_bytes(),
            [listener.as_fd(), pins.as_fd(), network.as_fd()],
            done,
        )
    });
    service
        .step(
            &mut session,
            origin_ms + 2500,
            None,
            verify_fixture_signature,
        )
        .unwrap();
    let _accepted = settle(|done| peer.receive(done));
    Fixture {
        service,
        session,
        peer,
        status,
        enrollment,
        root,
    }
}

fn response(result: ResponseResult) -> ProtocolResponse {
    ProtocolResponse {
        schema: RESPONSE_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "network-proof".into(),
        result,
    }
}

impl Fixture {
    fn status(&mut self, now_ms: u64) -> SessionStatus {
        thread::scope(|threads| {
            let peer = &self.peer;
            let current = &self.status;
            let worker = threads.spawn(move || lifecycle::answer_one_status_query(peer, current));
            let status = self
                .service
                .session_status(
                    &mut self.session,
                    &LifecycleCaller::Operator {
                        uid: CONTROLLER_UID,
                    },
                    now_ms,
                    verify_fixture_signature,
                )
                .unwrap();
            worker.join().unwrap();
            status
        })
    }

    fn activate(&mut self, now_ms: u64) -> Result<bool, BrokerError> {
        let message = response(ResponseResult::SenderGuardActivated {
            enrollment: self.enrollment.clone(),
        });
        settle(|done| self.peer.send(message.canonical_bytes(), done));
        self.service
            .step(&mut self.session, now_ms, None, verify_fixture_signature)
    }
}

#[test]
fn authenticated_activation_not_enrollment_verifies_network_without_disclosure() {
    let mut fixture = fixture();
    let pending = fixture.status(2600);
    let dimension = pending
        .posture
        .dimensions
        .iter()
        .find(|d| d.dimension == DimensionName::Network)
        .unwrap();
    assert_eq!(dimension.failure_code, Some(FailureCode::EvidenceMissing));
    fixture
        .activate(2700)
        .expect("authenticated activation must reach the evidence producer");
    let active = fixture.status(2800);
    let dimension = active
        .posture
        .dimensions
        .iter()
        .find(|d| d.dimension == DimensionName::Network)
        .unwrap();
    assert_eq!(dimension.state, DimensionState::Verified);
    assert_eq!(dimension.freshness.last_verified_at_ms, Some(2700));
    assert_eq!(
        active
            .posture
            .dimensions
            .iter()
            .find(|d| d.dimension == DimensionName::ProviderDisclosure)
            .unwrap()
            .failure_code,
        Some(FailureCode::EvidenceMissing)
    );
    let before = fixture.service.audit().unwrap();
    let original = active.posture.clone();
    fixture.activate(2900).unwrap();
    let repeated = fixture.status(3000);
    assert_eq!(
        repeated.posture, original,
        "duplicate activation and reads never renew proof"
    );
    assert_eq!(fixture.service.audit().unwrap(), before);
    let json = String::from_utf8(repeated.canonical_bytes()).unwrap();
    for secret in [
        "runtime_pid",
        "broker_pid",
        "network_id",
        "listener_cookie",
        "guard_id",
        "api.openai.com",
        "192.0.2.1",
    ] {
        assert!(!json.contains(secret), "status leaked {secret}");
    }
}

#[test]
fn pending_activation_and_revocation_preserve_last_success_without_claiming_network() {
    let mut fixture = fixture();
    fixture.activate(2700).unwrap();
    fixture.status.channel_state = ChannelState::Revoked;
    fixture.status.pending_operation = Some(PendingOperation {
        request_id: "resume-in-progress".into(),
        action: PendingAction::Resume,
        phase: PendingPhase::Activating,
    });
    let status = fixture.status(2800);
    let network = status
        .posture
        .dimensions
        .iter()
        .find(|d| d.dimension == DimensionName::Network)
        .unwrap();
    assert_eq!(network.failure_code, Some(FailureCode::EvidenceInvalidated));
    assert_eq!(network.freshness.last_verified_at_ms, Some(2700));
    fixture.status.pending_operation = None;
    let status = fixture.status(2900);
    assert_eq!(
        status
            .posture
            .dimensions
            .iter()
            .find(|d| d.dimension == DimensionName::Network)
            .unwrap()
            .state,
        DimensionState::Failed
    );
}

#[test]
fn expired_authority_invalidates_network_without_renewal_or_provider_effects() {
    let mut fixture = fixture();
    fixture.activate(2700).unwrap();
    let status = fixture.status(30_000);
    let network = status
        .posture
        .dimensions
        .iter()
        .find(|d| d.dimension == DimensionName::Network)
        .unwrap();
    assert_eq!(network.failure_code, Some(FailureCode::EvidenceInvalidated));
    assert_eq!(network.freshness.last_verified_at_ms, Some(2700));
    assert!(matches!(
        fixture.activate(30_000),
        Err(BrokerError::InvalidGrant)
    ));
}

#[test]
fn activation_cannot_substitute_subject_revision_or_runtime() {
    for change in [
        "session", "run", "envelope", "revision", "runtime", "broker", "deadline",
    ] {
        let mut fixture = fixture();
        match change {
            "session" => fixture.enrollment.scope.session_id = "foreign-session".into(),
            "run" => fixture.enrollment.scope.run_id = "foreign-run".into(),
            "envelope" => fixture.enrollment.scope.envelope_revision += 1,
            "revision" => fixture.enrollment.scope.revision += 1,
            "runtime" => fixture.enrollment.runtime_pid += 1,
            "broker" => fixture.enrollment.broker_pid += 1,
            "deadline" => fixture.enrollment.scope.deadline_ns += 1,
            _ => unreachable!(),
        }
        assert!(
            matches!(fixture.activate(2700), Err(BrokerError::RequestMismatch)),
            "{change} was not refused at the binding boundary"
        );
        assert!(fixture.session.channel().is_closed());
    }
}

#[test]
fn closed_listener_cannot_be_revived_by_an_old_activation() {
    let mut fixture = fixture();
    fixture.activate(2700).unwrap();
    let closing = response(ResponseResult::SenderGuardClosing {
        enrollment: fixture.enrollment.clone(),
    });
    settle(|done| fixture.peer.send(closing.canonical_bytes(), done));
    fixture
        .service
        .step(&mut fixture.session, 2800, None, verify_fixture_signature)
        .unwrap();
    let _closed = settle(|done| fixture.peer.receive(done));
    let status = fixture.status(2900);
    let network = status
        .posture
        .dimensions
        .iter()
        .find(|d| d.dimension == DimensionName::Network)
        .unwrap();
    assert_eq!(network.failure_code, Some(FailureCode::EvidenceInvalidated));
    assert_eq!(network.freshness.last_verified_at_ms, Some(2700));
    assert!(matches!(
        fixture.activate(3000),
        Err(BrokerError::InvalidGrant)
    ));
}

#[test]
fn unguarded_start_cannot_be_promoted_by_an_activation_observation() {
    let mut fixture = fixture_with_guard(false);
    assert!(matches!(
        fixture.activate(2700),
        Err(BrokerError::ReceiptUnauthorized)
    ));
    assert!(fixture.session.channel().is_closed());
}

#[test]
fn broker_reattachment_cannot_reconstruct_process_owned_network_proof() {
    let mut fixture = fixture();
    fixture.activate(2700).unwrap();
    fixture.session.close();
    let bytes = fixture
        .service
        .receipts()
        .stored_bytes("network-posture")
        .unwrap();
    let start = SignedReceipt::parse_canonical(bytes.last().unwrap()).unwrap();
    let peer = reconnect::peer(fixture.root.path(), &reconnect::checkpoint(&start));
    fixture.session = fixture
        .service
        .serve_reconnect(2800, verify_fixture_signature)
        .unwrap();
    let _ack = settle(|done| peer.receive(done));
    fixture.peer = peer;
    let status = fixture.status(2900);
    let network = status
        .posture
        .dimensions
        .iter()
        .find(|d| d.dimension == DimensionName::Network)
        .unwrap();
    assert_eq!(network.failure_code, Some(FailureCode::EvidenceMissing));
    assert_eq!(network.freshness.last_verified_at_ms, None);
    assert!(matches!(
        fixture.activate(3000),
        Err(BrokerError::InvalidGrant)
    ));
}

#[test]
fn lifting_a_provider_hold_cannot_revive_the_old_activation() {
    let mut fixture = fixture();
    fixture.activate(2700).unwrap();
    let credentials = provider_requests::credentials(fixture.root.path(), Some("openai"));
    let upstream = provider_requests::FakeUpstream::default();
    for attempt in 0..6 {
        thread::scope(|threads| {
            let peer = &fixture.peer;
            let current = &fixture.status;
            let worker = threads.spawn(move || lifecycle::answer_one_status_query(peer, current));
            let response = fixture.service.serve_provider_request(
                &mut fixture.session,
                &credentials,
                &upstream,
                &provider_requests::parsed(),
                2800,
                verify_fixture_signature,
            );
            worker.join().unwrap();
            if attempt < 5 {
                assert!(response.is_ok());
            } else {
                assert!(matches!(
                    response,
                    Err(BrokerError::ProviderBudgetExhausted)
                ));
            }
        });
    }
    let held = fixture.status(2900);
    assert_eq!(
        held.posture
            .dimensions
            .iter()
            .find(|d| d.dimension == DimensionName::Network)
            .unwrap()
            .state,
        DimensionState::Failed
    );
    fixture
        .service
        .extend_provider_budget(
            &fixture.session,
            CONTROLLER_UID,
            &louiselm_skills::broker::provider_extension::ExtensionRequest {
                request_id: "lift-hold".into(),
                additional_requests: 1,
                expires_at_ms: None,
            },
            3000,
        )
        .unwrap();
    assert!(fixture.service.provider_hold("run-1").unwrap().is_none());
    let lifted = fixture.status(3100);
    assert_eq!(
        lifted
            .posture
            .dimensions
            .iter()
            .find(|d| d.dimension == DimensionName::Network)
            .unwrap()
            .state,
        DimensionState::Failed
    );
    fixture.activate(3200).unwrap();
    assert_eq!(
        fixture.status(3300).posture,
        lifted.posture,
        "duplicate activation cannot cross a hold generation"
    );
}

#[test]
fn activation_consumed_during_status_uses_its_completed_observation_time() {
    let wall_now = || {
        u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis(),
        )
        .unwrap()
    };
    let mut fixture = fixture_at(true, wall_now() - 3000);
    let requested_at = wall_now();
    // The caller's timestamp precedes the queued source observation.
    thread::sleep(Duration::from_millis(10));
    let activated = response(ResponseResult::SenderGuardActivated {
        enrollment: fixture.enrollment.clone(),
    });
    settle(|done| fixture.peer.send(activated.canonical_bytes(), done));
    let status = thread::scope(|threads| {
        let peer = &fixture.peer;
        let current = &fixture.status;
        let answer = threads.spawn(move || {
            thread::sleep(Duration::from_millis(30));
            lifecycle::answer_one_status_query(peer, current);
        });
        let status = fixture
            .service
            .session_status(
                &mut fixture.session,
                &LifecycleCaller::Agent,
                requested_at,
                verify_fixture_signature,
            )
            .unwrap();
        answer.join().unwrap();
        status
    });
    let network = status
        .posture
        .dimensions
        .iter()
        .find(|d| d.dimension == DimensionName::Network)
        .unwrap();
    assert_eq!(network.state, DimensionState::Verified);
    assert!(network.freshness.last_verified_at_ms.unwrap() > requested_at);
}
