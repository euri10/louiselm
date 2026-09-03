//! Store behaviour: a digest names one sequence of bytes, and nothing else.

mod support;

use std::{fs, os::unix::fs::PermissionsExt};

use louiselm_skills::{Policy, PublishOutcome, StoreError, VerifyFailure};
use support::{Fixture, write_file};

#[test]
fn republishing_identical_bytes_changes_nothing() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");

    let (first, first_outcome) = fixture.capture(&candidate).expect("first capture succeeds");
    let (second, second_outcome) = fixture
        .capture(&candidate)
        .expect("second capture succeeds");

    assert_eq!(first.digest, second.digest);
    assert_eq!(first_outcome, PublishOutcome::Created);
    assert_eq!(second_outcome, PublishOutcome::Existing);
    assert_eq!(fixture.store().list().expect("store lists").len(), 1);
}

#[test]
fn a_digest_already_present_is_never_replaced_by_different_bytes() {
    let fixture = Fixture::new();
    let honest = fixture.candidate("honest");
    write_file(&honest.join("SKILL.md"), "honest\n");
    let hostile = fixture.candidate("hostile");
    write_file(&hostile.join("SKILL.md"), "hostile\n");

    let (honest_package, _) = fixture.capture(&honest).expect("honest capture succeeds");
    let (hostile_package, _) = fixture.capture(&hostile).expect("hostile capture succeeds");

    // Stand in for a SHA-256 collision: give the hostile bytes the honest
    // digest's directory, which is the only way the two can contend for one
    // address without breaking the hash.
    let packages = fixture.store_root().join("packages");
    fs::remove_dir_all(packages.join(honest_package.digest.directory_name()))
        .expect("honest package is removable");
    fs::rename(
        packages.join(hostile_package.digest.directory_name()),
        packages.join(honest_package.digest.directory_name()),
    )
    .expect("hostile package is movable");

    let error = fixture
        .capture(&honest)
        .expect_err("the contested digest is refused");

    assert!(
        matches!(error, StoreError::DigestConflict { .. }),
        "unexpected error: {error}",
    );
}

#[test]
fn verification_recomputes_content_rather_than_trusting_the_manifest() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");
    let (package, _) = fixture.capture(&candidate).expect("capture succeeds");
    let policy = Policy::embedded();

    let report = fixture
        .store()
        .verify(&package.digest, &policy)
        .expect("verification runs");
    assert!(
        report.is_intact(),
        "a fresh package verifies: {}",
        report.summary()
    );

    let stored = package.file_path("SKILL.md");
    fs::set_permissions(&stored, fs::Permissions::from_mode(0o644)).expect("mode is settable");
    fs::write(&stored, "tampered\n").expect("stored file is writable once unlocked");

    let report = fixture
        .store()
        .verify(&package.digest, &policy)
        .expect("verification runs");
    assert!(!report.is_intact());
    assert!(
        report
            .failures
            .iter()
            .any(|failure| matches!(failure, VerifyFailure::ContentMismatch { path, .. } if path == "SKILL.md")),
        "unexpected failures: {:?}",
        report.failures,
    );

    let error = fixture
        .capture(&candidate)
        .expect_err("republishing over tampered bytes is refused");
    assert!(
        matches!(error, StoreError::Tampered { .. }),
        "unexpected error: {error}",
    );
}

#[test]
fn verification_reports_content_the_manifest_never_named() {
    let fixture = Fixture::new();
    let candidate = fixture.candidate("candidate");
    write_file(&candidate.join("SKILL.md"), "body\n");
    let (package, _) = fixture.capture(&candidate).expect("capture succeeds");

    let smuggled = package.file_path("smuggled.md");
    fs::write(&smuggled, "smuggled\n").expect("file is writable");

    let report = fixture
        .store()
        .verify(&package.digest, &Policy::embedded())
        .expect("verification runs");

    assert!(
        report.failures.iter().any(
            |failure| matches!(failure, VerifyFailure::Unexpected { path } if path == "smuggled.md")
        ),
        "unexpected failures: {:?}",
        report.failures,
    );
}

#[test]
fn lineage_accumulates_every_local_source_of_one_package() {
    let fixture = Fixture::new();
    let first = fixture.candidate("first");
    write_file(&first.join("SKILL.md"), "body\n");
    let second = fixture.candidate("second");
    write_file(&second.join("SKILL.md"), "body\n");

    let (package, _) = fixture.capture(&first).expect("first capture succeeds");
    fixture.capture(&second).expect("second capture succeeds");

    let lineage = fixture
        .store()
        .lineage(&package.digest)
        .expect("lineage is readable")
        .expect("lineage was recorded");

    assert_eq!(lineage.captures.len(), 2);
    assert!(lineage.captures[0].source_root.ends_with("/first"));
    assert!(lineage.captures[1].source_root.ends_with("/second"));
    assert_eq!(lineage.package_digest, package.digest.to_string());
}

#[test]
fn an_unknown_digest_is_reported_rather_than_invented() {
    let fixture = Fixture::new();
    let absent = louiselm_skills::Digest::of(b"never captured");

    let error = fixture
        .store()
        .open_package(&absent, &Policy::embedded())
        .expect_err("an absent package is refused");

    assert!(
        matches!(error, StoreError::UnknownPackage(_)),
        "unexpected error: {error}",
    );
}
