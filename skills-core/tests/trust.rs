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
    sshsig::{SkPolicy, TRUST_NAMESPACE},
    trust::{Role, TrustError, TrustStore},
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
        "--recovery",
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
    let change = trust.rotation_payload(Role::Primary, &replacement.public_key(), SkPolicy::none());
    let signature = recovery.sign(TRUST_NAMESPACE, &change.canonical_bytes());
    write_file(&fixture.path("rotation.sig"), &signature);
    let signature_path = fixture.path("rotation.sig");
    let arguments = [
        "trust",
        "rotate",
        "--role",
        "primary",
        "--key",
        &replacement.public_key(),
        "--signature",
        signature_path.to_str().expect("UTF-8 path"),
    ];
    let path = fixture.path("store/trust/roles.json");
    let original = fs::read(&path).expect("enrollment");
    let mut previous_snapshot = File::open(&path).expect("reader before publication");

    let lock = hold_lock(&fixture);
    assert_refused(&run(&fixture, &arguments), "busy");
    assert_eq!(fs::read(&path).expect("enrollment"), original);
    drop(lock);
    let output = run(&fixture, &arguments);
    assert!(output.status.success(), "{output:?}");

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
        TrustStore::rotate(&fixture.store(), &change, &signature, 3),
        Err(TrustError::DoesNotApply(_))
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
    let change = trust.rotation_payload(Role::Primary, &replacement.public_key(), SkPolicy::none());
    let signature = recovery.sign(TRUST_NAMESPACE, &change.canonical_bytes());

    TrustStore::rotate(&fixture.store(), &change, &signature, 2).expect("rotation succeeds");
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
    assert!(matches!(
        TrustStore::bootstrap(
            &fixture.store(),
            "test/trust",
            "primary",
            "recovery",
            SkPolicy::none(),
            2
        ),
        Err(TrustError::Malformed(_))
    ));
    let change = trust.rotation_payload(Role::Primary, "replacement", SkPolicy::none());
    let signature = recovery.sign(TRUST_NAMESPACE, &change.canonical_bytes());
    assert!(matches!(
        TrustStore::rotate(&fixture.store(), &change, &signature, 2),
        Err(TrustError::Malformed(_))
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
        let change = trust.rotation_payload(Role::Primary, "replacement", SkPolicy::none());
        let signature = recovery.sign(TRUST_NAMESPACE, &change.canonical_bytes());

        assert!(
            TrustStore::load(&fixture.store()).is_err(),
            "refuse aliased trust"
        );
        assert!(TrustStore::rotate(&fixture.store(), &change, &signature, 2).is_err());
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
    let fixture = Fixture::new();
    let (mut trust, recovery) = bootstrap(&fixture);
    // History makes the state larger than the child write limit while signature
    // verification's temporary public payload and allowed-signers file still fit.
    trust.retired = vec![trust.admission_key().expect("primary").clone(); 100];
    fs::write(
        fixture.path("store/trust/roles.json"),
        serde_json::to_vec(&trust).expect("JSON"),
    )
    .expect("enrollment with history");
    let replacement = SshKey::generate(&fixture, "replacement");
    let change = trust.rotation_payload(Role::Primary, &replacement.public_key(), SkPolicy::none());
    write_file(
        &fixture.path("rotation.sig"),
        &recovery.sign(TRUST_NAMESPACE, &change.canonical_bytes()),
    );
    let original = fs::read(fixture.path("store/trust/roles.json")).expect("enrollment");
    // Limit only the child CLI's writes. Ignoring SIGXFSZ lets its real filesystem
    // error reach the caller; no global process state or injected production hook.
    let output = Command::new("/bin/sh")
        .args([
            "-c",
            "trap '' XFSZ; ulimit -f 4; exec \"$@\"",
            "trust-write-limit",
            BINARY,
        ])
        .args([
            "trust",
            "rotate",
            "--role",
            "primary",
            "--key",
            &replacement.public_key(),
            "--signature",
        ])
        .arg(fixture.path("rotation.sig"))
        .env("LOUISELM_SKILLS_STORE", fixture.store_root())
        .output()
        .expect("limited CLI runs");
    assert_refused(&output, "I/O failed");
    assert!(
        fs::read(fixture.path("store/trust/roles.json")).expect("enrollment") == original,
        "a failed write must preserve every previous byte"
    );
    assert!(
        fs::read_dir(fixture.path("store/trust"))
            .expect("trust directory")
            .all(|entry| {
                matches!(
                    entry.expect("directory entry").file_name().to_str(),
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
        .args([BINARY, "trust", "bootstrap", "--primary", "primary", "--recovery", "recovery"])
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
