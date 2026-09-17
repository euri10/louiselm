//! New effects exercise the same authenticated public service as comments.

#[test]
fn required_integration_gate_escalation_names_coordinator_without_granting_it() {
    let root = tempfile::tempdir().unwrap();
    let (service, mut session, peer) = fixture(root.path(), None, Some(Path::new("/bin/true")));
    let mut message = mutation(
        session.authorization(),
        0,
        BeadsMutationKind::LabelAdd {
            issue_id: "test-1".into(),
            label: "integration_verified".into(),
        },
    );
    if let CommandOperation::BeadsMutation { request } = &mut message.operation {
        request.required = true;
    }
    let CommandOperation::BeadsMutationRefused {
        escalation: Some(escalation),
        error,
    } = exchange(&service, &mut session, &peer, &message, true)
    else {
        panic!("required gate must escalate, never execute")
    };
    assert_eq!(error, ErrorCode::CapabilityDenied);
    assert_eq!(escalation.capability.role, BeadsRole::Coordinator);
    assert_eq!(
        escalation.capability.effect,
        BeadsEffect::LabelAdd {
            label: "integration_verified".into()
        }
    );
    assert_eq!(
        fs::read_dir(root.path().join("authorizations/beads-mutations/requests"))
            .unwrap()
            .count(),
        0
    );
    peer.close();
}
use super::*;
use louiselm_skills::beads_mutation::{
    BeadsCloseVerdict, BeadsDependencyKind, BeadsEffect, BeadsRole, BeadsVerdictKind,
};
use std::path::PathBuf;

fn kinds(issue: &str, other: &str) -> Vec<BeadsMutationKind> {
    vec![
        BeadsMutationKind::Claim {
            issue_id: issue.into(),
        },
        BeadsMutationKind::StatusUpdate {
            issue_id: issue.into(),
            status: "blocked".into(),
        },
        BeadsMutationKind::LabelAdd {
            issue_id: issue.into(),
            label: "needs-design".into(),
        },
        BeadsMutationKind::LabelRemove {
            issue_id: issue.into(),
            label: "needs-design".into(),
        },
        BeadsMutationKind::DependencyAdd {
            issue_id: issue.into(),
            depends_on_id: other.into(),
            dependency_type: BeadsDependencyKind::Blocks,
        },
        BeadsMutationKind::DependencyRemove {
            issue_id: issue.into(),
            depends_on_id: other.into(),
            dependency_type: BeadsDependencyKind::Blocks,
        },
        BeadsMutationKind::Close {
            issue_id: issue.into(),
            reason: "--force\n$(literal) `literal`".into(),
            verdict: BeadsCloseVerdict {
                kind: BeadsVerdictKind::Gate,
                reference: "tests/fixture.rs:12".into(),
            },
        },
    ]
}

fn grant_effects(issue: &str, other: &str) -> ApprovedBeadsMutations {
    let mut permission = permission(issue);
    permission.role = BeadsRole::Coordinator;
    permission.issue_ids.push(other.into());
    permission.issue_ids.sort();
    permission.effects = kinds(issue, other)
        .iter()
        .map(BeadsEffect::for_mutation)
        .collect();
    permission.effects.sort();
    permission.max_mutations = 16;
    permission
}

fn mutation(auth: &LaunchAuthorization, index: usize, kind: BeadsMutationKind) -> CommandMessage {
    let mut message = query(auth, "unused");
    message.operation = CommandOperation::BeadsMutation {
        request: BeadsMutationRequest {
            request_id: format!("effect-{index}"),
            required: false,
            kind,
        },
    };
    message
}

#[test]
fn exact_effects_cannot_borrow_comment_scope_or_change_status_label_or_edge_targets() {
    let root = tempfile::tempdir().unwrap();
    let (service, mut session, peer) = fixture(
        root.path(),
        Some(permission("test-1")),
        Some(Path::new("/bin/true")),
    );
    for (index, kind) in kinds("test-1", "test-2").into_iter().enumerate() {
        let message = mutation(session.authorization(), index, kind);
        assert!(matches!(
            exchange(&service, &mut session, &peer, &message, false),
            CommandOperation::BeadsMutationRefused { .. }
        ));
    }
    peer.close();
    let root = tempfile::tempdir().unwrap();
    let approved = grant_effects("test-1", "test-2");
    let (service, mut session, peer) =
        fixture(root.path(), Some(approved), Some(Path::new("/bin/true")));
    let denied = [
        BeadsMutationKind::StatusUpdate {
            issue_id: "test-1".into(),
            status: "open".into(),
        },
        BeadsMutationKind::LabelAdd {
            issue_id: "test-1".into(),
            label: "integration_verified".into(),
        },
        BeadsMutationKind::DependencyAdd {
            issue_id: "test-1".into(),
            depends_on_id: "test-3".into(),
            dependency_type: BeadsDependencyKind::Blocks,
        },
        BeadsMutationKind::DependencyAdd {
            issue_id: "test-1".into(),
            depends_on_id: "test-2".into(),
            dependency_type: BeadsDependencyKind::Related,
        },
        BeadsMutationKind::DependencyRemove {
            issue_id: "test-1".into(),
            depends_on_id: "test-2".into(),
            dependency_type: BeadsDependencyKind::Related,
        },
    ];
    for (index, kind) in denied.into_iter().enumerate() {
        let message = mutation(session.authorization(), index, kind);
        assert!(matches!(
            exchange(&service, &mut session, &peer, &message, false),
            CommandOperation::BeadsMutationRefused { .. }
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

#[test]
fn worker_grants_cannot_authorize_closure_blockers_or_integration_gates() {
    let mut approved = grant_effects("test-1", "test-2");
    assert!(approved.valid(1000));
    approved.role = BeadsRole::Worker;
    assert!(!approved.valid(1000));
    for effect in [
        BeadsEffect::Close,
        BeadsEffect::LabelAdd {
            label: "integration_verified".into(),
        },
        BeadsEffect::LabelRemove {
            label: "integration_verified".into(),
        },
        BeadsEffect::DependencyRemove {
            dependency_type: BeadsDependencyKind::Blocks,
        },
        BeadsEffect::DependencyAdd {
            dependency_type: BeadsDependencyKind::Blocks,
        },
        BeadsEffect::DependencyAdd {
            dependency_type: BeadsDependencyKind::ParentChild,
        },
    ] {
        approved.effects = vec![effect];
        assert!(!approved.valid(1000));
    }
    approved.effects = vec![BeadsEffect::DependencyAdd {
        dependency_type: BeadsDependencyKind::Related,
    }];
    assert!(approved.valid(1000));
}

#[test]
fn required_missing_capability_is_one_durable_escalation_without_mutating() {
    let root = tempfile::tempdir().unwrap();
    let (service, mut session, peer) = fixture(root.path(), None, Some(Path::new("/bin/true")));
    let mut message = query(session.authorization(), "test-1");
    if let CommandOperation::BeadsMutation { request } = &mut message.operation {
        request.required = true;
    }
    let reply = exchange(&service, &mut session, &peer, &message, true);
    let json = serde_json::to_value(&reply).unwrap();
    assert!(
        !json["escalation"].is_null(),
        "required denial needs an inspectable escalation: {json}"
    );
    assert_eq!(
        exchange(&service, &mut session, &peer, &message, true),
        reply
    );
    assert_eq!(
        fs::read_dir(root.path().join("authorizations/beads-mutations/requests"))
            .unwrap()
            .count(),
        0
    );
    peer.close();
}

#[test]
fn every_effect_shares_one_budget_and_replays_without_spending_again() {
    let root = tempfile::tempdir().unwrap();
    let mut approved = grant_effects("test-1", "test-2");
    approved.max_mutations = 1;
    let (service, mut session, peer) =
        fixture(root.path(), Some(approved), Some(Path::new("/bin/true")));
    let mut effects = kinds("test-1", "test-2").into_iter();
    let first = mutation(session.authorization(), 0, effects.next().unwrap());
    let receipt = accepted(exchange(&service, &mut session, &peer, &first, true));
    assert_eq!(
        accepted(exchange(&service, &mut session, &peer, &first, true)),
        receipt
    );
    let second = mutation(session.authorization(), 1, effects.next().unwrap());
    assert!(matches!(
        exchange(&service, &mut session, &peer, &second, true),
        CommandOperation::BeadsMutationRefused { .. }
    ));
    peer.close();
}

#[test]
fn required_requests_cannot_escalate_foreign_stale_or_quarantined_subjects() {
    for scenario in ["session", "run", "revision", "quarantined"] {
        let root = tempfile::tempdir().unwrap();
        let (service, mut session, peer) = fixture(root.path(), None, Some(Path::new("/bin/true")));
        let mut message = query(session.authorization(), "test-1");
        if let CommandOperation::BeadsMutation { request } = &mut message.operation {
            request.required = true;
        }
        match scenario {
            "session" => message.session_id = "foreign".into(),
            "run" => message.run_id = "foreign".into(),
            "revision" => message.envelope_revision += 1,
            _ => LifecycleStore::open(&root.path().join("authorizations/lifecycle"))
                .unwrap()
                .quarantine(&message.session_id)
                .unwrap(),
        }
        assert!(matches!(
            exchange(
                &service,
                &mut session,
                &peer,
                &message,
                scenario == "quarantined"
            ),
            CommandOperation::BeadsMutationRefused {
                escalation: None,
                ..
            }
        ));
        for directory in ["requests", "escalations"] {
            assert_eq!(
                fs::read_dir(
                    root.path()
                        .join("authorizations/beads-mutations")
                        .join(directory)
                )
                .unwrap()
                .count(),
                0
            );
        }
        peer.close();
    }
}

#[test]
#[ignore = "requires existing upstream br; set LOUISELM_TEST_BR to its absolute path"]
#[expect(
    clippy::too_many_lines,
    reason = "One ordered upstream workflow proves every effect, blocked closure and replay against the same canonical issue."
)]
fn real_br_effects_preserve_actor_graph_policy_and_exact_close_verdict() {
    let program = PathBuf::from(std::env::var_os("LOUISELM_TEST_BR").expect("explicit br path"));
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
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap()
    };
    run(&[
        "init",
        "--prefix",
        "fixture",
        "--actor",
        "fixture/setup",
        "--json",
    ]);
    let issue = run(&["create", "target", "--actor", "fixture/setup", "--json"])["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let other = run(&[
        "create",
        "prerequisite",
        "--actor",
        "fixture/setup",
        "--json",
    ])["id"]
        .as_str()
        .unwrap()
        .to_owned();
    fs::write(workspace.join(".beads/policy.yaml"), "close_policy:\n  require_typed_references:\n    enabled: true\n    required_kinds: [consumer, gate, live, inert, none]\n").unwrap();
    let (service, mut session, peer) = fixture(
        root.path(),
        Some(grant_effects(&issue, &other)),
        Some(&program),
    );
    let all = kinds(&issue, &other);
    run(&[
        "dep",
        "add",
        &issue,
        &other,
        "--type=related",
        "--actor",
        "fixture/setup",
        "--json",
    ]);
    for (index, kind) in all.iter().cloned().enumerate() {
        let message = mutation(session.authorization(), index, kind);
        let result = accepted(exchange(&service, &mut session, &peer, &message, true));
        assert_eq!(
            result.outcome,
            BeadsMutationOutcome::Completed,
            "effect {index}"
        );
        assert_eq!(
            accepted(exchange(&service, &mut session, &peer, &message, true)),
            result
        );
        let record = run(&["show", &issue, "--json"]);
        match index {
            0 => {
                assert_eq!(record[0]["assignee"], "demo/comment-session");
                assert_eq!(record[0]["status"], "in_progress");
            }
            1 => assert_eq!(record[0]["status"], "blocked"),
            2 => assert!(
                record[0]["labels"]
                    .as_array()
                    .unwrap()
                    .contains(&"needs-design".into())
            ),
            3 => assert!(
                !record[0]["labels"]
                    .as_array()
                    .is_some_and(|labels| labels.contains(&"needs-design".into()))
            ),
            4 => {
                assert_eq!(record[0]["dependencies"][0]["id"], other);
                let close = mutation(session.authorization(), 10, all[6].clone());
                assert!(
                    matches!(
                        accepted(exchange(&service, &mut session, &peer, &close, true)).outcome,
                        BeadsMutationOutcome::Failed { .. }
                    ),
                    "upstream must refuse closure while blocked"
                );
            }
            5 => {
                assert_eq!(record[0]["dependencies"].as_array().unwrap().len(), 1);
                assert_eq!(record[0]["dependencies"][0]["dependency_type"], "related");
            }
            6 => {
                assert_eq!(record[0]["status"], "closed");
                assert_eq!(
                    record[0]["close_reason"],
                    "gate:tests/fixture.rs:12 --force\n$(literal) `literal`"
                );
            }
            _ => unreachable!(),
        }
    }
    peer.close();
}
