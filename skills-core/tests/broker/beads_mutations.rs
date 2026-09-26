//! Authenticated comment requests, with opt-in real upstream br acceptance.
use super::*;
#[path = "beads_effects.rs"]
mod effects;
use louiselm_skills::{
    beads_mutation::{
        ApprovedBeadsMutations, BeadsInspectionDetail, BeadsMutationKind, BeadsMutationOutcome,
        BeadsMutationRequest, BeadsMutationStatus,
    },
    broker::{BrokerSession, lifecycle::LifecycleStore},
    launch_protocol::{COMMAND_SCHEMA, CommandMessage, CommandOperation},
    workspace::provenance::OutputProvenanceCode,
};

fn permission(issue: &str) -> ApprovedBeadsMutations {
    ApprovedBeadsMutations {
        role: louiselm_skills::beads_mutation::BeadsRole::Worker,
        effects: vec![louiselm_skills::beads_mutation::BeadsEffect::CommentAdd],
        project_digest: Digest::of(b"fixture-project").to_string(),
        issue_ids: vec![issue.into()],
        max_mutations: 2,
        expires_at_ms: 60_000,
    }
}

fn fixture(
    root: &Path,
    permission: Option<ApprovedBeadsMutations>,
    program: Option<&Path>,
) -> (BrokerService, BrokerSession, SeqpacketChannel) {
    let socket = root.join("broker.sock");
    let request = request("comment-session");
    let mut approval = grant(&request);
    approval.beads_mutations = permission.map(|mut permission| {
        permission.project_digest =
            Digest::of(root.join("workspace").as_os_str().as_encoded_bytes()).to_string();
        permission
    });
    let authorizations = AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap();
    authorizations.authorize(&approval, 1000).unwrap();
    let mut service = BrokerService::bind(
        &socket,
        authorizations,
        ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    if let Some(program) = program {
        let workspace = root.join("workspace");
        fs::create_dir_all(workspace.join(".beads")).unwrap();
        if !workspace.join(".beads/beads.db").exists() {
            fs::write(workspace.join(".beads/beads.db"), []).unwrap();
        }
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
    (service, session, peer.join().unwrap().1)
}

fn query(auth: &LaunchAuthorization, issue: &str) -> CommandMessage {
    CommandMessage {
        schema: COMMAND_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "relay-1".into(),
        session_id: auth.session_id.clone(),
        run_id: auth.run_id.clone(),
        envelope_revision: auth.envelope_revision,
        operation: CommandOperation::BeadsMutation {
            request: BeadsMutationRequest {
                request_id: "stable-comment".into(),
                required: false,
                kind: BeadsMutationKind::CommentAdd {
                    issue_id: issue.into(),
                    text: "--actor=forged\n$(no-shell) `literal`".into(),
                },
            },
        },
    }
}

fn exchange(
    service: &BrokerService,
    session: &mut BrokerSession,
    peer: &SeqpacketChannel,
    message: &CommandMessage,
    observe: bool,
) -> CommandOperation {
    let status = lifecycle::status(session.authorization());
    thread::scope(|scope| {
        let worker = scope.spawn(|| {
            settle(|done| peer.send(message.canonical_bytes(), done));
            if observe {
                lifecycle::answer_one_status_query(peer, &status);
            }
            let LauncherPacket::Request(ProtocolMessage::Command(reply)) =
                settle(|done| peer.receive(done)).packet
            else {
                panic!("comment reply");
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

fn accepted(operation: CommandOperation) -> BeadsMutationStatus {
    let CommandOperation::BeadsMutationResult { status } = operation else {
        panic!("comment outcome: {operation:?}")
    };
    status
}

#[test]
fn comment_requires_config_permission_exact_subject_revision_and_live_state() {
    for scenario in [
        "unset",
        "unapproved",
        "foreign-session",
        "foreign-run",
        "stale-revision",
        "out-of-scope",
        "expired",
        "quarantined",
    ] {
        let root = tempfile::tempdir().unwrap();
        let mut approved = permission("test-1");
        if scenario == "expired" {
            approved.expires_at_ms = 2500;
        }
        let program = (scenario != "unset").then_some(Path::new("/bin/true"));
        let (service, mut session, peer) = fixture(
            root.path(),
            (scenario != "unapproved").then_some(approved),
            program,
        );
        let mut query = query(
            session.authorization(),
            if scenario == "out-of-scope" {
                "test-2"
            } else {
                "test-1"
            },
        );
        match scenario {
            "foreign-session" => query.session_id = "foreign".into(),
            "foreign-run" => query.run_id = "foreign".into(),
            "stale-revision" => query.envelope_revision += 1,
            "quarantined" => LifecycleStore::open(&root.path().join("authorizations/lifecycle"))
                .unwrap()
                .quarantine(&query.session_id)
                .unwrap(),
            _ => {}
        }
        let result = exchange(
            &service,
            &mut session,
            &peer,
            &query,
            scenario == "quarantined",
        );
        assert!(
            matches!(result, CommandOperation::BeadsMutationRefused { .. }),
            "{scenario}: {result:?}"
        );
        if scenario == "stale-revision" {
            assert!(matches!(
                result,
                CommandOperation::BeadsMutationRefused {
                    error: ErrorCode::EnvelopeRevisionMismatch,
                    ..
                }
            ));
        }
        assert_eq!(
            fs::read_dir(root.path().join("authorizations/beads-mutations/requests"))
                .unwrap()
                .count(),
            0
        );
        peer.close();
    }
}

#[test]
fn completed_comment_replays_and_changed_content_is_refused() {
    let root = tempfile::tempdir().unwrap();
    let (service, mut session, peer) = fixture(
        root.path(),
        Some(permission("test-1")),
        Some(Path::new("/bin/true")),
    );
    let mut query = query(session.authorization(), "test-1");
    let first = accepted(exchange(&service, &mut session, &peer, &query, true));
    assert_eq!(first.outcome, BeadsMutationOutcome::Completed);
    let before = service
        .beads_mutation_control(CONTROLLER_UID, &first.operation_id, None)
        .unwrap();
    let BeadsInspectionDetail::Mutation {
        output_provenance, ..
    } = before.detail
    else {
        panic!("expected mutation inspection")
    };
    assert_eq!(output_provenance.code, OutputProvenanceCode::Untainted);
    assert_eq!(
        accepted(exchange(&service, &mut session, &peer, &query, true)),
        first
    );
    if let CommandOperation::BeadsMutation { request } = &mut query.operation
        && let BeadsMutationKind::CommentAdd { text, .. } = &mut request.kind
    {
        *text = "different".into();
    }
    assert!(matches!(
        exchange(&service, &mut session, &peer, &query, true),
        CommandOperation::BeadsMutationRefused { .. }
    ));
    LifecycleStore::open(&root.path().join("authorizations/lifecycle"))
        .unwrap()
        .quarantine(&query.session_id)
        .unwrap();
    let uncertain = service
        .beads_mutation_control(CONTROLLER_UID, &first.operation_id, None)
        .unwrap();
    let BeadsInspectionDetail::Mutation {
        output_provenance,
        status,
        ..
    } = uncertain.detail
    else {
        panic!("expected mutation inspection")
    };
    assert_eq!(status.outcome, BeadsMutationOutcome::Completed);
    assert_eq!(output_provenance.code, OutputProvenanceCode::Unknown);
    query.request_id = "relay-late".into();
    if let CommandOperation::BeadsMutation { request } = &mut query.operation {
        request.request_id = "late-comment".into();
    }
    assert!(matches!(
        exchange(&service, &mut session, &peer, &query, true),
        CommandOperation::BeadsMutationRefused { .. }
    ));
    peer.close();
}

#[test]
fn tracker_configuration_rejects_wrong_digest_and_missing_database() {
    let root = tempfile::tempdir().unwrap();
    let (mut service, _session, peer) = fixture(root.path(), None, None);
    assert!(
        service
            .configure_beads_tracker(root.path(), Path::new("/bin/true"), &Digest::of(b"wrong"))
            .is_err()
    );
    fs::create_dir(root.path().join(".beads")).unwrap();
    fs::write(root.path().join(".beads/beads.db"), []).unwrap();
    assert!(
        service
            .configure_beads_tracker(root.path(), Path::new("/bin/true"), &Digest::of(b"wrong"))
            .is_err()
    );
    peer.close();
}

#[test]
fn comment_grant_cannot_follow_a_change_of_canonical_project() {
    let root = tempfile::tempdir().unwrap();
    let (mut service, mut session, peer) = fixture(
        root.path(),
        Some(permission("test-1")),
        Some(Path::new("/bin/true")),
    );
    let other = root.path().join("other");
    fs::create_dir_all(other.join(".beads")).unwrap();
    fs::write(other.join(".beads/beads.db"), []).unwrap();
    service
        .configure_beads_tracker(
            &other,
            Path::new("/bin/true"),
            &Digest::of(&fs::read("/bin/true").unwrap()),
        )
        .unwrap();
    let query = query(session.authorization(), "test-1");
    assert!(matches!(
        exchange(&service, &mut session, &peer, &query, false),
        CommandOperation::BeadsMutationRefused { .. }
    ));
    peer.close();
}

#[test]
#[ignore = "requires existing upstream br; set LOUISELM_TEST_BR to its absolute path"]
fn real_br_comment_is_attributed_once_without_interpreting_text() {
    let program =
        std::path::PathBuf::from(std::env::var_os("LOUISELM_TEST_BR").expect("explicit br path"));
    assert!(program.is_absolute());
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path().join("workspace");
    fs::create_dir(&workspace).unwrap();
    let run = |args: &[&str]| {
        let output = std::process::Command::new(&program)
            .args(args)
            .env_clear()
            .current_dir(&workspace)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "upstream br: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    };
    run(&[
        "init",
        "--prefix",
        "fixture",
        "--actor",
        "fixture/setup",
        "--json",
    ]);
    let issue: serde_json::Value = serde_json::from_slice(&run(&[
        "create",
        "Disposable comment target",
        "--actor",
        "fixture/setup",
        "--json",
    ]))
    .unwrap();
    let issue = issue["id"].as_str().unwrap();
    let (service, mut session, peer) =
        fixture(root.path(), Some(permission(issue)), Some(&program));
    let query = query(session.authorization(), issue);
    let first = accepted(exchange(&service, &mut session, &peer, &query, true));
    assert_eq!(first.outcome, BeadsMutationOutcome::Completed);
    assert_eq!(
        accepted(exchange(&service, &mut session, &peer, &query, true)),
        first
    );
    let comments: serde_json::Value =
        serde_json::from_slice(&run(&["comments", "list", issue, "--json"])).unwrap();
    let comments = comments.as_array().unwrap();
    assert_eq!(comments.len(), 1);
    assert_eq!(comments[0]["author"], "demo/comment-session");
    assert_eq!(comments[0]["text"], "--actor=forged\n$(no-shell) `literal`");
    peer.close();
}
