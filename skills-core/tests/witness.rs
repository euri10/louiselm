//! Git witness absence is distinct from a failed remote lookup.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

mod support;

use louiselm_skills::{
    Digest,
    witness::{GitWitness, Witness, WitnessError},
};
use support::Fixture;

#[test]
fn unavailable_remote_is_a_lookup_error_not_an_absent_record() {
    let fixture = Fixture::new();
    let witness = GitWitness::new(
        &fixture.path("unavailable.git"),
        "skill-generations",
        &fixture.path("lookup"),
    );

    let error = witness
        .fetch(&Digest::of(b"generation"))
        .expect_err("an unavailable remote cannot establish absence");

    assert!(matches!(error, WitnessError::Git { ref reason, .. } if !reason.is_empty()));
}

#[test]
fn unavailable_remote_refuses_publication_before_preparing_a_record() {
    let fixture = Fixture::new();
    let workdir = fixture.path("publication");
    let witness = GitWitness::new(
        &fixture.path("unavailable.git"),
        "skill-generations",
        &workdir,
    );

    let error = witness
        .publish(&Digest::of(b"generation"), b"signed bytes")
        .expect_err("an unavailable remote cannot authorize branch bootstrap");

    assert!(matches!(error, WitnessError::Git { ref reason, .. } if !reason.is_empty()));
    assert!(
        !workdir.join("generations").exists(),
        "publication must stop before preparing a fresh record",
    );
}

#[test]
fn absent_branch_bootstraps_and_missing_records_remain_absent() {
    let fixture = Fixture::new();
    let witness = GitWitness::new(
        &fixture.witness_remote(),
        "skill-generations",
        &fixture.path("witness"),
    );
    let first = Digest::of(b"first generation");
    let second = Digest::of(b"second generation");

    assert_eq!(
        witness.fetch(&first).expect("empty remote is readable"),
        None
    );
    let published = witness.publish(&first, b"first bytes").expect("bootstrap");
    let (bytes, evidence) = witness
        .fetch(&first)
        .expect("lookup")
        .expect("first record");
    assert_eq!(bytes, b"first bytes");
    assert_eq!(evidence.commit, published.commit);
    assert_eq!(witness.fetch(&second).expect("missing record lookup"), None);

    witness.publish(&second, b"second bytes").expect("append");
    assert_eq!(witness.fetch(&first).unwrap().unwrap().0, b"first bytes");
    assert_eq!(witness.fetch(&second).unwrap().unwrap().0, b"second bytes");
}

#[test]
fn a_directory_at_the_record_path_is_an_error_not_absence() {
    let fixture = Fixture::new();
    let remote = fixture.witness_remote();
    let workdir = fixture.path("publication");
    let witness = GitWitness::new(&remote, "skill-generations", &workdir);
    witness.publish(&Digest::of(b"first"), b"bytes").unwrap();
    let malformed = Digest::of(b"malformed");
    support::write_file(
        &workdir.join(format!(
            "generations/{}.json/child",
            malformed.directory_name()
        )),
        "a directory where a blob should be",
    );
    for arguments in [
        vec!["add", "generations"],
        vec!["commit", "-q", "-m", "malformed record"],
        vec![
            "push",
            "-q",
            remote.to_str().unwrap(),
            "HEAD:refs/heads/skill-generations",
        ],
    ] {
        assert!(
            std::process::Command::new("git")
                .current_dir(&workdir)
                .args(arguments)
                .status()
                .unwrap()
                .success()
        );
    }

    let error = witness
        .fetch(&malformed)
        .expect_err("malformed witness record");
    assert!(matches!(error, WitnessError::Git { ref command, .. } if command == "cat-file"));
}
