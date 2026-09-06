//! Behavioral coverage for generation.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

use std::{
    cell::RefCell,
    io,
    sync::{Arc, Barrier},
    thread,
};

use louiselm_capture::{
    BeadsGenerator, CommandOutput, GenerateRequest, GenerationError, RunAdmission, RunSession,
    RunStore, mutation_external_ref,
};

const TOKEN: &str = "generate-token-1234";

fn admitted_store(root: &std::path::Path, run_id: &str, ceiling: u64) -> RunStore {
    let store = RunStore::new(root).expect("store");
    store
        .admit(
            RunAdmission {
                id: run_id.to_owned(),
                generated_work_ceiling: ceiling,
                park_ttl_ms: 3_600_000,
            },
            TOKEN,
        )
        .expect("admit");
    store
        .attach(RunSession {
            id: run_id.to_owned(),
            session_id: "codex/session-123".to_owned(),
            agent: "codex".to_owned(),
            acp_session_id: "session-123".to_owned(),
            working_dir: "/tmp/project".to_owned(),
            load_session: true,
        })
        .expect("attach");
    store
}

fn request(run_id: &str, mutation_id: &str, command: &str, arguments: &[&str]) -> GenerateRequest {
    GenerateRequest {
        run_id: run_id.to_owned(),
        token: TOKEN.to_owned(),
        mutation_id: mutation_id.to_owned(),
        command: command.to_owned(),
        arguments: arguments.iter().map(|value| (*value).to_owned()).collect(),
    }
}

#[test]
fn broker_reserves_before_create_and_enforces_actor_database_and_external_ref() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let run_id = "11111111-2222-4333-8444-555555555555";
    let mutation_id = "aaaaaaaa-bbbb-4ccc-8ddd-eeeeeeeeeeee";
    let store = admitted_store(temporary.path(), run_id, 1);
    let database = temporary.path().join("beads.db");
    let generator = BeadsGenerator::new("br", &database);
    let observed = RefCell::new(Vec::new());

    let issue = generator
        .generate_with(
            &store,
            &request(
                run_id,
                mutation_id,
                "create",
                &["--title", "Generated", "--json"],
            ),
            1_000,
            |arguments| {
                observed.replace(arguments.to_vec());
                assert_eq!(
                    store.run(run_id).expect("reserved").generated_work.reserved,
                    1
                );
                Ok(CommandOutput {
                    success: true,
                    stdout: "{\"id\":\"louiselm-created\"}\n".to_owned(),
                    stderr: String::new(),
                })
            },
        )
        .expect("generate");

    assert_eq!(issue.id, "louiselm-created");
    assert_eq!(
        observed.into_inner(),
        vec![
            "create",
            "--title",
            "Generated",
            "--actor",
            "codex/session-123",
            "--external-ref",
            &mutation_external_ref(run_id, mutation_id),
            "--db",
            database.to_str().expect("database"),
            "--json",
        ]
    );
    let run = store.run(run_id).expect("confirmed");
    assert_eq!(run.generated_work.consumed, 1);
    assert_eq!(run.generated_work.reserved, 0);
}

#[test]
fn exhaustion_parks_before_the_second_process_can_start() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let run_id = "22222222-3333-4444-8555-666666666666";
    let store = admitted_store(temporary.path(), run_id, 1);
    let generator = BeadsGenerator::new("br", temporary.path().join("beads.db"));
    generator
        .generate_with(
            &store,
            &request(
                run_id,
                "bbbbbbbb-cccc-4ddd-8eee-ffffffffffff",
                "create",
                &["--title", "First"],
            ),
            1_000,
            |_| {
                Ok(CommandOutput {
                    success: true,
                    stdout: "{\"id\":\"first\"}".to_owned(),
                    stderr: String::new(),
                })
            },
        )
        .expect("first");
    let mut called = false;
    let error = generator
        .generate_with(
            &store,
            &request(
                run_id,
                "cccccccc-dddd-4eee-8fff-aaaaaaaaaaaa",
                "create",
                &["--title", "Second"],
            ),
            2_000,
            |_| {
                called = true;
                unreachable!("over-budget br process started")
            },
        )
        .expect_err("exhausted");
    assert!(matches!(error, GenerationError::Exhausted));
    assert!(!called);
    assert_eq!(store.run(run_id).expect("Parked").state, "parked");
}

#[test]
fn ambiguous_success_reconciles_by_external_ref_without_repeating_create() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let run_id = "33333333-4444-4555-8666-777777777777";
    let mutation_id = "dddddddd-eeee-4fff-8aaa-bbbbbbbbbbbb";
    let store = admitted_store(temporary.path(), run_id, 2);
    let generator = BeadsGenerator::new("br", temporary.path().join("beads.db"));
    let first = generator.generate_with(
        &store,
        &request(run_id, mutation_id, "create", &["--title", "Maybe"]),
        1_000,
        |_| {
            Ok(CommandOutput {
                success: false,
                stdout: String::new(),
                stderr: "process interrupted".to_owned(),
            })
        },
    );
    assert!(matches!(first, Err(GenerationError::Ambiguous(_))));
    assert_eq!(
        store.run(run_id).expect("pending").generated_work.reserved,
        1
    );

    let mut calls = 0;
    let reconciled = generator
        .generate_with(
            &store,
            &request(run_id, mutation_id, "create", &["--title", "Maybe"]),
            2_000,
            |_| {
                calls += 1;
                Ok(CommandOutput {
                    success: true,
                    stdout: format!(
                        "[{{\"id\":\"found\",\"external_ref\":\"{}\"}}]",
                        mutation_external_ref(run_id, mutation_id)
                    ),
                    stderr: String::new(),
                })
            },
        )
        .expect("reconciled");
    assert_eq!(calls, 1);
    assert_eq!(reconciled.id, "found");
    assert_eq!(
        store
            .run(run_id)
            .expect("confirmed")
            .generated_work
            .consumed,
        1
    );
}

#[test]
fn process_start_failure_releases_capacity_and_q_maps_to_create() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let run_id = "44444444-5555-4666-8777-888888888888";
    let store = admitted_store(temporary.path(), run_id, 2);
    let generator = BeadsGenerator::new("br", temporary.path().join("beads.db"));
    let failure = generator.generate_with(
        &store,
        &request(
            run_id,
            "eeeeeeee-ffff-4aaa-8bbb-cccccccccccc",
            "create",
            &["--title", "Failure"],
        ),
        1_000,
        |_| Err(io::Error::new(io::ErrorKind::NotFound, "missing br")),
    );
    assert!(matches!(failure, Err(GenerationError::Start(_))));
    assert_eq!(
        store.run(run_id).expect("released").generated_work.reserved,
        0
    );

    let quick = generator
        .generate_with(
            &store,
            &request(
                run_id,
                "ffffffff-aaaa-4bbb-8ccc-dddddddddddd",
                "q",
                &["captured", "idea", "--priority", "1"],
            ),
            2_000,
            |arguments| {
                assert!(
                    arguments
                        .windows(2)
                        .any(|pair| pair == ["--title", "captured idea"])
                );
                Ok(CommandOutput {
                    success: true,
                    stdout: "{\"id\":\"quick\"}".to_owned(),
                    stderr: String::new(),
                })
            },
        )
        .expect("quick capture");
    assert_eq!(quick.id, "quick");
}

#[test]
fn concurrent_generators_cannot_overspend_one_run() {
    let temporary = tempfile::tempdir().expect("temporary directory");
    let run_id = "55555555-6666-4777-8888-999999999999";
    let store = admitted_store(temporary.path(), run_id, 1);
    let barrier = Arc::new(Barrier::new(2));
    let handles = [
        "11111111-aaaa-4bbb-8ccc-111111111111",
        "22222222-bbbb-4ccc-8ddd-222222222222",
    ]
    .into_iter()
    .map(|mutation_id| {
        let store = store.clone();
        let barrier = Arc::clone(&barrier);
        thread::spawn(move || {
            barrier.wait();
            store.reserve_generated_work(
                louiselm_capture::GeneratedWorkReservation {
                    run_id: run_id.to_owned(),
                    token: TOKEN.to_owned(),
                    mutation_id: mutation_id.to_owned(),
                    kind: "beads_issue".to_owned(),
                    units: 1,
                },
                1_000,
            )
        })
    })
    .collect::<Vec<_>>();
    let results = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .expect("generator thread")
                .expect("reservation")
        })
        .collect::<Vec<_>>();
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == louiselm_capture::ReserveResult::Reserved)
            .count(),
        1
    );
    assert_eq!(
        results
            .iter()
            .filter(|result| **result == louiselm_capture::ReserveResult::Exhausted)
            .count(),
        1
    );
    let run = store.run(run_id).expect("Run");
    assert_eq!(run.generated_work.reserved, 1);
    assert_eq!(run.state, "parked");
}
