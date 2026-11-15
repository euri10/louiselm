//! Real broker channel, durable operator decisions and subject cleanup.
use super::*;
use louiselm_skills::{
    broker::{
        BrokerSession,
        attention::{AttentionSubject, Outbox, ProjectionChange},
    },
    launch_protocol::{COMMAND_SCHEMA, CommandMessage, CommandOperation, LifecycleAction},
    skill_request::{
        ApprovedSkillRequests, SkillRequest, SkillRequestOutcome, SkillRequestStatus, SkillSubject,
    },
};
use std::path::PathBuf;

fn query(auth: &LaunchAuthorization) -> CommandMessage {
    CommandMessage {
        schema: COMMAND_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "relay-1".into(),
        session_id: auth.session_id.clone(),
        run_id: auth.run_id.clone(),
        envelope_revision: auth.envelope_revision,
        operation: CommandOperation::SkillRequest {
            request: SkillRequest {
                request_id: "stable-request".into(),
                subject: SkillSubject::Session,
                packages: vec![Digest::of(b"exact-package").to_string()],
                agents: vec!["codex".into()],
            },
        },
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ProfileCase {
    Single,
    Packages,
    Sources,
    SignedSource,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RaceWinner {
    Decision,
    Signing,
    FailedSigner,
}

#[test]
fn cancelled_profile_request_cannot_be_signed_by_a_paused_ceremony() {
    for decision in [
        SkillRequestOutcome::Cancelled,
        SkillRequestOutcome::Rejected,
    ] {
        paused_profile_ordering(ProfileCase::Single, RaceWinner::Decision, decision);
    }
}

#[test]
fn cancelled_profile_batch_cannot_be_signed_by_a_paused_ceremony() {
    for decision in [
        SkillRequestOutcome::Cancelled,
        SkillRequestOutcome::Rejected,
    ] {
        paused_profile_ordering(ProfileCase::Packages, RaceWinner::Decision, decision);
    }
}

#[test]
fn signed_profile_request_cannot_be_cancelled() {
    for decision in [
        SkillRequestOutcome::Cancelled,
        SkillRequestOutcome::Rejected,
    ] {
        paused_profile_ordering(ProfileCase::Single, RaceWinner::Signing, decision);
    }
}

#[test]
fn signed_profile_batch_cannot_be_cancelled() {
    for decision in [
        SkillRequestOutcome::Cancelled,
        SkillRequestOutcome::Rejected,
    ] {
        paused_profile_ordering(ProfileCase::Packages, RaceWinner::Signing, decision);
    }
}

#[test]
fn failed_profile_signer_stays_pending_across_broker_restart() {
    paused_profile_ordering(
        ProfileCase::Single,
        RaceWinner::FailedSigner,
        SkillRequestOutcome::Cancelled,
    );
}

#[test]
fn failed_batch_signer_keeps_all_sources_pending_after_restart() {
    paused_profile_ordering(
        ProfileCase::Sources,
        RaceWinner::FailedSigner,
        SkillRequestOutcome::Cancelled,
    );
}

#[test]
fn cancelled_source_prevents_a_batch_signature() {
    for decision in [
        SkillRequestOutcome::Cancelled,
        SkillRequestOutcome::Rejected,
    ] {
        paused_profile_ordering(ProfileCase::Sources, RaceWinner::Decision, decision);
    }
}

#[test]
fn signed_batch_supersedes_its_pending_sources() {
    for decision in [
        SkillRequestOutcome::Cancelled,
        SkillRequestOutcome::Rejected,
    ] {
        paused_profile_ordering(ProfileCase::Sources, RaceWinner::Signing, decision);
    }
}

#[test]
fn an_already_signed_source_cannot_be_superseded_by_a_new_batch() {
    paused_profile_ordering(
        ProfileCase::SignedSource,
        RaceWinner::Decision,
        SkillRequestOutcome::Cancelled,
    );
}

#[expect(
    clippy::too_many_lines,
    reason = "One fixture drives both orderings through the real broker and Admission signer."
)]
fn paused_profile_ordering(case: ProfileCase, winner: RaceWinner, decision: SkillRequestOutcome) {
    use louiselm_skills::{
        Policy, ReviewDepth, Signer, Store,
        admission::{self, AdmissionMember, AdmissionRequest},
        broker::admission_source::AdmissionSource,
        dossier::{Dossier, DossierRequest},
        signer::{SignerError, SshKeygenSigner},
        sshsig::SkPolicy,
        trust::TrustStore,
    };
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc,
    };

    struct PausedSigner<'a> {
        signer: &'a SshKeygenSigner,
        signing_first: bool,
        fail_signer: bool,
        reached: mpsc::SyncSender<()>,
        resume: mpsc::Receiver<()>,
        signed: &'a AtomicUsize,
    }
    impl PausedSigner<'_> {
        fn pause(&self) {
            self.reached.send(()).unwrap();
            self.resume.recv().unwrap();
        }
    }
    impl Signer for PausedSigner<'_> {
        fn sign(&self, namespace: &str, payload: &[u8]) -> Result<String, SignerError> {
            if self.signing_first {
                self.pause();
            }
            if self.fail_signer {
                return Err(SignerError::Failed("fixture signer failed".into()));
            }
            let signature = self.signer.sign(namespace, payload)?;
            self.signed.fetch_add(1, Ordering::SeqCst);
            Ok(signature)
        }
    }
    struct NoRetrySignature;
    impl Signer for NoRetrySignature {
        fn sign(&self, _: &str, _: &[u8]) -> Result<String, SignerError> {
            Err(SignerError::Failed("retry must not sign".into()))
        }
    }

    let batch = case != ProfileCase::Single;
    let signing_first = winner != RaceWinner::Decision;
    let fail_signer = winner == RaceWinner::FailedSigner;
    let with_sources = matches!(case, ProfileCase::Sources | ProfileCase::SignedSource);
    let source_signed = case == ProfileCase::SignedSource;

    let root = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let (service, mut session, peer) = fixture_for(root.path(), true, false, true, uid);
    let key_fixture = support::Fixture::new();
    let key = support::SshKey::generate(&key_fixture, "primary");
    let release = support::SshKey::generate(&key_fixture, "release");
    let store = Store::open(&root.path().join("supply")).unwrap();
    TrustStore::bootstrap(
        &store,
        "louiselm/skills",
        &key.public_key(),
        &release.public_key(),
        SkPolicy::none(),
        1,
    )
    .unwrap();
    fs::write(store.root().join("provenance.json"), br#"{"schema":"louiselm.skills.store-provenance/1","trusted":true,"created_by_release":"fixture"}"#).unwrap();
    let profile = key_fixture.candidate("profile");
    support::write_file(
        &profile.join("PROFILE.md"),
        "---\ntype: louiselm/operator-profile\n---\nPrivate fixture.\n",
    );
    let profile = store
        .capture(&profile, &Policy::embedded(), 1)
        .unwrap()
        .0
        .digest;
    let reviewed = Dossier::build(
        &store,
        &Policy::embedded(),
        &DossierRequest::new(&profile).with_review_depth(ReviewDepth::Read),
    )
    .unwrap()
    .portable_digest();
    let mut packages = vec![profile];
    if batch {
        let skill = key_fixture.candidate("skill");
        support::write_file(
            &skill.join("SKILL.md"),
            "---\nname: skill\ndescription: Test skill.\n---\nBody.\n",
        );
        packages.push(
            store
                .capture(&skill, &Policy::embedded(), 1)
                .unwrap()
                .0
                .digest,
        );
    }
    packages.sort_by_key(ToString::to_string);
    let mut message = query(session.authorization());
    let mut source_operations = Vec::new();
    let mut source_packages = Vec::new();
    if with_sources {
        for index in 0..2 {
            let candidate = key_fixture.candidate(&format!("source-profile-{index}"));
            support::write_file(
                &candidate.join("PROFILE.md"),
                &format!("---\ntype: louiselm/operator-profile\n---\nSource {index}.\n"),
            );
            let source_package = store
                .capture(&candidate, &Policy::embedded(), 1)
                .unwrap()
                .0
                .digest;
            let CommandOperation::SkillRequest { request } = &mut message.operation else {
                unreachable!()
            };
            request.request_id = format!("source-{index}");
            request.subject = SkillSubject::Supply;
            request.packages = vec![source_package.to_string()];
            let operation =
                accepted(exchange(&service, &mut session, &peer, &message, true)).operation_id;
            source_operations.push(operation.clone());
            source_packages.push((operation, source_package));
        }
        source_operations.sort();
    }
    let CommandOperation::SkillRequest { request } = &mut message.operation else {
        unreachable!()
    };
    request.request_id = "batch-target".into();
    request.subject = SkillSubject::Supply;
    request.packages = packages.iter().map(ToString::to_string).collect();
    let pending = accepted(exchange(&service, &mut session, &peer, &message, true));
    assert_eq!(
        service
            .skill_request_control(uid, &pending.operation_id, None)
            .unwrap()
            .outcome,
        SkillRequestOutcome::Pending
    );

    let (reached_send, reached_recv) = mpsc::sync_channel(0);
    let (resume_send, resume_recv) = mpsc::sync_channel(0);
    let signature_count = AtomicUsize::new(0);
    let signer = SshKeygenSigner::new(key.private_key_path());
    if source_signed {
        let (operation, package) = &source_packages[0];
        let reviewed_source = Dossier::build(
            &store,
            &Policy::embedded(),
            &DossierRequest::new(package).with_review_depth(ReviewDepth::Read),
        )
        .unwrap()
        .portable_digest();
        admission::admit_linked(
            &store,
            &Policy::embedded(),
            &AdmissionRequest {
                members: vec![AdmissionMember {
                    package: package.clone(),
                    depth: ReviewDepth::Read,
                    agents: vec!["codex".into()],
                }],
                signer: &signer,
                admitted_at_ms: 2,
                configured_agents: Some(vec!["codex".into()]),
            },
            operation,
            Some(&reviewed_source),
            &|| Ok(()), // Fixture for a record signed before reservations existed.
        )
        .unwrap();
    }
    fs::create_dir_all(store.root().join("generations")).unwrap();
    protect_fixture_directories(store.root());
    let source = AdmissionSource {
        store: store.root().into(),
        trust_domain: "louiselm/skills".into(),
        operator_uid: uid,
        broker_uid: uid + 1,
    };
    if source_signed {
        assert!(matches!(
            service.reserve_skill_signing(uid, &pending.operation_id, &source_operations, &source),
            Err(BrokerError::RequestMismatch)
        ));
        service
            .reconcile_skill_admissions(None, Some(&source), verify_fixture_signature)
            .unwrap();
        assert_eq!(
            service
                .skill_request_control(uid, &source_packages[0].0, None)
                .unwrap()
                .outcome,
            SkillRequestOutcome::Approved
        );
        assert_eq!(
            service
                .skill_request_control(uid, &pending.operation_id, None)
                .unwrap()
                .outcome,
            SkillRequestOutcome::Pending
        );
        return;
    }
    let (settled, result) = thread::scope(|scope| {
        let worker = scope.spawn(|| {
            let paused = PausedSigner {
                signer: &signer,
                signing_first,
                fail_signer,
                reached: reached_send,
                resume: resume_recv,
                signed: &signature_count,
            };
            admission::admit_linked(
                &store,
                &Policy::embedded(),
                &AdmissionRequest {
                    members: packages
                        .into_iter()
                        .map(|package| AdmissionMember {
                            package,
                            depth: ReviewDepth::Read,
                            agents: vec!["codex".into()],
                        })
                        .collect(),
                    signer: &paused,
                    admitted_at_ms: 2,
                    configured_agents: Some(vec!["codex".into()]),
                },
                &pending.operation_id,
                Some(&reviewed),
                &|| {
                    if !signing_first {
                        paused.pause();
                    }
                    service
                        .reserve_skill_signing(
                            uid,
                            &pending.operation_id,
                            &source_operations,
                            &source,
                        )
                        .map(|_| ())
                        .map_err(|_| admission::AdmissionError::SigningReservation)
                },
            )
        });
        reached_recv.recv().unwrap();
        let decision_operation = source_operations.first().unwrap_or(&pending.operation_id);
        let settled = service.skill_request_control(uid, decision_operation, Some(decision));
        resume_send.send(()).unwrap();
        let result = worker.join().unwrap();
        (settled.unwrap(), result)
    });
    if signing_first {
        assert_eq!(settled.outcome, SkillRequestOutcome::Pending);
        if fail_signer {
            assert!(
                matches!(result, Err(admission::AdmissionError::Signer(SignerError::Failed(message))) if message == "fixture signer failed")
            );
            assert_eq!(signature_count.load(Ordering::SeqCst), 0);
            peer.close();
            drop(session);
            drop(service);
            let restarted = BrokerService::bind(
                &root.path().join("restarted-signing.sock"),
                AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap(),
                ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
                AuditLog::open(&root.path().join("audit")).unwrap(),
                local_pin(),
            )
            .unwrap();
            assert_eq!(
                restarted
                    .skill_request_control(uid, &pending.operation_id, None)
                    .unwrap()
                    .outcome,
                SkillRequestOutcome::Pending
            );
            assert!(matches!(
                restarted.reserve_skill_signing(
                    uid,
                    &pending.operation_id,
                    &source_operations,
                    &source,
                ),
                Err(BrokerError::RequestMismatch)
            ));
            assert_eq!(
                restarted
                    .skill_request_control(uid, &pending.operation_id, Some(decision))
                    .unwrap()
                    .outcome,
                SkillRequestOutcome::Pending
            );
            for operation in &source_operations {
                assert_eq!(
                    restarted
                        .skill_request_control(uid, operation, Some(decision))
                        .unwrap()
                        .outcome,
                    SkillRequestOutcome::Pending
                );
            }
            return;
        }
        assert_eq!(signature_count.load(Ordering::SeqCst), 1);
        let record = result.unwrap();
        let retry = admission::admit_linked(
            &store,
            &Policy::embedded(),
            &AdmissionRequest {
                members: pending
                    .packages
                    .iter()
                    .map(|package| AdmissionMember {
                        package: Digest::parse(package).unwrap(),
                        depth: ReviewDepth::Read,
                        agents: pending.agents.clone(),
                    })
                    .collect(),
                signer: &NoRetrySignature,
                admitted_at_ms: 2,
                configured_agents: Some(vec!["codex".into()]),
            },
            &pending.operation_id,
            None,
            &|| Err(admission::AdmissionError::SigningReservation),
        )
        .unwrap();
        assert_eq!(retry.generation, record.generation);
        protect_fixture_directories(store.root());
        assert_eq!(
            admission::verify_linked(
                &store,
                &Policy::embedded(),
                "louiselm/skills",
                &pending.operation_id,
                &pending.packages,
                &pending.agents,
            )
            .unwrap(),
            Some(record.generation.clone())
        );
        let verify_outcomes = |broker: &BrokerService| {
            broker
                .reconcile_skill_admissions(None, Some(&source), verify_fixture_signature)
                .unwrap();
            let approved = broker
                .skill_request_control(uid, &pending.operation_id, None)
                .unwrap();
            assert_eq!(approved.outcome, SkillRequestOutcome::Approved);
            assert_eq!(
                approved.admission.as_deref(),
                Some(record.generation.as_str())
            );
            for operation in &source_operations {
                let superseded = broker.skill_request_control(uid, operation, None).unwrap();
                assert_eq!(superseded.outcome, SkillRequestOutcome::Superseded);
                assert_eq!(
                    superseded.admission.as_deref(),
                    Some(record.generation.as_str())
                );
                assert_ne!(superseded.packages, pending.packages);
            }
        };
        if with_sources {
            peer.close();
            drop(session);
            drop(service);
            let restarted = BrokerService::bind(
                &root.path().join("restarted-batch.sock"),
                AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap(),
                ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
                AuditLog::open(&root.path().join("audit")).unwrap(),
                local_pin(),
            )
            .unwrap();
            assert_eq!(
                restarted
                    .inspect_skill_signing(uid, &pending.operation_id, &source_operations)
                    .unwrap()
                    .outcome,
                SkillRequestOutcome::Pending
            );
            assert!(matches!(
                restarted.inspect_skill_signing(uid, &pending.operation_id, &[]),
                Err(BrokerError::RequestMismatch)
            ));
            verify_outcomes(&restarted);
            assert_eq!(
                restarted
                    .inspect_skill_signing(uid, &pending.operation_id, &source_operations)
                    .unwrap()
                    .outcome,
                SkillRequestOutcome::Approved
            );
            verify_outcomes(&restarted);
        } else {
            verify_outcomes(&service);
        }
        let outbox = Outbox::open(&root.path().join("authorizations/attention-outbox")).unwrap();
        while let Some(entry) = outbox.next().unwrap() {
            let wire = entry.wire().to_string();
            assert!(!wire.contains("Private fixture"));
            assert!(!wire.contains("Source 0"));
            assert!(!wire.contains(store.root().to_str().unwrap()));
            for package in &pending.packages {
                assert!(!wire.contains(package));
            }
            for (_, package) in &source_packages {
                assert!(!wire.contains(&package.to_string()));
            }
            outbox.acknowledge(entry.sequence, &entry.digest()).unwrap();
        }
    } else {
        assert_eq!(settled.outcome, decision);
        assert_eq!(
            signature_count.load(Ordering::SeqCst),
            0,
            "a terminal decision must prevent a new signature"
        );
        assert!(matches!(
            result,
            Err(admission::AdmissionError::SigningReservation)
        ));
    }
}

fn fixture(root: &Path, permitted: bool) -> (BrokerService, BrokerSession, SeqpacketChannel) {
    fixture_with_run(root, permitted, false)
}

fn fixture_with_run(
    root: &Path,
    permitted: bool,
    allow_run: bool,
) -> (BrokerService, BrokerSession, SeqpacketChannel) {
    fixture_for(root, permitted, allow_run, false, CONTROLLER_UID)
}

fn fixture_for(
    root: &Path,
    permitted: bool,
    allow_run: bool,
    allow_supply: bool,
    uid: u32,
) -> (BrokerService, BrokerSession, SeqpacketChannel) {
    let socket = root.join("broker.sock");
    let mut request = request("skill-session");
    request.run_id = "12345678-1234-4234-8234-123456789abc".into();
    let mut approval = grant(&request);
    approval.controller_uid = uid;
    approval.skill_requests = permitted.then(|| ApprovedSkillRequests {
        agents: vec!["codex".into()],
        allow_run,
        allow_supply,
        expires_at_ms: 60000,
    });
    let authorizations = AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap();
    authorizations.authorize(&approval, 1000).unwrap();
    let service = BrokerService::bind(
        &socket,
        authorizations,
        ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    let peer = thread::spawn(move || fake_supervisor_for(&socket, &request, 2000, uid));
    let session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    (service, session, peer.join().unwrap().1)
}

#[test]
fn verified_admission_resolves_pending_witness_and_terminal_decisions_win() {
    for subject in [SkillSubject::Session, SkillSubject::Supply] {
        assert_verified_admission(subject, &mut |_| {});
    }
}

#[test]
#[ignore = "requires scripts/test-skill-requests capture-service and Neovim"]
fn verified_admission_reaches_neovim() {
    let mut observer = crate::attention_delivery::Observer::new();
    assert_verified_admission(SkillSubject::Session, &mut |outbox| {
        observer.drain(outbox);
    });
}

#[test]
#[ignore = "requires scripts/test-skill-requests capture-service and Neovim"]
fn supply_admission_reaches_neovim() {
    let mut observer = crate::attention_delivery::Observer::new();
    assert_verified_admission(SkillSubject::Supply, &mut |outbox| {
        observer.drain(outbox);
    });
}

#[expect(
    clippy::too_many_lines,
    reason = "One real-signing scenario follows approval and competing terminal decisions through the same broker owner."
)]
fn assert_verified_admission(subject: SkillSubject, observe: &mut dyn FnMut(&Outbox)) {
    use louiselm_skills::{
        Policy, Store,
        admission::{self, AdmissionMember, AdmissionRequest},
        broker::admission_source::AdmissionSource,
        dossier::{Dossier, DossierRequest, ReviewDepth},
        signer::SshKeygenSigner,
        sshsig::SkPolicy,
        trust::TrustStore,
    };
    use std::os::unix::fs::PermissionsExt;
    // Evidence ancestors must exclude untrusted writers; /tmp is deliberately refused.
    let root = tempfile::tempdir_in(std::env::var_os("HOME").unwrap()).unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let uid = rustix::process::geteuid().as_raw();
    let (service, mut session, peer) = fixture_for(
        root.path(),
        true,
        false,
        subject == SkillSubject::Supply,
        uid,
    );
    let outbox = Outbox::open(&root.path().join("authorizations/attention-outbox")).unwrap();
    let key_fixture = support::Fixture::new();
    let key = support::SshKey::generate(&key_fixture, "primary");
    let release = support::SshKey::generate(&key_fixture, "release");
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
    // Test-only trusted provenance, with real software-key Admission and verification.
    fs::write(store.root().join("provenance.json"), br#"{"schema":"louiselm.skills.store-provenance/1","trusted":true,"created_by_release":"fixture"}"#).unwrap();
    let candidate = key_fixture.candidate("skill");
    support::write_file(
        &candidate.join("SKILL.md"),
        "---\nname: skill\ndescription: Test skill.\n---\nBody.\n",
    );
    let (package, _) = store
        .capture(&candidate, &Policy::embedded(), 1000)
        .unwrap();
    let mut packages = vec![package.digest];
    let mut reviewed_profile = None;
    if subject == SkillSubject::Supply {
        let candidate = key_fixture.candidate("profile");
        support::write_file(
            &candidate.join("PROFILE.md"),
            "---\ntype: louiselm/operator-profile\n---\nPrivate profile fixture.\nSecond proposed change.\n",
        );
        let digest = store
            .capture(&candidate, &Policy::embedded(), 1000)
            .unwrap()
            .0
            .digest;
        reviewed_profile = Some(
            Dossier::build(
                &store,
                &Policy::embedded(),
                &DossierRequest::new(&digest).with_review_depth(ReviewDepth::Read),
            )
            .unwrap()
            .portable_digest(),
        );
        packages.push(digest);
    }
    packages.sort_by_key(ToString::to_string);
    let source = AdmissionSource {
        store: store.root().into(),
        trust_domain: "louiselm/skills".into(),
        operator_uid: uid,
        broker_uid: uid + 1,
    };
    let mut message = query(session.authorization());
    let CommandOperation::SkillRequest { request } = &mut message.operation else {
        unreachable!()
    };
    request.packages = packages.iter().map(ToString::to_string).collect();
    request.subject = subject;
    let first = accepted(exchange(&service, &mut session, &peer, &message, true));
    assert_eq!(first.subject, subject);
    assert_eq!(first.session_id, session.authorization().session_id);
    assert_eq!(first.run_id, session.authorization().run_id);
    let projection = outbox.next().unwrap().unwrap();
    let encoded_projection = projection.wire().to_string();
    for private in packages.iter().map(ToString::to_string).chain([
        store.root().display().to_string(),
        "codex".into(),
        "Private profile fixture".into(),
        "Second proposed change".into(),
    ]) {
        assert!(!encoded_projection.contains(&private));
    }
    let ProjectionChange::Upsert(condition) = projection.change else {
        panic!("pending request projection");
    };
    assert_eq!(
        condition.subject,
        if subject == SkillSubject::Supply {
            AttentionSubject::Supply(uid.to_string())
        } else {
            AttentionSubject::Session("skill-session".into())
        }
    );
    assert_eq!(condition.operation_id, first.operation_id);
    for outcome in [None, Some(SkillRequestOutcome::Rejected)] {
        assert!(matches!(
            service.skill_request_control(uid + 1, &first.operation_id, outcome),
            Err(louiselm_skills::broker::BrokerError::ControllerMismatch)
        ));
    }
    observe(&outbox);
    let launch_history = service.receipts().stored_bytes("skill-session").unwrap();
    assert!(
        service
            .skill_request_control(
                uid,
                &first.operation_id,
                Some(SkillRequestOutcome::Approved)
            )
            .is_err()
    );
    let signer = SshKeygenSigner::new(key.private_key_path());
    let ceremony = AdmissionRequest {
        members: packages
            .into_iter()
            .map(|package| AdmissionMember {
                package,
                depth: ReviewDepth::Read,
                agents: vec!["codex".into()],
            })
            .collect(),
        signer: &signer,
        admitted_at_ms: 4000,
        configured_agents: (subject == SkillSubject::Supply).then(|| vec!["codex".into()]),
    };
    fs::create_dir_all(store.root().join("generations")).unwrap();
    protect_fixture_directories(store.root());
    let record = admission::admit_linked(
        &store,
        &Policy::embedded(),
        &ceremony,
        &first.operation_id,
        reviewed_profile.as_ref(),
        &|| {
            service
                .reserve_skill_signing(uid, &first.operation_id, &[], &source)
                .map(|_| ())
                .map_err(|_| admission::AdmissionError::SigningReservation)
        },
    )
    .unwrap();
    protect_fixture_directories(store.root());
    for ancestor in store.root().ancestors() {
        use std::os::unix::fs::MetadataExt;
        let metadata = fs::symlink_metadata(ancestor).unwrap();
        assert!(
            metadata.is_dir() && metadata.mode() & 0o022 == 0,
            "{}: {metadata:?}",
            ancestor.display()
        );
    }
    service
        .reconcile_skill_admissions(None, None, verify_fixture_signature)
        .unwrap();
    observe(&outbox);
    assert_eq!(
        service
            .skill_request_control(uid, &first.operation_id, None)
            .unwrap()
            .outcome,
        SkillRequestOutcome::Pending
    );
    service
        .reconcile_skill_admissions(None, Some(&source), verify_fixture_signature)
        .unwrap();
    let approved = service
        .skill_request_control(uid, &first.operation_id, None)
        .unwrap();
    assert_eq!(approved.outcome, SkillRequestOutcome::Approved);
    observe(&outbox);
    assert_eq!(
        approved.admission.as_deref(),
        Some(record.generation.as_str())
    );
    assert_eq!(
        record.state,
        louiselm_skills::generation::GenerationState::PendingWitness
    );
    for outcome in [
        SkillRequestOutcome::Rejected,
        SkillRequestOutcome::Cancelled,
    ] {
        let CommandOperation::SkillRequest { request } = &mut message.operation else {
            unreachable!()
        };
        request.request_id = format!("request-{outcome:?}");
        let pending = accepted(exchange(&service, &mut session, &peer, &message, true));
        observe(&outbox);
        service
            .skill_request_control(uid, &pending.operation_id, Some(outcome))
            .unwrap();
        observe(&outbox);
        let attempt = admission::admit_linked(
            &store,
            &Policy::embedded(),
            &ceremony,
            &pending.operation_id,
            reviewed_profile.as_ref(),
            &|| {
                service
                    .reserve_skill_signing(uid, &pending.operation_id, &[], &source)
                    .map(|_| ())
                    .map_err(|_| admission::AdmissionError::SigningReservation)
            },
        );
        assert!(matches!(
            attempt,
            Err(admission::AdmissionError::SigningReservation)
        ));
        service
            .reconcile_skill_admissions(None, Some(&source), verify_fixture_signature)
            .unwrap();
        assert_eq!(
            service
                .skill_request_control(uid, &pending.operation_id, None)
                .unwrap()
                .outcome,
            outcome
        );
    }
    service
        .reconcile_skill_admissions(None, Some(&source), verify_fixture_signature)
        .unwrap();
    assert_eq!(
        service
            .skill_request_control(uid, &first.operation_id, None)
            .unwrap(),
        approved
    );
    let encoded = serde_json::to_string(&approved).unwrap();
    assert!(!encoded.contains(store.root().to_str().unwrap()));
    assert!(!encoded.contains("signature"));
    assert_eq!(
        service.receipts().stored_bytes("skill-session").unwrap(),
        launch_history
    );
    let CommandOperation::SkillRequest { request } = &mut message.operation else {
        unreachable!()
    };
    request.request_id = "disposed-request".into();
    let pending = accepted(exchange(&service, &mut session, &peer, &message, true));
    observe(&outbox);
    let current = lifecycle::status(session.authorization());
    let mut change = lifecycle::park(session.authorization());
    change.action = LifecycleAction::Disposal;
    thread::scope(|scope| {
        let worker = scope.spawn(|| lifecycle::drive_lifecycle_peer(&peer, &current));
        service
            .request_lifecycle(
                &mut session,
                &louiselm_skills::broker::lifecycle::LifecycleCaller::Operator { uid },
                &change,
                4000,
                verify_fixture_signature,
            )
            .unwrap();
        worker.join().unwrap();
    });
    let after_disposal = service
        .skill_request_control(uid, &pending.operation_id, None)
        .unwrap();
    assert_eq!(
        after_disposal.outcome,
        if subject == SkillSubject::Supply {
            SkillRequestOutcome::Pending
        } else {
            SkillRequestOutcome::Cancelled
        }
    );
    observe(&outbox);
    peer.close();
    drop(session);
    drop(service);
    let service = BrokerService::bind(
        &root.path().join("restarted.sock"),
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap(),
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    service
        .reconcile_skill_requests(None, verify_fixture_signature)
        .unwrap();
    assert_eq!(
        service
            .skill_request_control(uid, &first.operation_id, None)
            .unwrap(),
        approved
    );
    assert_eq!(
        service
            .skill_request_control(uid, &pending.operation_id, None)
            .unwrap(),
        after_disposal
    );
    observe(&outbox);
    let late = admission::admit_linked(
        &store,
        &Policy::embedded(),
        &ceremony,
        &pending.operation_id,
        reviewed_profile.as_ref(),
        &|| {
            service
                .reserve_skill_signing(uid, &pending.operation_id, &[], &source)
                .map(|_| ())
                .map_err(|_| admission::AdmissionError::SigningReservation)
        },
    );
    if subject == SkillSubject::Supply {
        let late = late.unwrap();
        assert_eq!(
            admission::load_verified(&store, &late.digest()).unwrap(),
            late
        );
    } else {
        assert!(matches!(
            late,
            Err(admission::AdmissionError::SigningReservation)
        ));
    }
    service
        .reconcile_skill_admissions(None, Some(&source), verify_fixture_signature)
        .unwrap();
    assert_eq!(
        service
            .skill_request_control(uid, &pending.operation_id, None)
            .unwrap()
            .outcome,
        if subject == SkillSubject::Supply {
            SkillRequestOutcome::Approved
        } else {
            SkillRequestOutcome::Cancelled
        }
    );
    assert_eq!(
        service
            .skill_request_control(uid, &first.operation_id, None)
            .unwrap(),
        approved
    );
    observe(&outbox);
}

pub(super) fn protect_fixture_directories(root: &Path) {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(root, fs::Permissions::from_mode(0o755)).unwrap();
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_dir() {
            protect_fixture_directories(&entry.path());
        } else {
            let mode = entry.metadata().unwrap().permissions().mode() & !0o022;
            fs::set_permissions(entry.path(), fs::Permissions::from_mode(mode)).unwrap();
        }
    }
}

#[test]
#[ignore = "requires scripts/test-broker-admission.py private mount namespace and distinct UIDs"]
#[expect(
    clippy::too_many_lines,
    reason = "This fixture owns one broker process from request creation through reconciliation and operator exchanges."
)]
fn installed_linked_admission_server() {
    use louiselm_skills::broker::{
        admission_source::AdmissionSource,
        operator::{InspectError, OperatorServer, SOCKET},
    };
    let root = PathBuf::from(std::env::var_os("LOUISELM_ADMISSION_STATE").unwrap());
    let uid: u32 = std::env::var("LOUISELM_ADMISSION_OPERATOR")
        .unwrap()
        .parse()
        .unwrap();
    let calls: usize = std::env::var("LOUISELM_ADMISSION_CALLS")
        .unwrap()
        .parse()
        .unwrap();
    let (service, operation) = if root.join("operation").exists() {
        let service = BrokerService::bind(
            &root.join(format!("restarted-{}.sock", std::process::id())),
            AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap(),
            ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
            AuditLog::open(&root.join("audit")).unwrap(),
            local_pin(),
        )
        .unwrap();
        (service, fs::read_to_string(root.join("operation")).unwrap())
    } else {
        let supply = std::env::var_os("LOUISELM_ADMISSION_SUPPLY").is_some();
        let (service, mut session, peer) = fixture_for(&root, true, false, supply, uid);
        let mut message = query(session.authorization());
        let source_packages: Vec<String> = std::env::var("LOUISELM_ADMISSION_SOURCES").map_or_else(
            |_| Vec::new(),
            |value| serde_json::from_str(&value).unwrap(),
        );
        let mut sources = Vec::new();
        for (index, package) in source_packages.iter().enumerate() {
            let CommandOperation::SkillRequest { request } = &mut message.operation else {
                unreachable!()
            };
            request.request_id = format!("source-{index}");
            request.subject = SkillSubject::Supply;
            request.packages = vec![package.clone()];
            sources.push(
                accepted(exchange(&service, &mut session, &peer, &message, true)).operation_id,
            );
        }
        sources.sort();
        fs::write(root.join("sources"), serde_json::to_vec(&sources).unwrap()).unwrap();
        let CommandOperation::SkillRequest { request } = &mut message.operation else {
            unreachable!()
        };
        request.request_id = "batch-target".into();
        request.packages = match std::env::var("LOUISELM_ADMISSION_PACKAGES") {
            Ok(packages) => serde_json::from_str(&packages).unwrap(),
            Err(_) => vec![std::env::var("LOUISELM_ADMISSION_PACKAGE").unwrap()],
        };
        if supply {
            request.subject = SkillSubject::Supply;
        }
        let accepted = accepted(exchange(&service, &mut session, &peer, &message, true));
        fs::write(root.join("operation"), &accepted.operation_id).unwrap();
        if std::env::var_os("LOUISELM_ADMISSION_CANCEL_SOURCE").is_some() {
            service
                .skill_request_control(uid, &sources[0], Some(SkillRequestOutcome::Cancelled))
                .unwrap();
        }
        peer.close();
        (service, accepted.operation_id)
    };
    let reconcile = || {
        let source = AdmissionSource::installed().unwrap();
        service
            .reconcile_skill_admissions(None, source.as_ref(), verify_fixture_signature)
            .unwrap();
        fs::write(
            root.join("status"),
            serde_json::to_vec(
                &service
                    .skill_request_control(uid, &operation, None)
                    .unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        let sources: Vec<String> =
            serde_json::from_slice(&fs::read(root.join("sources")).unwrap()).unwrap();
        let statuses = sources
            .iter()
            .map(|id| service.skill_request_control(uid, id, None).unwrap())
            .collect::<Vec<_>>();
        fs::write(
            root.join("source-statuses"),
            serde_json::to_vec(&statuses).unwrap(),
        )
        .unwrap();
    };
    reconcile();
    let server = OperatorServer::bind(Path::new(SOCKET), uid).unwrap();
    for _ in 0..calls {
        server
            .serve_once(
                |_, _| Err(louiselm_skills::broker::operator::InspectError::UnknownSession),
                |_| panic!("not launch-input staging"),
                |_, _| panic!("not verification control"),
                |_| panic!("authorization request not expected"),
                |_, _| Err(louiselm_skills::broker::operator::InspectError::UnknownSession),
                |_, _| Err(InspectError::UnknownSession),
                |_| Err(InspectError::UnknownSession),
                |id, outcome, sources| {
                    let source = AdmissionSource::installed().unwrap();
                    let result = match outcome {
                        Some(SkillRequestOutcome::Pending) => service.reserve_skill_signing(
                            uid,
                            id,
                            sources,
                            source.as_ref().unwrap(),
                        ),
                        Some(SkillRequestOutcome::Approved) => {
                            service.inspect_skill_signing(uid, id, sources)
                        }
                        _ => service.skill_request_control(uid, id, outcome),
                    };
                    result.map_err(|_| InspectError::StatusUnavailable)
                },
                |_, _| Err(InspectError::StatusUnavailable),
                |_, _| Err(InspectError::StatusUnavailable),
                |_, _, _| panic!("not waiver control"),
                |_, _, _| Err(louiselm_skills::broker::provider_extension::ExtensionError::Unknown),
            )
            .unwrap();
    }
}

#[test]
#[ignore = "invoked with a disposable capture-service by scripts/test-skill-requests"]
fn cross_service_requests() {
    use louiselm_skills::broker::attention::AttentionEndpoint;
    let root = PathBuf::from(std::env::var_os("LOUISELM_REQUEST_STATE").unwrap());
    let endpoint: AttentionEndpoint = serde_json::from_slice(
        &fs::read(std::env::var_os("LOUISELM_REQUEST_ENDPOINT").unwrap()).unwrap(),
    )
    .unwrap();
    let phase = std::env::var("LOUISELM_REQUEST_PHASE").unwrap();
    let service = if phase == "accept" {
        let (service, mut session, peer) = fixture_for(&root, true, true, true, CONTROLLER_UID);
        let mut message = query(session.authorization());
        let session_request = accepted(exchange(&service, &mut session, &peer, &message, true));
        let CommandOperation::SkillRequest { request } = &mut message.operation else {
            unreachable!()
        };
        request.subject = SkillSubject::Supply;
        request.request_id = "supply-batch".into();
        request
            .packages
            .push(Digest::of(b"second-package").to_string());
        request.packages.sort();
        let supply_request = accepted(exchange(&service, &mut session, &peer, &message, true));
        assert_eq!(
            accepted(exchange(&service, &mut session, &peer, &message, true)),
            supply_request
        );
        let CommandOperation::SkillRequest { request } = &mut message.operation else {
            unreachable!()
        };
        request.subject = SkillSubject::Run;
        request.request_id = "run-request".into();
        let current = lifecycle::status(session.authorization());
        thread::scope(|scope| {
            let worker = scope.spawn(|| {
                for _ in 0..2 {
                    settle(|done| peer.send(message.canonical_bytes(), done));
                    lifecycle::answer_one_status_query(&peer, &current);
                    let LauncherPacket::Request(ProtocolMessage::Command(reply)) =
                        settle(|done| peer.receive(done)).packet
                    else {
                        panic!("request result");
                    };
                    assert_eq!(
                        accepted(reply.operation).outcome,
                        SkillRequestOutcome::Pending
                    );
                }
            });
            service
                .step(
                    &mut session,
                    3000,
                    Some(&endpoint),
                    verify_fixture_signature,
                )
                .unwrap();
            // An accepted retry needs no receiver connection; no second request is created.
            service
                .step(&mut session, 3000, None, verify_fixture_signature)
                .unwrap();
            worker.join().unwrap();
        });
        assert_eq!(
            service
                .skill_request_control(CONTROLLER_UID, &session_request.operation_id, None)
                .unwrap(),
            session_request
        );
        assert_eq!(
            service
                .skill_request_control(CONTROLLER_UID, &supply_request.operation_id, None)
                .unwrap(),
            supply_request
        );
        service
    } else {
        BrokerService::bind(
            &root.join(format!("{phase}.sock")),
            AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap(),
            ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
            AuditLog::open(&root.join("audit")).unwrap(),
            local_pin(),
        )
        .unwrap()
    };
    service
        .reconcile_skill_requests(Some(&endpoint), verify_fixture_signature)
        .unwrap();
    let outbox = Outbox::open(&root.join("authorizations/attention-outbox")).unwrap();
    while outbox.deliver_next(&endpoint).unwrap() {}
}

fn exchange(
    service: &BrokerService,
    session: &mut BrokerSession,
    peer: &SeqpacketChannel,
    message: &CommandMessage,
    queries_status: bool,
) -> CommandOperation {
    let status = lifecycle::status(session.authorization());
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            settle(|done| peer.send(message.canonical_bytes(), done));
            if queries_status {
                lifecycle::answer_one_status_query(peer, &status);
            }
            let LauncherPacket::Request(ProtocolMessage::Command(reply)) =
                settle(|done| peer.receive(done)).packet
            else {
                panic!("request reply");
            };
            assert_eq!(reply.request_id, message.request_id);
            reply.operation
        });
        assert!(
            !service
                .step(session, 3000, None, verify_fixture_signature)
                .unwrap()
        );
        worker.join().unwrap()
    })
}

fn accepted(operation: CommandOperation) -> SkillRequestStatus {
    let CommandOperation::SkillRequestResult { status } = operation else {
        panic!("durable acceptance: {operation:?}");
    };
    status
}

#[test]
fn broker_permission_is_explicit_and_subject_revision_scope_are_not_client_authority() {
    for permitted in [false, true] {
        let root = TempDir::new().unwrap();
        let (service, mut session, peer) = fixture(root.path(), permitted);
        let original = query(session.authorization());
        let mut foreign = original.clone();
        foreign.session_id = "other-session".into();
        let mut stale = original.clone();
        stale.envelope_revision += 1;
        let mut scope = original.clone();
        let CommandOperation::SkillRequest { request } = &mut scope.operation else {
            unreachable!()
        };
        request.agents = vec!["other".into()];
        let mut run = original.clone();
        let CommandOperation::SkillRequest { request } = &mut run.operation else {
            unreachable!()
        };
        request.subject = SkillSubject::Run;
        let mut supply = original.clone();
        let CommandOperation::SkillRequest { request } = &mut supply.operation else {
            unreachable!()
        };
        request.subject = SkillSubject::Supply;
        for invalid in [&foreign, &stale, &scope, &run, &supply] {
            assert!(matches!(
                exchange(&service, &mut session, &peer, invalid, false),
                CommandOperation::SkillRequestRefused {
                    error: ErrorCode::InvalidRequest
                }
            ));
        }
        let result = exchange(&service, &mut session, &peer, &original, permitted);
        if permitted {
            assert_eq!(accepted(result).outcome, SkillRequestOutcome::Pending);
        } else {
            assert!(matches!(
                result,
                CommandOperation::SkillRequestRefused { .. }
            ));
        }
        let outbox = Outbox::open(&root.path().join("authorizations/attention-outbox")).unwrap();
        assert_eq!(outbox.next().unwrap().is_some(), permitted);
    }
}

#[test]
fn durable_operator_result_replays_without_admission_or_receipt_changes() {
    let root = TempDir::new().unwrap();
    let (service, mut session, peer) = fixture(root.path(), true);
    let mut message = query(session.authorization());
    let before = service.receipts().stored_bytes("skill-session").unwrap();
    let first = accepted(exchange(&service, &mut session, &peer, &message, true));
    message.request_id = "relay-retry".into();
    assert_eq!(
        accepted(exchange(&service, &mut session, &peer, &message, true)),
        first
    );
    let mut conflict = message.clone();
    let CommandOperation::SkillRequest { request } = &mut conflict.operation else {
        unreachable!()
    };
    request.packages = vec![Digest::of(b"changed").to_string()];
    assert!(matches!(
        exchange(&service, &mut session, &peer, &conflict, true),
        CommandOperation::SkillRequestRefused { .. }
    ));
    assert!(
        service
            .skill_request_control(CONTROLLER_UID + 1, &first.operation_id, None)
            .is_err()
    );
    let rejected = service
        .skill_request_control(
            CONTROLLER_UID,
            &first.operation_id,
            Some(SkillRequestOutcome::Rejected),
        )
        .unwrap();
    assert_eq!(
        accepted(exchange(&service, &mut session, &peer, &message, true)),
        rejected
    );
    assert_eq!(
        service.receipts().stored_bytes("skill-session").unwrap(),
        before
    );
    drop(session);
    drop(peer);
    drop(service);
    let socket = root.path().join("restarted.sock");
    let service = BrokerService::bind(
        &socket,
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap(),
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    assert_eq!(
        service
            .skill_request_control(CONTROLLER_UID, &first.operation_id, None)
            .unwrap(),
        rejected
    );
    let outbox = Outbox::open(&root.path().join("authorizations/attention-outbox")).unwrap();
    let pending = outbox.next().unwrap().unwrap();
    assert!(matches!(pending.change, ProjectionChange::Upsert(_)));
    let bytes = pending.wire().to_string();
    assert!(
        bytes.contains("admission_required")
            && !bytes.contains("codex")
            && !bytes.contains("packages")
    );
    outbox
        .acknowledge(pending.sequence, &pending.digest())
        .unwrap();
    assert!(matches!(
        outbox.next().unwrap().unwrap().change,
        ProjectionChange::Clear(_)
    ));
}

#[test]
fn signed_disposal_cancels_but_signed_park_retains_the_request() {
    for action in [LifecycleAction::Park, LifecycleAction::Disposal] {
        let root = TempDir::new().unwrap();
        let (service, mut session, peer) = fixture(root.path(), true);
        let message = query(session.authorization());
        let pending = accepted(exchange(&service, &mut session, &peer, &message, true));
        let current = lifecycle::status(session.authorization());
        let mut change = lifecycle::park(session.authorization());
        change.action = action;
        thread::scope(|scope| {
            let worker = scope.spawn(|| lifecycle::drive_lifecycle_peer(&peer, &current));
            service
                .request_lifecycle(
                    &mut session,
                    &louiselm_skills::broker::lifecycle::LifecycleCaller::Operator {
                        uid: CONTROLLER_UID,
                    },
                    &change,
                    4000,
                    verify_fixture_signature,
                )
                .unwrap();
            worker.join().unwrap();
        });
        let expected = if action == LifecycleAction::Disposal {
            SkillRequestOutcome::Cancelled
        } else {
            SkillRequestOutcome::Pending
        };
        assert_eq!(
            service
                .skill_request_control(CONTROLLER_UID, &pending.operation_id, None)
                .unwrap()
                .outcome,
            expected
        );
    }
}
