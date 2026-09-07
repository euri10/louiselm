//! Durable, cross-process trust mutations through the public API and CLI.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

mod support;

use std::{
    fs::{self, File, OpenOptions},
    io::Read,
    os::unix::fs::{MetadataExt, symlink},
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

use louiselm_skills::{
    sshsig::SkPolicy,
    trust::{
        Role, TrustError, TrustStore,
        recovery::{
            self, POSSESSION_NAMESPACE, RECOVERY_NAMESPACE, RecoveryAuthorization, RecoveryChange,
            RecoveryConfirmation, RecoveryError, ReplacementProof,
        },
    },
};
use rustix::fs::{FlockOperation, flock};
use support::{Fixture, SshKey, write_file};

const BINARY: &str = env!("CARGO_BIN_EXE_louiselm-skills");

fn run(fixture: &Fixture, arguments: &[&str]) -> Output {
    let mut child = Command::new(BINARY)
        .args(arguments)
        .env("LOUISELM_SKILLS_STORE", fixture.store_root())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("the CLI runs");
    let deadline = Instant::now() + Duration::from_secs(10);
    while child.try_wait().expect("child status").is_none() {
        if Instant::now() >= deadline {
            child.kill().expect("stop hung CLI");
            child.wait().expect("reap hung CLI");
            panic!("trust CLI blocked: {arguments:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    child.wait_with_output().expect("CLI output")
}

fn assert_refused(output: &Output, reason: &str) {
    assert_eq!(output.status.code(), Some(1), "{output:?}");
    assert!(output.stdout.is_empty(), "no success before persistence");
    assert!(
        String::from_utf8_lossy(&output.stderr).contains(reason),
        "{output:?}"
    );
}

fn hold_lock(fixture: &Fixture) -> File {
    fs::create_dir_all(fixture.path("store/trust")).expect("trust directory");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(fixture.path("store/trust/roles.lock"))
        .expect("persistent lock file");
    flock(&file, FlockOperation::NonBlockingLockExclusive).expect("hold storage lock");
    file
}

fn bootstrap(fixture: &Fixture) -> (TrustStore, SshKey) {
    let primary = SshKey::generate(fixture, "primary");
    let recovery = SshKey::generate(fixture, "recovery");
    let trust = TrustStore::bootstrap(
        &fixture.store(),
        "test/trust",
        &primary.public_key(),
        &recovery.public_key(),
        SkPolicy::none(),
        1,
    )
    .expect("bootstrap succeeds");
    (trust, recovery)
}

#[test]
fn another_process_holding_the_lock_refuses_bootstrap_and_reset() {
    let fixture = Fixture::new();
    let primary = SshKey::generate(&fixture, "primary");
    let recovery = SshKey::generate(&fixture, "recovery");
    let arguments = [
        "trust",
        "bootstrap",
        "--primary",
        &primary.public_key(),
        "--release",
        &recovery.public_key(),
    ];

    let lock = hold_lock(&fixture);
    let lock_inode = lock.metadata().expect("lock metadata").ino();
    assert_refused(&run(&fixture, &arguments), "busy");
    assert!(!fixture.path("store/trust/roles.json").exists());
    drop(lock);
    assert!(run(&fixture, &arguments).status.success());

    let original = fs::read(fixture.path("store/trust/roles.json")).expect("enrollment");
    let lock = hold_lock(&fixture);
    assert_refused(&run(&fixture, &["trust", "reset", "--confirm"]), "busy");
    assert_eq!(
        fs::read(fixture.path("store/trust/roles.json")).expect("unchanged enrollment"),
        original
    );
    drop(lock);
    assert!(
        run(&fixture, &["trust", "reset", "--confirm"])
            .status
            .success()
    );
    assert!(run(&fixture, &arguments).status.success());
    assert_eq!(
        fs::metadata(fixture.path("store/trust/roles.lock"))
            .expect("reset must retain the lock inode")
            .ino(),
        lock_inode
    );
}

#[test]
fn rotation_is_locked_atomic_and_cannot_replay() {
    let fixture = Fixture::new();
    let (trust, recovery) = bootstrap(&fixture);
    let replacement = SshKey::generate(&fixture, "replacement");
    let change = support::key_change(&trust, Role::Primary, &replacement.public_key());
    let signature = recovery.sign(RECOVERY_NAMESPACE, &change.canonical_bytes());
    let path = fixture.path("store/trust/roles.json");
    let original = fs::read(&path).expect("enrollment");
    let mut previous_snapshot = File::open(&path).expect("reader before publication");

    let lock = hold_lock(&fixture);
    assert!(matches!(
        support::apply_key_change(&fixture.store(), &change, &signature, &replacement, 2),
        Err(RecoveryError::Trust(TrustError::Busy(_)))
    ));
    assert_eq!(fs::read(&path).expect("enrollment"), original);
    drop(lock);
    support::apply_key_change(&fixture.store(), &change, &signature, &replacement, 2)
        .expect("rotation succeeds");

    let mut previous_bytes = Vec::new();
    previous_snapshot
        .read_to_end(&mut previous_bytes)
        .expect("old reader completes");
    assert_eq!(
        previous_bytes, original,
        "publication must replace, never truncate"
    );
    let current = TrustStore::load(&fixture.store())
        .expect("readable")
        .expect("enrolled");
    assert_eq!(current.sequence, 1);
    assert_eq!(
        current.admission_key().expect("primary").public_key,
        replacement.public_key()
    );
    assert_eq!(
        current.retired,
        vec![trust.admission_key().expect("old primary").clone()]
    );

    assert!(matches!(
        support::apply_key_change(&fixture.store(), &change, &signature, &replacement, 3),
        Err(RecoveryError::InvalidChange(_))
    ));
    assert_eq!(
        TrustStore::load(&fixture.store()).expect("readable"),
        Some(current)
    );
}

#[test]
fn rotation_replaces_the_inode_even_without_a_contending_writer() {
    let fixture = Fixture::new();
    let (trust, recovery) = bootstrap(&fixture);
    let replacement = SshKey::generate(&fixture, "replacement");
    let path = fixture.path("store/trust/roles.json");
    let mut reader = File::open(&path).expect("old snapshot");
    let before = fs::read(&path).expect("old bytes");
    let change = support::key_change(&trust, Role::Primary, &replacement.public_key());
    let signature = recovery.sign(RECOVERY_NAMESPACE, &change.canonical_bytes());

    support::apply_key_change(&fixture.store(), &change, &signature, &replacement, 2)
        .expect("rotation succeeds");
    let mut old_bytes = Vec::new();
    reader
        .read_to_end(&mut old_bytes)
        .expect("reader survives publication");
    assert_eq!(
        old_bytes, before,
        "in-flight readers must see a complete old state"
    );
    assert_ne!(fs::read(&path).expect("new bytes"), before);
}

#[test]
fn malformed_existing_state_is_not_silently_replaced() {
    let fixture = Fixture::new();
    let (trust, recovery) = bootstrap(&fixture);
    let path = fixture.path("store/trust/roles.json");
    fs::write(&path, b"interrupted JSON").expect("corrupt enrollment");
    let refusal = TrustStore::bootstrap(
        &fixture.store(),
        "test/trust",
        "primary",
        "recovery",
        SkPolicy::none(),
        2,
    );
    assert!(
        matches!(refusal, Err(TrustError::Malformed(_))),
        "{refusal:?}"
    );
    let replacement = SshKey::generate(&fixture, "replacement");
    let change = support::key_change(&trust, Role::Primary, &replacement.public_key());
    let signature = recovery.sign(RECOVERY_NAMESPACE, &change.canonical_bytes());
    assert!(matches!(
        support::apply_key_change(&fixture.store(), &change, &signature, &replacement, 2),
        Err(RecoveryError::Trust(TrustError::Malformed(_)))
    ));
    assert_eq!(
        fs::read(&path).expect("original corrupt bytes"),
        b"interrupted JSON"
    );
    assert_refused(&run(&fixture, &["trust", "reset"]), "--confirm");
    assert!(
        run(&fixture, &["trust", "reset", "--confirm"])
            .status
            .success()
    );
}

#[test]
fn symlinked_trust_paths_never_redirect_bootstrap() {
    for entry in ["trust", "trust/roles.json", "trust/roles.lock"] {
        let fixture = Fixture::new();
        let store = fixture.store();
        let target = fixture.path("unrelated");
        if entry == "trust" {
            fs::create_dir(&target).expect("unrelated directory");
        } else {
            fs::create_dir(fixture.path("store/trust")).expect("trust directory");
        }
        symlink(&target, store.root().join(entry)).expect("redirecting symlink");
        assert!(
            TrustStore::bootstrap(
                &store,
                "test/trust",
                "primary",
                "recovery",
                SkPolicy::none(),
                1
            )
            .is_err(),
            "must refuse {entry}"
        );
        if entry == "trust" {
            assert_eq!(fs::read_dir(&target).expect("directory").count(), 0);
        } else {
            assert!(!target.exists(), "must not create the symlink target");
        }
    }
}

#[test]
fn aliased_state_cannot_be_read_rotated_or_reset() {
    for hard_link in [false, true] {
        let fixture = Fixture::new();
        let (trust, recovery) = bootstrap(&fixture);
        let state = fixture.path("store/trust/roles.json");
        let unrelated = fixture.path("unrelated.json");
        fs::rename(&state, &unrelated).expect("move enrollment out of store");
        if hard_link {
            fs::hard_link(&unrelated, &state).expect("alias enrollment");
        } else {
            symlink(&unrelated, &state).expect("redirect enrollment");
        }
        let original = fs::read(&unrelated).expect("unrelated bytes");
        let replacement = SshKey::generate(&fixture, "replacement");
        let change = support::key_change(&trust, Role::Primary, &replacement.public_key());
        let signature = recovery.sign(RECOVERY_NAMESPACE, &change.canonical_bytes());

        assert!(
            TrustStore::load(&fixture.store()).is_err(),
            "refuse aliased trust"
        );
        assert!(
            support::apply_key_change(&fixture.store(), &change, &signature, &replacement, 2)
                .is_err()
        );
        assert!(TrustStore::reset(&fixture.store()).is_err());
        assert_eq!(
            fs::read(&unrelated).expect("preserved unrelated file"),
            original
        );
        assert!(
            fs::symlink_metadata(&state).is_ok(),
            "refuse, do not remove alias"
        );
    }
}

#[cfg(target_os = "linux")]
#[test]
fn failed_staging_write_preserves_enrollment_and_reports_no_success() {
    const MARKER: &str = "LOUISELM_TEST_KEY_WRITE_LIMIT";
    if let Some(path) = std::env::var_os(MARKER) {
        let root = std::path::Path::new(&path);
        let store = louiselm_skills::Store::open(root).unwrap();
        let input: serde_json::Value =
            serde_json::from_slice(&fs::read(root.join("change.json")).unwrap()).unwrap();
        let change: RecoveryChange = serde_json::from_value(input["change"].clone()).unwrap();
        let proof = ReplacementProof {
            role: Role::Primary,
            signature: input["proof"].as_str().unwrap().to_owned(),
        };
        let result = recovery::apply(
            &store,
            &change,
            RecoveryAuthorization::SigningKey {
                role: Role::Release,
                signature: input["signature"].as_str().unwrap(),
            },
            &[proof],
            RecoveryConfirmation::default(),
            2,
        );
        assert!(
            matches!(result, Err(RecoveryError::Trust(TrustError::Io { .. }))),
            "{result:?}"
        );
        return;
    }
    let fixture = Fixture::new();
    let (mut trust, recovery) = bootstrap(&fixture);
    // History exceeds the child write limit; public verification scratch still fits.
    trust.retired = vec![trust.admission_key().unwrap().clone(); 100];
    fs::write(
        fixture.path("store/trust/roles.json"),
        serde_json::to_vec(&trust).unwrap(),
    )
    .unwrap();
    let replacement = SshKey::generate(&fixture, "replacement");
    let change = support::key_change(&trust, Role::Primary, &replacement.public_key());
    fs::write(
        fixture.path("store/change.json"),
        serde_json::to_vec(&serde_json::json!({
            "signature": recovery.sign(RECOVERY_NAMESPACE, &change.canonical_bytes()),
            "proof": replacement.sign(POSSESSION_NAMESPACE, &change.canonical_bytes()),
            "change": change,
        }))
        .unwrap(),
    )
    .unwrap();
    let original = fs::read(fixture.path("store/trust/roles.json")).unwrap();
    // Real RLIMIT_FSIZE, isolated to a subprocess; no production fault-injection hook.
    let output = Command::new("/bin/sh")
        .args([
            "-c",
            r#"trap '' XFSZ; ulimit -f 4; exec "$@""#,
            "trust-write-limit",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "failed_staging_write_preserves_enrollment_and_reports_no_success",
            "--nocapture",
        ])
        .env(MARKER, fixture.store_root())
        .output()
        .unwrap();
    assert!(output.status.success(), "{output:?}");
    assert_eq!(
        fs::read(fixture.path("store/trust/roles.json")).unwrap(),
        original
    );
    assert!(
        fs::read_dir(fixture.path("store/trust"))
            .unwrap()
            .all(|entry| {
                matches!(
                    entry.unwrap().file_name().to_str(),
                    Some("roles.json" | "roles.lock")
                )
            }),
        "failed staging must leave no new partial file"
    );
}

#[test]
fn non_regular_state_is_refused_without_blocking() {
    for directory in [false, true] {
        let fixture = Fixture::new();
        let store = fixture.store();
        fs::create_dir(store.root().join("trust")).expect("trust directory");
        let state = store.root().join("trust/roles.json");
        if directory {
            fs::create_dir(&state).expect("misplaced directory");
        } else {
            rustix::fs::mknodat(
                rustix::fs::CWD,
                &state,
                rustix::fs::FileType::Fifo,
                rustix::fs::Mode::RUSR | rustix::fs::Mode::WUSR,
                0,
            )
            .expect("misplaced FIFO");
        }
        assert_refused(&run(&fixture, &["trust", "show"]), "regular file");
        assert_refused(
            &run(&fixture, &["trust", "reset", "--confirm"]),
            "regular file",
        );
        assert!(
            fs::symlink_metadata(&state).is_ok(),
            "unexpected target is preserved"
        );
    }
}

#[test]
fn a_colliding_temporary_is_neither_overwritten_nor_cleaned_up() {
    let fixture = Fixture::new();
    let store = fixture.store();
    fs::create_dir(store.root().join("trust")).expect("trust directory");
    let unrelated = fixture.path("unrelated.txt");
    write_file(&unrelated, "unrelated data");
    let output = Command::new("/bin/sh")
        .args([
            "-c",
            "ln -s \"$1\" \"$LOUISELM_SKILLS_STORE/trust/.roles-$$-0.pending\" || exit 99; shift; exec \"$@\"",
            "trust-temp-collision",
        ])
        .arg(&unrelated)
        .args([BINARY, "trust", "bootstrap", "--primary", "primary", "--release", "recovery"])
        .env("LOUISELM_SKILLS_STORE", store.root())
        .output()
        .expect("CLI with colliding temporary");
    assert_refused(&output, "I/O failed");
    assert_eq!(
        fs::read_to_string(unrelated).expect("unrelated file"),
        "unrelated data"
    );
    assert!(!store.root().join("trust/roles.json").exists());
    assert_eq!(
        fs::read_dir(store.root().join("trust"))
            .expect("trust directory")
            .filter(|entry| entry.as_ref().expect("entry").file_name() != "roles.lock")
            .count(),
        1,
        "the pre-existing temporary must not be removed"
    );
}

#[test]
fn exhausted_trust_counter_is_a_refusal_not_a_panic() {
    let fixture = Fixture::new();
    let (mut trust, _) = bootstrap(&fixture);
    trust.sequence = u64::MAX;
    let bytes = serde_json::to_vec(&trust).expect("JSON");
    fs::write(fixture.path("store/trust/roles.json"), &bytes).expect("exhausted state");
    assert!(matches!(
        RecoveryChange::new(&trust, vec![], None),
        Err(RecoveryError::Trust(TrustError::SequenceExhausted))
    ));
    assert_eq!(
        fs::read(fixture.path("store/trust/roles.json")).expect("unchanged state"),
        bytes
    );
}

#[test]
fn last_trust_change_is_valid_then_all_successor_paths_refuse_without_wrapping() {
    let fixture = Fixture::new();
    let (mut trust, recovery) = bootstrap(&fixture);
    trust.sequence = u64::MAX - 1;
    fs::write(
        fixture.path("store/trust/roles.json"),
        serde_json::to_vec(&trust).unwrap(),
    )
    .unwrap();
    let replacement = SshKey::generate(&fixture, "replacement");
    let change = support::key_change(&trust, Role::Primary, &replacement.public_key());
    assert_eq!(change.sequence, u64::MAX);
    let trust = support::apply_key_change(
        &fixture.store(),
        &change,
        &recovery.sign(RECOVERY_NAMESPACE, &change.canonical_bytes()),
        &replacement,
        3,
    )
    .unwrap();
    let before = fs::read(fixture.path("store/trust/roles.json")).unwrap();
    assert!(matches!(
        support::apply_key_change(&fixture.store(), &change, "unused", &replacement, 4),
        Err(RecoveryError::Trust(TrustError::SequenceExhausted))
    ));
    assert!(matches!(
        trust.next_sequence(),
        Err(louiselm_skills::trust::TrustError::SequenceExhausted)
    ));
    assert_eq!(
        fs::read(fixture.path("store/trust/roles.json")).unwrap(),
        before
    );
}
