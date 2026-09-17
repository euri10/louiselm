//! Dependency authority never follows mutable lockfiles or Agent assertions.
#![allow(clippy::unwrap_used, reason = "Tests abort on fixture failures.")]

use louiselm_skills::{
    Digest,
    cache::CacheBase,
    dependency_fetch::{
        Attendance, Candidate, Decision, DependencyPolicy, DependencySession, Source,
        StartingLockfile,
    },
    sandbox::IdentityPlan,
};

fn candidate() -> Candidate {
    Candidate {
        name: "example".into(),
        version: "1.2.3".into(),
        source: Source::Registry {
            registry: "crates-io".into(),
        },
        integrity: Some(Digest::of(b"archive").to_string()),
    }
}

fn lockfile() -> Vec<u8> {
    format!(
        "version = 4\n[[package]]\nname = \"example\"\nversion = \"1.2.3\"\nsource = \"registry+https://github.com/rust-lang/crates.io-index\"\nchecksum = \"{}\"\n",
        Digest::of(b"archive").hex()
    ).into_bytes()
}

fn session(attendance: Attendance, preapproved: Vec<Candidate>) -> DependencySession {
    let bytes = lockfile();
    let starting = StartingLockfile::cargo(&bytes).unwrap();
    DependencySession::new(
        DependencyPolicy {
            session_id: "session-one".into(),
            run_id: "run-one".into(),
            envelope_revision: 1,
            attendance,
            starting,
            preapproved,
            expires_at_ms: 1_000,
            max_fetches: 4,
            max_bytes: 1_024,
        },
        100,
    )
    .unwrap()
}

#[test]
fn only_exact_starting_registry_coordinates_and_integrity_are_automatic() {
    let mut owner = session(Attendance::Interactive, vec![]);
    assert_eq!(
        owner.consider(&candidate(), 101).unwrap(),
        Decision::Authorized
    );
    for changed in [
        Candidate {
            version: "1.2.4".into(),
            ..candidate()
        },
        Candidate {
            name: "secret-from-prompt".into(),
            ..candidate()
        },
        Candidate {
            integrity: Some(Digest::of(b"altered").to_string()),
            ..candidate()
        },
    ] {
        assert!(matches!(
            owner.consider(&changed, 101).unwrap(),
            Decision::Pending { .. }
        ));
    }
    assert_eq!(owner.pending().len(), 3);
}

#[test]
fn approvals_are_exact_batched_and_never_available_mid_unattended_run() {
    let other = Candidate {
        name: "new-dependency".into(),
        ..candidate()
    };
    let mut interactive = session(Attendance::Interactive, vec![]);
    let Decision::Pending { candidate_id } = interactive.consider(&other, 101).unwrap() else {
        unreachable!()
    };
    assert!(
        interactive
            .approve_batch(&[candidate_id.clone(), "unknown".into()], 102)
            .is_err()
    );
    assert!(matches!(
        interactive.consider(&other, 103).unwrap(),
        Decision::Pending { .. }
    ));
    interactive.approve_batch(&[candidate_id], 104).unwrap();
    assert_eq!(
        interactive.consider(&other, 105).unwrap(),
        Decision::Authorized
    );
    let mut unattended = session(Attendance::Unattended, vec![]);
    assert_eq!(unattended.consider(&other, 101).unwrap(), Decision::Denied);
    assert!(
        unattended.pending().is_empty(),
        "unattended Runs never prompt"
    );
    assert!(unattended.approve_batch(&[], 102).is_err());
    let mut approved = session(Attendance::Unattended, vec![other.clone()]);
    assert_eq!(
        approved.consider(&other, 101).unwrap(),
        Decision::Authorized
    );
}

#[test]
fn exceptional_sources_and_missing_integrity_always_need_explicit_approval() {
    let exceptional = [
        Candidate {
            integrity: None,
            ..candidate()
        },
        Candidate {
            source: Source::Git {
                repository: "https://example.invalid/repo".into(),
                revision: "a".repeat(40),
            },
            ..candidate()
        },
        Candidate {
            source: Source::Url {
                url: "https://example.invalid/archive".into(),
            },
            ..candidate()
        },
        Candidate {
            source: Source::Other {
                locator: "custom:package".into(),
            },
            ..candidate()
        },
    ];
    for dependency in exceptional {
        for attendance in [Attendance::Interactive, Attendance::Unattended] {
            let mut owner = session(attendance, vec![dependency.clone()]);
            assert_eq!(
                owner.consider(&dependency, 101).unwrap(),
                Decision::Authorized
            );
        }
        let mut owner = session(Attendance::Interactive, vec![]);
        assert!(matches!(
            owner.consider(&dependency, 101).unwrap(),
            Decision::Pending { .. }
        ));
    }
}

#[test]
fn expiry_revocation_and_changed_binding_stop_authorization() {
    let mut owner = session(Attendance::Interactive, vec![]);
    assert!(
        owner
            .check_binding("session-two", "run-one", 1, 101)
            .is_err()
    );
    assert!(
        owner
            .check_binding("session-one", "run-two", 1, 101)
            .is_err()
    );
    assert!(
        owner
            .check_binding("session-one", "run-one", 2, 101)
            .is_err()
    );
    assert!(owner.consider(&candidate(), 1_000).is_err());
    owner.revoke();
    assert!(owner.consider(&candidate(), 101).is_err());
}

#[test]
fn cargo_capture_binds_bytes_and_rejects_duplicate_or_malformed_checksums() {
    let mut bytes = lockfile();
    let captured = StartingLockfile::cargo(&bytes).unwrap();
    let digest = captured.digest().to_owned();
    bytes.extend_from_slice(b"\n# later Agent edit\n");
    assert_ne!(StartingLockfile::cargo(&bytes).unwrap().digest(), digest);
    assert_eq!(captured.entries(), &[candidate()]);
    assert!(StartingLockfile::cargo(&[lockfile(), lockfile()].concat()).is_err());
    let bad = String::from_utf8(lockfile())
        .unwrap()
        .replace(Digest::of(b"archive").hex(), "wrong");
    assert!(StartingLockfile::cargo(bad.as_bytes()).is_err());
    assert!(serde_json::from_value::<Candidate>(serde_json::json!({
        "name":"example", "version":"1.2.3", "source":{"kind":"registry","registry":"crates-io"},
        "integrity":null, "approved":true
    })).is_err());
}

#[test]
fn starting_git_and_integrity_less_registry_entries_are_never_automatic() {
    let checksum = format!("checksum = \"{}\"\n", Digest::of(b"archive").hex());
    for bytes in [
        String::from_utf8(lockfile())
            .unwrap()
            .replace(&checksum, ""),
        String::from_utf8(lockfile()).unwrap().replace(
            "registry+https://github.com/rust-lang/crates.io-index",
            &format!("git+https://example.invalid/repo#{}", "a".repeat(40)),
        ),
    ] {
        let starting = StartingLockfile::cargo(bytes.as_bytes()).unwrap();
        let entry = starting.entries()[0].clone();
        let mut owner = DependencySession::new(
            DependencyPolicy {
                session_id: "session-one".into(),
                run_id: "run-one".into(),
                envelope_revision: 1,
                attendance: Attendance::Interactive,
                starting,
                preapproved: vec![],
                expires_at_ms: 1_000,
                max_fetches: 4,
                max_bytes: 1_024,
            },
            100,
        )
        .unwrap();
        assert!(matches!(
            owner.consider(&entry, 101).unwrap(),
            Decision::Pending { .. }
        ));
    }
}

#[test]
fn fetch_permits_spend_bounded_authority_and_recheck_the_overlay_and_lifetime() {
    use std::{fs, os::unix::fs::PermissionsExt};
    let root = tempfile::tempdir().unwrap();
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    let source = root.path().join("base");
    fs::create_dir(&source).unwrap();
    let base = CacheBase::capture(&source).unwrap();
    let mut overlay = base
        .materialize(root.path(), "session-one", IdentityPlan::NamespaceOnly)
        .unwrap();
    let mut foreign = base
        .materialize(root.path(), "session-two", IdentityPlan::NamespaceOnly)
        .unwrap();
    let mut owner = session(Attendance::Interactive, vec![]);
    let permit = owner.begin_fetch(&candidate(), 256, 101).unwrap();
    assert!(
        owner
            .finish_fetch(permit, b"archive", &mut foreign, 102)
            .is_err()
    );
    assert_eq!(fs::read_dir(foreign.path()).unwrap().count(), 0);
    let permit = owner.begin_fetch(&candidate(), 256, 103).unwrap();
    assert!(
        owner
            .finish_fetch(permit, b"corrupt", &mut overlay, 104)
            .is_err()
    );
    let permit = owner.begin_fetch(&candidate(), 256, 105).unwrap();
    let artifact = owner
        .finish_fetch(permit, b"archive", &mut overlay, 106)
        .unwrap();
    assert_eq!(
        fs::read(overlay.path().join(&artifact.name)).unwrap(),
        b"archive"
    );
    assert!(artifact.integrity_verified);
    let permit = owner.begin_fetch(&candidate(), 256, 107).unwrap();
    assert!(
        owner.begin_fetch(&candidate(), 1, 108).is_err(),
        "attempts and byte reservations are never refunded"
    );
    owner.revoke();
    assert!(
        owner
            .finish_fetch(permit, b"archive", &mut overlay, 109)
            .is_err()
    );
}

#[test]
fn candidate_names_cannot_reach_a_transport_before_approval() {
    let mut owner = session(Attendance::Interactive, vec![]);
    let malicious = Candidate {
        name: "secret-from-prompt".into(),
        ..candidate()
    };
    assert!(owner.begin_fetch(&malicious, 256, 101).is_err());
    let permit = owner.begin_fetch(&candidate(), 256, 102).unwrap();
    assert_eq!(permit.candidate(), &candidate());
    assert_eq!(permit.max_bytes(), 256);
}
