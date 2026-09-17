use super::*;
use louiselm_skills::dependency_fetch::{
    ApprovedDependencies, Attendance, Candidate, DependencyRequest, Source, StartingLockfile,
};

fn permission(request: &LaunchRequest) -> ApprovedDependencies {
    ApprovedDependencies {
        input_manifest_digest: request.session_input_manifest_id.clone(),
        lockfile_path: "Cargo.lock".into(),
        lockfile_digest: StartingLockfile::cargo(b"version = 4\n")
            .unwrap()
            .digest()
            .into(),
        registries: vec![],
        preapproved: vec![],
        max_fetches: 2,
        max_bytes: 1024,
        expires_at_ms: 60_000,
    }
}

fn staged_inputs(
    service: &BrokerService,
    root: &Path,
) -> (
    louiselm_skills::session_manifest::SessionInputManifest,
    Vec<u8>,
) {
    use louiselm_skills::{
        cache::CacheBase,
        registry::{AgentRegistration, Provider, RuntimeMeasurement},
        session_manifest::{SessionInputManifest, SessionInputs},
        workspace,
    };
    let repo = root.join("source");
    let cache = root.join("cache-base");
    let snapshot = root.join("snapshot");
    fs::create_dir(&repo).unwrap();
    fs::create_dir(&cache).unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--allow-empty",
            "--quiet",
            "-m",
            "fixture",
        ],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap())
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .unwrap();
        assert!(output.status.success(), "{output:?}");
    }
    let bytes = b"version = 4\n".to_vec();
    fs::write(repo.join("Cargo.lock"), &bytes).unwrap();
    let preview = workspace::prepare(&repo, &["Cargo.lock".into()], &snapshot).unwrap();
    let digest = |bytes: &[u8]| Some(Digest::of(bytes).to_string());
    let manifest = SessionInputManifest::build(SessionInputs {
        agent: Some(AgentRegistration {
            id: "demo".into(),
            provider: Provider::Fixed("fixture".into()),
            runtime_id: "runtime".into(),
            arguments: vec![],
            environment: std::collections::BTreeMap::new(),
            tool_integration: None,
        }),
        runtime: Some(RuntimeMeasurement {
            runtime_id: "runtime".into(),
            executable_sha256: Digest::of(b"runtime").hex().into(),
            adapters: vec![],
            version: "1".into(),
            origin: "fixture".into(),
        }),
        skill_generation_id: digest(b"generation"),
        view_digest: digest(b"view"),
        project_instructions: Some(vec![]),
        tool_schemas: Some(vec![]),
        plugin_schemas: Some(vec![]),
        source_snapshot_digest: Some(preview.snapshot_digest),
        source_base_digest: Some(preview.base_digest),
        cache_base_digest: Some(CacheBase::capture(&cache).unwrap().digest().to_string()),
        policy_digest: digest(b"policy"),
        isolation_receipt: Some("isolation".into()),
        envelope_id: Some("envelope-1".into()),
        envelope_revision: Some(7),
        acp_mcp_servers: Some(vec![]),
    })
    .unwrap();
    service
        .stage_launch_inputs(&manifest, &snapshot, &cache)
        .unwrap();
    (manifest, bytes)
}

#[test]
fn dependency_real_broker_uses_immutable_inputs_and_local_interactive_decisions() {
    use louiselm_skills::launch_protocol::{COMMAND_SCHEMA, CommandMessage, CommandOperation};
    for attendance in [Attendance::Interactive, Attendance::Unattended] {
        let root = TempDir::new().unwrap();
        let socket = root.path().join("broker.sock");
        let store = AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
        let service = BrokerService::bind(
            &socket,
            store,
            ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
            AuditLog::open(&root.path().join("audit")).unwrap(),
            local_pin(),
        )
        .unwrap();
        let (manifest, _) = staged_inputs(&service, root.path());
        let mut launch = request("dependencies");
        launch.session_input_manifest_id = manifest.digest().to_string();
        let mut approved = grant(&launch);
        approved.dependencies = Some(permission(&launch));
        approved.conformance.attendance = attendance;
        service.authorizations().authorize(&approved, 1).unwrap();
        let peer = thread::spawn(move || fake_supervisor(&socket, &launch, 2));
        let mut session = service.serve_launch(2, verify_fixture_signature).unwrap();
        let (_, peer) = peer.join().unwrap();
        // A live workspace edit cannot become this Session's starting lockfile.
        fs::write(
            root.path().join("source/Cargo.lock"),
            b"version = 4\n[[package]]\nname = 'secret'\nversion = '1.0.0'\n",
        )
        .unwrap();
        let auth = session.authorization();
        let query = CommandMessage {
            schema: COMMAND_SCHEMA.into(),
            protocol_version: PROTOCOL_VERSION,
            request_id: "outer".into(),
            session_id: auth.session_id.clone(),
            run_id: auth.run_id.clone(),
            envelope_revision: auth.envelope_revision,
            operation: CommandOperation::DependencyFetch {
                request: DependencyRequest {
                    request_id: "dependency".into(),
                    candidate: Candidate {
                        name: "secret".into(),
                        version: "1.0.0".into(),
                        source: Source::Registry {
                            registry: "attacker".into(),
                        },
                        integrity: None,
                    },
                    max_bytes: 128,
                },
            },
        };
        let status = lifecycle::status(session.authorization());
        let reply = thread::scope(|scope| {
            let worker = scope.spawn(|| {
                settle(|done| peer.send(query.canonical_bytes(), done));
                lifecycle::answer_one_status_query(&peer, &status);
                settle(|done| peer.receive(done)).packet
            });
            assert!(
                !service
                    .step(&mut session, 3, None, verify_fixture_signature)
                    .unwrap()
            );
            worker.join().unwrap()
        });
        let LauncherPacket::Request(ProtocolMessage::Command(reply)) = reply else {
            panic!("typed result")
        };
        let view = service
            .dependency_control(CONTROLLER_UID, "dependencies", None, 4)
            .unwrap();
        if attendance == Attendance::Interactive {
            assert!(matches!(
                reply.operation,
                CommandOperation::DependencyResult {
                    status: louiselm_skills::dependency_fetch::DependencyStatus::Pending { .. }
                }
            ));
            assert_eq!(view.pending.len(), 1);
            let ids = vec![view.pending[0].candidate_id.clone()];
            assert_eq!(
                service
                    .dependency_control(CONTROLLER_UID, "dependencies", Some(&ids), 5)
                    .unwrap()
                    .approved,
                ids
            );
        } else {
            assert!(matches!(
                reply.operation,
                CommandOperation::DependencyResult {
                    status: louiselm_skills::dependency_fetch::DependencyStatus::Denied
                }
            ));
            assert!(view.pending.is_empty());
        }
    }
}

#[test]
fn dependency_unattended_scope_must_be_authorized_before_any_session_in_the_run_starts() {
    let root = TempDir::new().unwrap();
    let store = AuthorizationStore::open(root.path(), pool(4)).unwrap();
    let first = request("first");
    let mut initial = grant(&first);
    initial.conformance.attendance = Attendance::Unattended;
    initial.dependencies = Some(permission(&first));
    store.authorize(&initial, 1).unwrap();
    let second = request("second");
    let mut planned = grant(&second);
    planned.conformance.attendance = Attendance::Unattended;
    planned.dependencies = Some(permission(&second));
    store.authorize(&planned, 2).unwrap();
    store.consume(&first, CONTROLLER_UID, 3).unwrap();
    // Already authorized Session scope remains usable after Run start.
    store.consume(&second, CONTROLLER_UID, 4).unwrap();
    let third = request("late");
    let mut late = grant(&third);
    late.conformance.attendance = Attendance::Unattended;
    late.dependencies = Some(permission(&third));
    assert!(
        store.authorize(&late, 5).is_err(),
        "mid-Run authorization must not create unattended dependency authority"
    );
    // An unset optional capability never blocks a Session that needs no fetches.
    late.dependencies = None;
    store.authorize(&late, 6).unwrap();
}

#[test]
fn dependency_unset_permission_is_refused_on_the_real_broker_step_without_network() {
    use louiselm_skills::launch_protocol::{COMMAND_SCHEMA, CommandMessage, CommandOperation};
    let root = TempDir::new().unwrap();
    let socket = root.path().join("broker.sock");
    let launch = request("dependencies-unset");
    let store = AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
    store.authorize(&grant(&launch), 1).unwrap();
    let service = BrokerService::bind(
        &socket,
        store,
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    let peer = thread::spawn(move || fake_supervisor(&socket, &launch, 2));
    let mut session = service.serve_launch(2, verify_fixture_signature).unwrap();
    let (_, peer) = peer.join().unwrap();
    let auth = session.authorization();
    let query = CommandMessage {
        schema: COMMAND_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "outer".into(),
        session_id: auth.session_id.clone(),
        run_id: auth.run_id.clone(),
        envelope_revision: auth.envelope_revision,
        operation: CommandOperation::DependencyFetch {
            request: DependencyRequest {
                request_id: "dependency".into(),
                candidate: Candidate {
                    name: "secret".into(),
                    version: "1.0.0".into(),
                    source: Source::Registry {
                        registry: "attacker".into(),
                    },
                    integrity: None,
                },
                max_bytes: 128,
            },
        },
    };
    settle(|done| peer.send(query.canonical_bytes(), done));
    assert!(
        !service
            .step(&mut session, 3, None, verify_fixture_signature)
            .unwrap()
    );
    let LauncherPacket::Request(ProtocolMessage::Command(reply)) =
        settle(|done| peer.receive(done)).packet
    else {
        panic!("typed refusal")
    };
    assert!(matches!(
        reply.operation,
        CommandOperation::DependencyRefused { .. }
    ));
}
