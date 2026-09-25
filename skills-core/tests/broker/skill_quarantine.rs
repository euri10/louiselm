//! A skill quarantine reaches running Sessions pinned to an affected Generation
//! (`louiselm-d6fv.6.5.1`): real store, real signed Admission, fake supervisor.

use super::*;
use louiselm_skills::{
    Policy, Store,
    admission::{self, AdmissionMember, AdmissionRequest},
    broker::{BrokerSession, admission_source::AdmissionSource, lifecycle::LifecycleStore},
    dossier::ReviewDepth,
    launch_protocol::{ChannelState, CommandOperation, SupervisorStatus},
    launch_receipt::SignedReceipt,
    quarantine,
    signer::SshKeygenSigner,
    sshsig::SkPolicy,
    trust::TrustStore,
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
    // Test-only trusted provenance, with real software-key Admission.
    fs::write(store.root().join("provenance.json"), br#"{"schema":"louiselm.skills.store-provenance/1","trusted":true,"created_by_release":"fixture"}"#).unwrap();
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
    let socket = root.join("broker.sock");
    let mut request = request("quarantined");
    request.skill_generation_id = generation.into();
    let authorizations = AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap();
    authorizations.authorize(&grant(&request), 1000).unwrap();
    let service = BrokerService::bind(
        &socket,
        authorizations,
        ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
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
            let supervisor = scope.spawn(move || {
                let packet = settle(|complete| peer.receive(complete));
                let LauncherPacket::Request(ProtocolMessage::Command(mut message)) = packet.packet
                else {
                    panic!("command revocation before Park")
                };
                assert!(matches!(message.operation, CommandOperation::Revoke));
                message.operation = CommandOperation::Revoked { enforced: true };
                settle(|complete| peer.send(message.canonical_bytes(), complete));
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
    let receipt = live.settle_and_park(&supply.source);
    assert!(matches!(
        receipt.payload.outcome,
        louiselm_skills::launch_receipt::ReceiptOutcome::Park { .. }
    ));
    assert!(live.quarantined());
    assert!(live.session.command_revocation_complete());
    // Settled: later ticks do nothing, with no supervisor exchange.
    assert_eq!(live.settle_quietly(Some(&supply.source)).unwrap(), None);
}

#[test]
fn excluding_everything_reaches_every_session() {
    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    quarantine::exclude_everything(&supply.store, "incident", 5000).unwrap();
    live.settle_and_park(&supply.source);
    assert!(live.quarantined());
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
}

#[test]
fn without_an_installed_admission_source_nothing_is_observable() {
    let supply = supply();
    let mut live = launched(supply.root.path(), &supply.generation);
    quarantine::exclude_everything(&supply.store, "incident", 5000).unwrap();
    assert_eq!(live.settle_quietly(None).unwrap(), None);
    assert!(!live.quarantined());
}
