//! A skill quarantine reaches running Sessions pinned to an affected Generation
//! (`louiselm-d6fv.6.5.1`): real store, real signed Admission, fake supervisor.

use super::*;
use louiselm_skills::{
    Digest, Policy, Store,
    admission::{self, AdmissionMember, AdmissionRequest},
    beads_mutation::{
        ApprovedBeadsMutations, BeadsEffect, BeadsInspectionDetail, BeadsMutationKind,
        BeadsMutationOutcome, BeadsMutationRequest, BeadsRole,
    },
    broker::{
        BrokerSession, admission_source::AdmissionSource, lifecycle::LifecycleStore,
        skill_quarantine::TaintSource, verification::VerificationStatus,
    },
    dossier::ReviewDepth,
    launch_protocol::{
        COMMAND_SCHEMA, ChannelState, CommandMessage, CommandOperation, PendingAction,
        PendingOperation, PendingPhase, SupervisorStatus, VERIFICATION_SCHEMA,
        VerificationOperation, VerificationRequest,
    },
    launch_receipt::{ReceiptHead, SignedReceipt},
    quarantine,
    signer::SshKeygenSigner,
    sshsig::SkPolicy,
    trust::TrustStore,
    witness::GitWitness,
};
use std::os::unix::fs::PermissionsExt;

/// An operator store holding one admitted Generation of `member`, plus one
/// captured package outside it.
struct Supply {
    root: TempDir,
    store: Store,
    member: String,
    outsider: String,
    generation: String,
    source: AdmissionSource,
    _keys: support::Fixture,
}

fn supply() -> Supply {
    // Evidence ancestors must exclude untrusted writers; /tmp is refused.
    let root = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let keys = support::Fixture::new();
    let key = support::SshKey::generate(&keys, "primary");
    let release = support::SshKey::generate(&keys, "release");
    let store = Store::open(&root.path().join("supply")).unwrap();
    TrustStore::bootstrap(
        &store,
        "louiselm/skills",
        &key.public_key(),
        &release.public_key(),
        SkPolicy::none(),
        1000,
    )
    .unwrap();
    let mut packages = ["member", "outsider"].map(|name| {
        let candidate = keys.candidate(name);
        support::write_file(
            &candidate.join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: Test skill.\n---\nBody.\n"),
        );
        store
            .capture(&candidate, &Policy::embedded(), 1000)
            .unwrap()
            .0
            .digest
    });
    let signer = SshKeygenSigner::new(key.private_key_path());
    let record = admission::admit(
        &store,
        &Policy::embedded(),
        &AdmissionRequest {
            members: vec![AdmissionMember {
                package: packages[0].clone(),
                depth: ReviewDepth::Read,
                agents: vec!["codex".into()],
            }],
            signer: &signer,
            admitted_at_ms: 4000,
        },
    )
    .unwrap();
    let witness = GitWitness::new(
        &keys.witness_remote(),
        "skill-generations",
        &keys.path("witness-work"),
    );
    admission::witness(&store, &record.digest(), &witness, 4500).unwrap();
    admission::activate(&store, &record.digest(), 5000).unwrap();
    // Test-only installed source provenance, after the development fixture has
    // completed its real software-key Admission.
    fs::write(store.root().join("provenance.json"), br#"{"schema":"louiselm.skills.store-provenance/1","trusted":true,"created_by_release":"fixture"}"#).unwrap();
    skill_requests::protect_fixture_directories(store.root());
    let [member, outsider] = packages.each_mut().map(|digest| digest.to_string());
    Supply {
        source: AdmissionSource {
            store: store.root().into(),
            trust_domain: "louiselm/skills".into(),
            operator_uid: uid,
            broker_uid: uid + 1,
        },
        generation: record.generation,
        root,
        store,
        member,
        outsider,
        _keys: keys,
    }
}

struct Live {
    service: BrokerService,
    session: BrokerSession,
    peer: SeqpacketChannel,
    current: SupervisorStatus,
    lifecycle: LifecycleStore,
}

/// A running Session pinned to `generation`, with command authority granted.
fn launched(root: &Path, generation: &str) -> Live {
    launched_with_beads(root, generation, false)
}

fn launched_with_beads(root: &Path, generation: &str, beads: bool) -> Live {
    let socket = root.join("broker.sock");
    let mut request = request("quarantined");
    request.skill_generation_id = generation.into();
    let authorizations = AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap();
    let mut approval = grant(&request);
    if beads {
        approval.beads_mutations = Some(ApprovedBeadsMutations {
            role: BeadsRole::Worker,
            effects: vec![BeadsEffect::CommentAdd],
            project_digest: Digest::of(root.join("workspace").as_os_str().as_encoded_bytes())
                .to_string(),
            issue_ids: vec!["test-1".into()],
            max_mutations: 2,
            expires_at_ms: 60_000,
        });
    }
    authorizations.authorize(&approval, 1000).unwrap();
    let mut service = BrokerService::bind(
        &socket,
        authorizations,
        ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    if beads {
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.join(".beads")).unwrap();
        fs::write(workspace.join(".beads/beads.db"), []).unwrap();
        let program = Path::new("/bin/true");
        service
            .configure_beads_tracker(
                &workspace,
                program,
                &Digest::of(&fs::read(program).unwrap()),
            )
            .unwrap();
    }
    let peer = thread::spawn(move || fake_supervisor(&socket, &request, 2000));
    let session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let (binding, peer) = peer.join().unwrap();
    // The shared status fixture derives its head from the unpinned Launch receipt.
    let mut current = lifecycle::status(&binding);
    let head = service.receipts().head("quarantined").unwrap();
    current.launcher_head.clone_from(&head);
    current.broker_head = head;
    Live {
        current,
        lifecycle: LifecycleStore::open(&root.join("authorizations/lifecycle")).unwrap(),
        service,
        session,
        peer,
    }
}

impl Live {
    fn add_comment(&mut self, request_id: &str) -> CommandOperation {
        let auth = self.session.authorization();
        let message = CommandMessage {
            schema: COMMAND_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: format!("relay-{request_id}"),
            session_id: auth.session_id.clone(),
            run_id: auth.run_id.clone(),
            envelope_revision: auth.envelope_revision,
            operation: CommandOperation::BeadsMutation {
                request: BeadsMutationRequest {
                    request_id: request_id.into(),
                    required: false,
                    kind: BeadsMutationKind::CommentAdd {
                        issue_id: "test-1".into(),
                        text: "prior Session-authored content".into(),
                    },
                },
            },
        };
        thread::scope(|scope| {
            let peer = &self.peer;
            let status = self.current.clone();
            let worker = scope.spawn(move || {
                settle(|done| peer.send(message.canonical_bytes(), done));
                lifecycle::answer_one_status_query(peer, &status);
                let LauncherPacket::Request(ProtocolMessage::Command(reply)) =
                    settle(|done| peer.receive(done)).packet
                else {
                    panic!("Beads mutation reply")
                };
                reply.operation
            });
            assert!(
                !self
                    .service
                    .step(&mut self.session, 3000, None, verify_fixture_signature)
                    .unwrap()
            );
            worker.join().unwrap()
        })
    }

    /// A settle that must decide without any supervisor exchange.
    fn settle_quietly(
        &mut self,
        source: Option<&AdmissionSource>,
    ) -> Result<Option<SignedReceipt>, BrokerError> {
        self.service.settle_skill_quarantine(
            &mut self.session,
            source,
            3000,
            verify_fixture_signature,
        )
    }

    /// The supervisor confirms revocation, answers status, then serves the Park.
    fn settle_and_park(&mut self, source: &AdmissionSource) -> SignedReceipt {
        thread::scope(|scope| {
            let (peer, mut current) = (&self.peer, self.current.clone());
            let revoked = self.session.command_revocation_complete();
            let supervisor = scope.spawn(move || {
                if !revoked {
                    let packet = settle(|complete| peer.receive(complete));
                    let LauncherPacket::Request(ProtocolMessage::Command(mut message)) =
                        packet.packet
                    else {
                        panic!("command revocation before Park")
                    };
                    assert!(matches!(message.operation, CommandOperation::Revoke));
                    message.operation = CommandOperation::Revoked { enforced: true };
                    settle(|complete| peer.send(message.canonical_bytes(), complete));
                }
                current.channel_state = ChannelState::Revoked;
                lifecycle::answer_one_status_query(peer, &current);
                lifecycle::drive_lifecycle_peer(peer, &current)
            });
            let receipt = self
                .service
                .settle_skill_quarantine(
                    &mut self.session,
                    Some(source),
                    3000,
                    verify_fixture_signature,
                )
                .unwrap()
                .unwrap();
            assert_eq!(receipt, supervisor.join().unwrap());
            receipt
        })
    }

    fn quarantined(&self) -> bool {
        self.lifecycle.is_quarantined("quarantined").unwrap()
    }

    fn settle_while_park_is_pending(&mut self, source: &AdmissionSource) {
        thread::scope(|scope| {
            let (peer, mut current) = (&self.peer, self.current.clone());
            let supervisor = scope.spawn(move || {
                let packet = settle(|complete| peer.receive(complete));
                let LauncherPacket::Request(ProtocolMessage::Command(mut message)) = packet.packet
                else {
                    panic!("command revocation before status")
                };
                assert!(matches!(message.operation, CommandOperation::Revoke));
                message.operation = CommandOperation::Revoked { enforced: true };
                settle(|complete| peer.send(message.canonical_bytes(), complete));
                current.channel_state = ChannelState::Revoked;
                current.pending_operation = Some(PendingOperation {
                    request_id: "pending-park".into(),
                    action: PendingAction::Park,
                    phase: PendingPhase::Applying,
                });
                lifecycle::answer_one_status_query(peer, &current);
            });
            assert_eq!(
                self.service
                    .settle_skill_quarantine(
                        &mut self.session,
                        Some(source),
                        3000,
                        verify_fixture_signature
                    )
                    .unwrap(),
                None
            );
            supervisor.join().unwrap();
        });
        self.current.channel_state = ChannelState::Revoked;
    }
}

fn assert_unknown_beads_provenance(service: &BrokerService, uid: u32, operation_id: &str) {
    use louiselm_skills::workspace::provenance::OutputProvenanceCode;
    let inspection = service
        .beads_mutation_control(uid, operation_id, None)
        .unwrap();
    let BeadsInspectionDetail::Mutation {
        output_provenance, ..
    } = inspection.detail
    else {
        panic!("mutation inspection expected")
    };
    assert_eq!(output_provenance.code, OutputProvenanceCode::Unknown);
}

#[test]
fn completed_beads_mutation_keeps_audit_and_gains_taint_on_inspection() {
    use louiselm_skills::workspace::provenance::OutputProvenanceCode;
    let supply = supply();
    let mut live = launched_with_beads(supply.root.path(), &supply.generation, true);
    let uid = live.session.authorization().controller_uid;
    let CommandOperation::BeadsMutationResult { status } = live.add_comment("prior-comment") else {
        panic!("completed Beads mutation expected")
    };
    assert_eq!(status.outcome, BeadsMutationOutcome::Completed);
    let before = live
        .service
        .beads_mutation_control(uid, &status.operation_id, None)
        .unwrap();
    let BeadsInspectionDetail::Mutation {
        request_digest,
        output_provenance,
        ..
    } = &before.detail
    else {
        panic!("mutation inspection expected")
    };
    assert_eq!(output_provenance.code, OutputProvenanceCode::Untainted);
    let original_digest = request_digest.clone();
    let ledger = supply
        .root
        .path()
        .join("authorizations/beads-mutations/requests");
    let record = fs::read_dir(&ledger)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let original_record = fs::read(&record).unwrap();
    let original_database = fs::read(supply.root.path().join("workspace/.beads/beads.db")).unwrap();
    quarantine::exclude_everything(&supply.store, "compromised", 6000).unwrap();
    live.settle_and_park(&supply.source);
    let taint = live
        .service
        .inspect("quarantined")
        .unwrap()
        .unwrap()
        .output_taint
        .unwrap();
    let restarted = verification::reopen(supply.root.path(), "beads-restart.sock");
    let after = restarted
        .beads_mutation_control(uid, &status.operation_id, None)
        .unwrap();
    let BeadsInspectionDetail::Mutation {
        request_digest,
        status: after_status,
        output_provenance,
        ..
    } = &after.detail
    else {
        panic!("mutation inspection expected")
    };
    assert_eq!(request_digest, &original_digest);
    assert_eq!(after_status.outcome, BeadsMutationOutcome::Completed);
    assert_eq!(
        output_provenance.code,
        OutputProvenanceCode::SessionOutputTainted
    );
    assert_eq!(
        output_provenance.taint_digest.as_deref(),
        Some(taint.digest.as_str())
    );
    assert_eq!(fs::read(&record).unwrap(), original_record);
    assert_eq!(
        fs::read(supply.root.path().join("workspace/.beads/beads.db")).unwrap(),
        original_database
    );
    let portable = serde_json::to_string(output_provenance).unwrap();
    assert!(!portable.contains("quarantined"));
    assert!(!portable.contains("prior Session-authored content"));

    let taint_record = supply
        .root
        .path()
        .join("authorizations/lifecycle/quarantined/session-taint.json");
    fs::write(&taint_record, b"corrupt").unwrap();
    assert_unknown_beads_provenance(&restarted, uid, &status.operation_id);
    fs::remove_file(taint_record).unwrap();
    assert_unknown_beads_provenance(&restarted, uid, &status.operation_id);
    assert_eq!(fs::read(&record).unwrap(), original_record);
}

#[test]
fn quarantine_of_a_pinned_member_revokes_then_parks_only_that_session() {
    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    // No quarantine, then one that misses the pinned Generation: no effect, no exchange.
    assert_eq!(live.settle_quietly(Some(&supply.source)).unwrap(), None);
    quarantine::exclude(
        &supply.store,
        std::slice::from_ref(&supply.outsider),
        "unrelated",
        5000,
    )
    .unwrap();
    assert_eq!(live.settle_quietly(Some(&supply.source)).unwrap(), None);
    assert!(!live.quarantined());

    quarantine::exclude(
        &supply.store,
        std::slice::from_ref(&supply.member),
        "compromised",
        6000,
    )
    .unwrap();
    let source_digest = Digest::of(&fs::read(supply.store.root().join("quarantine.json")).unwrap());
    let receipt = live.settle_and_park(&supply.source);
    assert!(matches!(
        receipt.payload.outcome,
        louiselm_skills::launch_receipt::ReceiptOutcome::Park { .. }
    ));
    assert!(live.quarantined());
    assert!(live.session.command_revocation_complete());
    let taint = live
        .service
        .inspect("quarantined")
        .unwrap()
        .unwrap()
        .output_taint
        .unwrap();
    assert_eq!(taint.skill_generation_id, supply.generation);
    assert_eq!(
        taint.source,
        TaintSource::Quarantine {
            digest: source_digest.to_string()
        }
    );
    assert_eq!(taint.detected_at_ms, 3000);
    assert_eq!(
        taint.park_receipt.as_ref().unwrap().digest,
        receipt.digest().to_string()
    );
    assert!(!format!("{taint:?}").contains("quarantined"));
    assert_eq!(
        live.service
            .audit()
            .unwrap()
            .iter()
            .filter(|entry| entry.decision == AuditDecision::SessionOutputTainted)
            .count(),
        1
    );
    // Settled: later ticks do nothing, with no supervisor exchange.
    assert_eq!(live.settle_quietly(Some(&supply.source)).unwrap(), None);
    assert_eq!(
        live.service
            .inspect("quarantined")
            .unwrap()
            .unwrap()
            .output_taint,
        Some(taint)
    );
    assert!(
        LifecycleStore::open(&supply.root.path().join("authorizations/lifecycle"))
            .unwrap()
            .is_quarantined("quarantined")
            .unwrap()
    );
}

#[test]
fn workspace_provenance_covers_prior_output_after_restart_and_verification() {
    use louiselm_skills::workspace::provenance::OutputProvenanceCode;

    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    let uid = live.session.authorization().controller_uid;
    let before = live
        .service
        .workspace_retention(uid, "quarantined", None)
        .unwrap();
    assert_eq!(
        before.output_provenance.code,
        OutputProvenanceCode::Untainted
    );
    quarantine::exclude_everything(&supply.store, "compromised", 6000).unwrap();
    live.settle_and_park(&supply.source);
    let taint = live
        .service
        .inspect("quarantined")
        .unwrap()
        .unwrap()
        .output_taint
        .unwrap();
    let restarted = verification::reopen(supply.root.path(), "restarted.sock");
    let retained = restarted
        .workspace_retention(uid, "quarantined", None)
        .unwrap();
    assert_eq!(
        retained.output_provenance.code,
        OutputProvenanceCode::SessionOutputTainted
    );
    assert_eq!(
        retained.output_provenance.taint_digest.as_deref(),
        Some(taint.digest.as_str())
    );
    let portable = serde_json::to_value(&retained.output_provenance).unwrap();
    assert_eq!(portable.as_object().unwrap().len(), 4);
    assert!(!portable.to_string().contains("quarantined"));
    assert!(!portable.to_string().contains("compromised"));

    let verifier = request("verifier");
    consumed_authorization(supply.root.path(), &verifier);
    let directory = supply.root.path().join("authorizations/verification");
    fs::create_dir_all(&directory).unwrap();
    let intent = VerificationRequest {
        schema: VERIFICATION_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "verify-tainted".into(),
        launch: verifier,
        head: ReceiptHead {
            sequence: 1,
            digest: Digest::of(b"head").to_string(),
        },
        expires_at_ms: 30_000,
        operation: VerificationOperation::Run {
            producer_session_id: "quarantined".into(),
            export_request_id: "export".into(),
            export_digest: Digest::of(b"export").to_string(),
            job_digest: Digest::of(b"job").to_string(),
        },
    };
    fs::write(
        directory.join("intent-verifier.json"),
        intent.canonical_bytes(),
    )
    .unwrap();
    assert!(matches!(
        restarted.verification_status("verifier").unwrap(),
        VerificationStatus::Quarantined { output_provenance }
            if output_provenance.taint_digest.as_deref() == Some(taint.digest.as_str())
    ));
}

#[test]
fn completed_promotion_remains_visible_when_its_producer_becomes_tainted() {
    use louiselm_skills::broker::promotion::PromotionStatus;
    use louiselm_skills::workspace::provenance::OutputProvenanceCode;

    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    let mut promotion = promotion::selection();
    promotion.producer_session_id = "quarantined".into();
    promotion.request_id = "historical".into();
    let journal = supply
        .root
        .path()
        .join("authorizations/promotions/historical");
    fs::create_dir_all(&journal).unwrap();
    fs::write(
        journal.join("request.json"),
        serde_json::to_vec(&promotion).unwrap(),
    )
    .unwrap();
    fs::write(journal.join("0.grant"), b"0").unwrap();
    fs::write(journal.join("0.done"), b"0").unwrap();
    fs::write(
        journal.join("result.json"),
        serde_json::to_vec(&louiselm_skills::workspace::promotion::ApplicationResult {
            complete: true,
            completed_steps: 1,
            output_provenance: serde_json::from_value(serde_json::json!({
                "schema": "louiselm.workspace.output-provenance/1",
                "code": "untainted",
                "taint_digest": null,
                "clean_review_refs": []
            }))
            .unwrap(),
        })
        .unwrap(),
    )
    .unwrap();
    assert!(matches!(
        live.service.promotion_status("historical").unwrap(),
        PromotionStatus::Completed { output_provenance, .. }
            if output_provenance.code == OutputProvenanceCode::Untainted
    ));

    quarantine::exclude_everything(&supply.store, "compromised", 6000).unwrap();
    live.settle_and_park(&supply.source);
    let digest = live
        .service
        .inspect("quarantined")
        .unwrap()
        .unwrap()
        .output_taint
        .unwrap()
        .digest;
    let restarted = verification::reopen(supply.root.path(), "restarted.sock");
    assert!(matches!(
        restarted.promotion_status("historical").unwrap(),
        PromotionStatus::Completed { output_provenance, result }
            if result.complete && result.completed_steps == 1
                && output_provenance.taint_digest.as_deref() == Some(digest.as_str())
    ));
    fs::write(
        supply
            .root
            .path()
            .join("authorizations/lifecycle/quarantined/session-taint.json"),
        b"{}",
    )
    .unwrap();
    assert!(matches!(
        restarted.promotion_status("historical").unwrap(),
        PromotionStatus::Completed { output_provenance, result }
            if result.complete && output_provenance.code == OutputProvenanceCode::Unknown
    ));
}

#[test]
fn exact_tainted_review_remains_in_promotion_result_and_refuses_changed_binding() {
    use louiselm_skills::{
        broker::promotion::{PromotionReview, PromotionStatus},
        workspace::{promotion::ApplicationResult, provenance::OutputProvenanceCode},
    };

    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    quarantine::exclude_everything(&supply.store, "compromised", 6000).unwrap();
    live.settle_and_park(&supply.source);
    let taint_digest = live
        .service
        .inspect("quarantined")
        .unwrap()
        .unwrap()
        .output_taint
        .unwrap()
        .digest;
    let mut request = promotion::selection();
    request.producer_session_id = "quarantined".into();
    request.request_id = "reviewed-once".into();
    let mut review = PromotionReview {
        schema: "louiselm.workspace.promotion-review/1".into(),
        request_digest: Digest::of(&serde_json::to_vec(&request).unwrap()).to_string(),
        output_digest: request.job.result_digest.clone(),
        taint_digest: taint_digest.clone(),
        action: "workspace_promotion".into(),
        destination: request.destination,
    };
    let review_digest = review.digest().unwrap();
    let journal = supply
        .root
        .path()
        .join("authorizations/promotions/reviewed-once");
    fs::create_dir_all(&journal).unwrap();
    fs::write(
        journal.join("request.json"),
        serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    fs::write(
        journal.join("review.json"),
        serde_json::to_vec(&review).unwrap(),
    )
    .unwrap();
    fs::write(journal.join("0.grant"), b"0").unwrap();
    fs::write(journal.join("0.done"), b"0").unwrap();
    let result = ApplicationResult {
        complete: true,
        completed_steps: 1,
        output_provenance: serde_json::from_value(serde_json::json!({
            "schema": "louiselm.workspace.output-provenance/1",
            "code": "session_output_tainted",
            "taint_digest": taint_digest,
            "clean_review_refs": [review_digest.clone()]
        }))
        .unwrap(),
    };
    fs::write(
        journal.join("result.json"),
        serde_json::to_vec(&result).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        live.service.promotion_status("reviewed-once").unwrap(),
        PromotionStatus::Completed { output_provenance, result: actual }
            if actual == result
                && output_provenance.code == OutputProvenanceCode::SessionOutputTainted
                && output_provenance.clean_review_refs == [review_digest]
    ));
    review.destination.inode += 1;
    fs::write(
        journal.join("review.json"),
        serde_json::to_vec(&review).unwrap(),
    )
    .unwrap();
    assert!(live.service.promotion_status("reviewed-once").is_err());
}

#[test]
fn excluding_everything_reaches_only_sessions_pinned_to_that_generation() {
    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    let replacement_root = supply.root.path().join("replacement-session");
    fs::create_dir(&replacement_root).unwrap();
    let mut replacement = launched(
        &replacement_root,
        &Digest::of(b"replacement-generation").to_string(),
    );
    quarantine::exclude_everything(&supply.store, "incident", 5000).unwrap();
    live.settle_and_park(&supply.source);
    assert!(live.quarantined());
    assert_eq!(
        replacement.settle_quietly(Some(&supply.source)).unwrap(),
        None
    );
    assert!(!replacement.quarantined());
}

#[test]
fn unreadable_quarantine_immediately_fails_closed() {
    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    let path = supply.store.root().join("quarantine.json");
    fs::write(&path, b"{ not json").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    live.settle_and_park(&supply.source);
    assert!(live.quarantined());
    let taint = live
        .service
        .inspect("quarantined")
        .unwrap()
        .unwrap()
        .output_taint
        .unwrap();
    assert_eq!(taint.source, TaintSource::EvidenceUnreadable);
    assert!(taint.park_receipt.is_some());
}

#[test]
fn missing_or_corrupt_taint_after_detection_refuses_inspection_and_capabilities() {
    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    quarantine::exclude_everything(&supply.store, "incident", 5000).unwrap();
    live.settle_and_park(&supply.source);
    let path = supply
        .root
        .path()
        .join("authorizations/lifecycle/quarantined/session-taint.json");
    fs::write(&path, b"{}").unwrap();
    assert!(live.lifecycle.is_quarantined("quarantined").is_err());
    assert!(live.service.inspect("quarantined").is_err());
    assert!(
        live.service
            .workspace_retention(
                live.session.authorization().controller_uid,
                "quarantined",
                None
            )
            .is_err()
    );
    fs::remove_file(&path).unwrap();
    assert!(live.lifecycle.is_quarantined("quarantined").is_err());
    assert!(live.service.inspect("quarantined").is_err());
}

#[test]
fn taint_persists_before_park_and_later_links_the_signed_receipt() {
    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    quarantine::exclude_everything(&supply.store, "incident", 5000).unwrap();
    live.settle_while_park_is_pending(&supply.source);
    assert!(live.quarantined());
    let before = live
        .service
        .inspect("quarantined")
        .unwrap()
        .unwrap()
        .output_taint
        .unwrap();
    assert!(before.park_receipt.is_none());

    let receipt = live.settle_and_park(&supply.source);
    let after = live
        .service
        .inspect("quarantined")
        .unwrap()
        .unwrap()
        .output_taint
        .unwrap();
    assert_eq!(after.digest, before.digest);
    assert_eq!(after.detected_at_ms, before.detected_at_ms);
    assert_eq!(
        after.park_receipt.unwrap().digest,
        receipt.digest().to_string()
    );
}

#[test]
fn prior_quarantine_does_not_hide_a_later_skill_taint() {
    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    live.lifecycle.quarantine("quarantined").unwrap();
    quarantine::exclude_everything(&supply.store, "incident", 5000).unwrap();
    live.settle_and_park(&supply.source);
    assert!(
        live.service
            .inspect("quarantined")
            .unwrap()
            .unwrap()
            .output_taint
            .is_some()
    );
    fs::remove_file(
        supply
            .root
            .path()
            .join("authorizations/lifecycle/quarantined/session-taint.json"),
    )
    .unwrap();
    assert!(live.lifecycle.is_quarantined("quarantined").is_err());
}

#[test]
fn failed_park_keeps_taint_without_claiming_a_signed_receipt() {
    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    quarantine::exclude_everything(&supply.store, "incident", 5000).unwrap();
    thread::scope(|scope| {
        let peer = &live.peer;
        let supervisor = scope.spawn(move || {
            let packet = settle(|complete| peer.receive(complete));
            let LauncherPacket::Request(ProtocolMessage::Command(mut message)) = packet.packet
            else {
                panic!("command revocation before Park")
            };
            assert!(matches!(message.operation, CommandOperation::Revoke));
            message.operation = CommandOperation::Revoked { enforced: true };
            settle(|complete| peer.send(message.canonical_bytes(), complete));
            peer.close();
        });
        assert!(
            live.service
                .settle_skill_quarantine(
                    &mut live.session,
                    Some(&supply.source),
                    3000,
                    verify_fixture_signature,
                )
                .is_err()
        );
        supervisor.join().unwrap();
    });
    assert!(live.quarantined());
    assert!(live.session.channel().is_closed());
    let taint = live
        .service
        .inspect("quarantined")
        .unwrap()
        .unwrap()
        .output_taint
        .unwrap();
    assert!(taint.park_receipt.is_none());
}

#[test]
fn without_an_installed_admission_source_nothing_is_observable() {
    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    quarantine::exclude_everything(&supply.store, "incident", 5000).unwrap();
    assert_eq!(live.settle_quietly(None).unwrap(), None);
    assert!(!live.quarantined());
}
