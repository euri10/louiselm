//! The private mirror follows signed Admissions and never publishes unsafe paths.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Fixtures assert the CLI, signed Admission and filesystem behavior."
)]

mod support;

use std::{
    fs,
    io::Read,
    os::unix::fs::{PermissionsExt, symlink},
    path::{Path, PathBuf},
    process::{Command, Output},
};

use louiselm_skills::{
    Digest, Policy, ReviewDepth, Signer, SshKeygenSigner, TrustStore,
    admission::{self, AdmissionMember, AdmissionRequest},
    dossier::{Dossier, DossierRequest},
    signer::SignerError,
    sshsig::SkPolicy,
    witness::GitWitness,
};
use serde_json::{Value, json};
use support::{Fixture, SshKey, write_file};

const FIRST: &str = "---\ntype: louiselm/operator-profile\n---\n\nFirst private fixture.\n";
const SECOND: &str = "---\ntype: louiselm/operator-profile\n---\n\nSecond private fixture.\n";

struct World {
    fixture: Fixture,
    primary: SshKey,
}

impl World {
    fn new() -> Self {
        let fixture = Fixture::new();
        fs::set_permissions(fixture.path(""), fs::Permissions::from_mode(0o700)).unwrap();
        let primary = SshKey::generate(&fixture, "primary");
        let release = SshKey::generate(&fixture, "release");
        TrustStore::bootstrap(
            &fixture.store(),
            "louiselm/skills",
            &primary.public_key(),
            &release.public_key(),
            SkPolicy::none(),
            1,
        )
        .unwrap();
        Self { fixture, primary }
    }

    fn capture(&self, name: &str, file: &str, bytes: &str) -> Digest {
        let candidate = self.fixture.candidate(name);
        write_file(&candidate.join(file), bytes);
        self.fixture.capture(&candidate).unwrap().0.digest
    }

    fn admit(
        &self,
        members: &[&Digest],
    ) -> Result<louiselm_skills::GenerationRecord, admission::AdmissionError> {
        self.admit_with(
            members,
            &SshKeygenSigner::new(self.primary.private_key_path()),
        )
    }

    fn admit_with(
        &self,
        members: &[&Digest],
        signer: &dyn Signer,
    ) -> Result<louiselm_skills::GenerationRecord, admission::AdmissionError> {
        admission::admit(
            &self.fixture.store(),
            &Policy::embedded(),
            &AdmissionRequest {
                members: members
                    .iter()
                    .map(|package| AdmissionMember {
                        package: (*package).clone(),
                        depth: ReviewDepth::Read,
                        agents: vec!["agent".into()],
                    })
                    .collect(),
                signer,
                admitted_at_ms: 2,
                configured_agents: Some(vec!["agent".into()]),
            },
        )
    }

    fn run(&self, args: &[&str], path: Option<&Path>) -> Output {
        let mut command = Command::new(env!("CARGO_BIN_EXE_louiselm-skills"));
        command
            .args(args)
            .env("LOUISELM_SKILLS_STORE", self.fixture.store_root());
        if let Some(path) = path {
            command.arg(path);
        }
        command.output().unwrap()
    }

    fn configure(&self, path: &Path) -> Output {
        self.run(&["generation", "mirror", "--output"], Some(path))
    }

    fn mirror_status(&self) -> Value {
        let output = self.run(&["generation", "status", "--robot-json"], None);
        assert!([0, 2].contains(&output.status.code().unwrap()));
        let text = String::from_utf8(output.stdout).unwrap();
        assert!(!text.contains("private fixture"));
        serde_json::from_str::<Value>(&text).unwrap()["operator_profile_mirror"].clone()
    }
}

struct RefuseSigning;

impl Signer for RefuseSigning {
    fn sign(&self, _namespace: &str, _payload: &[u8]) -> Result<String, SignerError> {
        Err(SignerError::Failed("test signing refused".into()))
    }
}

fn linked_request<'a>(member: &AdmissionMember, signer: &'a dyn Signer) -> AdmissionRequest<'a> {
    AdmissionRequest {
        members: vec![member.clone()],
        signer,
        admitted_at_ms: 2,
        configured_agents: Some(vec!["agent".into()]),
    }
}

fn git(directory: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(directory)
        .args(args)
        .status()
        .unwrap();
    assert!(status.success());
}

fn private_checkout(world: &World) -> PathBuf {
    let root = world.fixture.path("private");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    git(&root, &["init", "-q"]);
    fs::set_permissions(root.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(root.join(".git/config"), fs::Permissions::from_mode(0o600)).unwrap();
    root
}

#[test]
fn admitted_profile_is_atomically_mirrored_and_drift_is_reported() {
    let world = World::new();
    let root = private_checkout(&world);
    let mirror = root.join("profile.md");
    let output = world.configure(&mirror);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(world.mirror_status()["state"], "no_profile");

    let first = world.capture("first", "PROFILE.md", FIRST);
    let record = world.admit(&[&first]).unwrap();
    assert_eq!(fs::read(&mirror).unwrap(), FIRST.as_bytes());
    assert_eq!(
        fs::metadata(&mirror).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(world.mirror_status()["state"], "current");
    assert_eq!(world.mirror_status()["generation"], record.generation);

    write_file(&mirror, "operator edit awaiting review");
    assert_eq!(world.mirror_status()["state"], "drift");
    let skill = world.capture(
        "skill",
        "SKILL.md",
        "---\nname: example\ndescription: Example.\n---\nBody.\n",
    );
    world.admit(&[&first, &skill]).unwrap();
    assert_eq!(fs::read(&mirror).unwrap(), b"operator edit awaiting review");

    let mut old_inode = fs::File::open(&mirror).unwrap();
    let second = world.capture("second", "PROFILE.md", SECOND);
    world.admit(&[&second, &skill]).unwrap();
    assert_eq!(fs::read(&mirror).unwrap(), SECOND.as_bytes());
    let mut old = String::new();
    old_inode.read_to_string(&mut old).unwrap();
    assert_eq!(old, "operator edit awaiting review");
    assert_eq!(world.mirror_status()["state"], "current");

    write_file(&mirror, "another edit awaiting review");
    assert_eq!(world.mirror_status()["state"], "drift");
    let output = world.configure(&mirror);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&mirror).unwrap(), SECOND.as_bytes());
    assert_eq!(world.mirror_status()["state"], "current");
}

#[test]
fn an_unconfigured_mirror_does_not_write_a_profile() {
    let world = World::new();
    let profile = world.capture("first", "PROFILE.md", FIRST);
    world.admit(&[&profile]).unwrap();
    assert_eq!(world.mirror_status(), json!({"state":"unconfigured"}));
    assert!(!world.fixture.path("profile.md").exists());
}

#[test]
fn public_checkout_symlinks_and_new_remotes_are_refused() {
    let world = World::new();
    let root = private_checkout(&world);
    let mirror = root.join("profile.md");
    git(
        &root,
        &["remote", "add", "origin", "https://example.org/public.git"],
    );
    let output = world.configure(&mirror);
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("mirror_checkout_has_remote"),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!mirror.exists());

    git(&root, &["remote", "remove", "origin"]);
    let link = world.fixture.path("link");
    symlink(&root, &link).unwrap();
    let output = world.configure(&link.join("profile.md"));
    assert!(!output.status.success());
    assert!(!mirror.exists());
    assert!(world.configure(&mirror).status.success());
    git(
        &root,
        &["remote", "add", "origin", "https://example.org/public.git"],
    );
    let profile = world.capture("first", "PROFILE.md", FIRST);
    let error = world.admit_with(&[&profile], &RefuseSigning).unwrap_err();
    assert!(error.to_string().contains("mirror_checkout_has_remote"));
    assert!(!mirror.exists());
    assert_eq!(world.mirror_status()["state"], "unsafe");
}

#[test]
fn signing_failure_and_profile_removal_do_not_overwrite_the_backup() {
    let world = World::new();
    let root = private_checkout(&world);
    let mirror = root.join("profile.md");
    assert!(world.configure(&mirror).status.success());
    let first = world.capture("first", "PROFILE.md", FIRST);
    let error = world.admit_with(&[&first], &RefuseSigning).unwrap_err();
    assert!(matches!(error, admission::AdmissionError::Signer(_)));
    assert!(!mirror.exists());
    assert_eq!(world.mirror_status()["state"], "no_profile");

    world.admit(&[&first]).unwrap();
    let skill = world.capture(
        "skill",
        "SKILL.md",
        "---\nname: example\ndescription: Example.\n---\nBody.\n",
    );
    world.admit(&[&skill]).unwrap();
    assert_eq!(world.mirror_status()["state"], "no_profile");
    assert_eq!(fs::read(&mirror).unwrap(), FIRST.as_bytes());
}

#[test]
fn unsafe_files_directories_and_store_paths_are_refused() {
    let world = World::new();
    let root = private_checkout(&world);
    let target = root.join("profile.md");
    let outside = world.fixture.path("outside.md");
    write_file(&outside, "unrelated private content");
    fs::set_permissions(&outside, fs::Permissions::from_mode(0o600)).unwrap();
    symlink(&outside, &target).unwrap();
    let output = world.configure(&target);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mirror_unsafe_file"));
    assert_eq!(fs::read(&outside).unwrap(), b"unrelated private content");
    fs::remove_file(&target).unwrap();
    fs::hard_link(&outside, &target).unwrap();
    let output = world.configure(&target);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mirror_unsafe_file"));
    fs::remove_file(&target).unwrap();
    fs::set_permissions(root.join(".git"), fs::Permissions::from_mode(0o777)).unwrap();
    let output = world.configure(&target);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mirror_unsafe_directory"));
    fs::set_permissions(root.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
    if rustix::process::geteuid().as_raw() == 0 {
        fs::set_permissions(root.join(".git"), fs::Permissions::from_mode(0o1777)).unwrap();
        let output = world.configure(&target);
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr).contains("mirror_unsafe_checkout"));
        fs::set_permissions(root.join(".git"), fs::Permissions::from_mode(0o700)).unwrap();
    }
    fs::set_permissions(&root, fs::Permissions::from_mode(0o770)).unwrap();
    let output = world.configure(&target);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mirror_unsafe_directory"));
    let output = world.configure(&world.fixture.store_root().join("profile.md"));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mirror_inside_store"));
    let output = world.configure(Path::new("relative/profile.md"));
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mirror_path_invalid"));
}

#[test]
fn pending_mirror_repair_rechecks_the_admission_and_destination() {
    let world = World::new();
    let root = world.fixture.path("backup");
    fs::create_dir(&root).unwrap();
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    let mirror = root.join("profile.md");
    assert!(world.configure(&mirror).status.success());
    let profile = world.capture("first", "PROFILE.md", FIRST);
    world.admit(&[&profile]).unwrap();

    // Model interruption after the pending record was persisted but before its
    // mirror update settled; the signed Admission is real, with a fixture key.
    let state_path = world
        .fixture
        .store_root()
        .join("operator-profile-mirror.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    state["pending"] = json!(true);
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    fs::remove_file(&mirror).unwrap();
    assert_eq!(world.mirror_status()["state"], "drift");
    let output = world.configure(&mirror);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&mirror).unwrap(), FIRST.as_bytes());
    assert_eq!(world.mirror_status()["state"], "current");

    state["generation"] = json!(Digest::of(b"not an Admission").to_string());
    state["pending"] = json!(true);
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    assert_eq!(world.mirror_status()["state"], "unsettled");
    assert!(!world.configure(&mirror).status.success());
    assert_eq!(fs::read(&mirror).unwrap(), FIRST.as_bytes());
}

#[test]
fn checkout_includes_and_configured_branch_remotes_are_refused() {
    let world = World::new();
    let root = private_checkout(&world);
    let mirror = root.join("profile.md");
    git(
        &root,
        &["config", "--local", "include.path", "/not-read/git-config"],
    );
    let output = world.configure(&mirror);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mirror_checkout_has_remote"));
    git(&root, &["config", "--local", "--unset", "include.path"]);
    git(
        &root,
        &[
            "config",
            "--local",
            "branch.main.remote",
            "https://example.org/public.git",
        ],
    );
    let output = world.configure(&mirror);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mirror_checkout_has_remote"));
    assert!(!mirror.exists());
}

#[test]
fn a_public_remote_in_global_git_configuration_is_refused() {
    let world = World::new();
    let root = private_checkout(&world);
    let mirror = root.join("profile.md");
    let global = world.fixture.path("global-git-config");
    write_file(
        &global,
        "[remote \"origin\"]\n\turl = https://example.org/public.git\n",
    );
    fs::set_permissions(&global, fs::Permissions::from_mode(0o600)).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args(["generation", "mirror", "--output"])
        .arg(&mirror)
        .env("LOUISELM_SKILLS_STORE", world.fixture.store_root())
        .env("GIT_CONFIG_GLOBAL", &global)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mirror_checkout_has_remote"));
    assert!(!mirror.exists());
}

#[test]
fn linked_admission_recovery_does_not_overwrite_an_unchanged_profile_edit() {
    let world = World::new();
    let root = private_checkout(&world);
    let mirror = root.join("profile.md");
    assert!(world.configure(&mirror).status.success());
    let profile = world.capture("first", "PROFILE.md", FIRST);
    let reviewed = Dossier::build(
        &world.fixture.store(),
        &Policy::embedded(),
        &DossierRequest::new(&profile).with_review_depth(ReviewDepth::Read),
    )
    .unwrap()
    .portable_digest();
    let member = AdmissionMember {
        package: profile,
        depth: ReviewDepth::Read,
        agents: vec!["agent".into()],
    };
    let signer = SshKeygenSigner::new(world.primary.private_key_path());
    let operation = "8d345c45-f2d1-4d0b-90d1-bfe577870f33";
    let first = admission::admit_linked(
        &world.fixture.store(),
        &Policy::embedded(),
        &linked_request(&member, &signer),
        operation,
        Some(&reviewed),
    )
    .unwrap();
    assert_eq!(fs::read(&mirror).unwrap(), FIRST.as_bytes());
    write_file(&mirror, "an edit awaiting Admission");
    let recovered = admission::admit_linked(
        &world.fixture.store(),
        &Policy::embedded(),
        &linked_request(&member, &RefuseSigning),
        operation,
        None,
    )
    .unwrap();
    assert_eq!(recovered.generation, first.generation);
    assert_eq!(fs::read(&mirror).unwrap(), b"an edit awaiting Admission");
    assert_eq!(world.mirror_status()["state"], "drift");
}

#[test]
fn configuring_after_activation_copies_the_verified_current_profile() {
    let world = World::new();
    let profile = world.capture("first", "PROFILE.md", FIRST);
    let record = world.admit(&[&profile]).unwrap();
    let witness = GitWitness::new(
        &world.fixture.witness_remote(),
        "generations",
        &world.fixture.path("witness-work"),
    );
    admission::witness(&world.fixture.store(), &record.digest(), &witness, 3).unwrap();
    admission::activate(&world.fixture.store(), &record.digest(), 4).unwrap();
    let root = private_checkout(&world);
    let mirror = root.join("profile.md");
    let output = world.configure(&mirror);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read(&mirror).unwrap(), FIRST.as_bytes());
    assert_eq!(world.mirror_status()["generation"], record.generation);
    assert_eq!(world.mirror_status()["state"], "current");
}

#[test]
fn corrupted_admitted_package_is_not_used_to_repair_or_report_a_mirror() {
    let world = World::new();
    let root = private_checkout(&world);
    let mirror = root.join("profile.md");
    assert!(world.configure(&mirror).status.success());
    let profile = world.capture("first", "PROFILE.md", FIRST);
    world.admit(&[&profile]).unwrap();
    let file = world
        .fixture
        .store()
        .open_package(&profile, &Policy::embedded())
        .unwrap()
        .file_path("PROFILE.md");
    fs::set_permissions(&file, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(&file, "altered unapproved bytes").unwrap();
    let output = world.run(&["generation", "status", "--robot-json"], None);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("mirror_package_invalid"));
    assert!(!String::from_utf8_lossy(&output.stderr).contains("altered unapproved bytes"));
    assert!(!world.configure(&mirror).status.success());
    assert_eq!(fs::read(&mirror).unwrap(), FIRST.as_bytes());
}

#[test]
fn status_refuses_a_mirror_record_that_hides_an_admitted_profile() {
    let world = World::new();
    let root = private_checkout(&world);
    let mirror = root.join("profile.md");
    assert!(world.configure(&mirror).status.success());
    let profile = world.capture("first", "PROFILE.md", FIRST);
    world.admit(&[&profile]).unwrap();
    let state_path = world
        .fixture
        .store_root()
        .join("operator-profile-mirror.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
    state["profile"] = Value::Null;
    fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
    let output = world.run(&["generation", "status", "--robot-json"], None);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("mirror_profile_mismatch"));
    assert_eq!(fs::read(&mirror).unwrap(), FIRST.as_bytes());
}
