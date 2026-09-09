//! Instruction views expose only the current Generation's Agent-scoped bytes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures abort on setup failure and assert failures directly."
)]

mod support;

use std::{fs, os::unix::fs::PermissionsExt, path::Path};

use louiselm_skills::{
    Digest, GenerationRecord, GenerationState, Policy,
    admission::{self, AdmissionMember, AdmissionRequest},
    dossier::ReviewDepth,
    instruction_view::{self, InstructionView},
    quarantine,
    registry::Registry,
    signer::SshKeygenSigner,
    sshsig::SkPolicy,
    trust::TrustStore,
    witness::GitWitness,
};
use support::{Fixture, SshKey, write_file};

struct Supply {
    fixture: Fixture,
    primary: SshKey,
    registry: Registry,
}

impl Supply {
    fn new() -> Self {
        let fixture = Fixture::new();
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
        write_file(
            &fixture.path("registry/agents.json"),
            r#"{"schema":"louiselm.launch.registry/1","entries":[
                {"id":"claude","provider":"anthropic","runtime_id":"test","arguments":[],"environment":{}},
                {"id":"codex","provider":"openai","runtime_id":"test","arguments":[],"environment":{}},
                {"id":"unused","provider":"openai","runtime_id":"test","arguments":[],"environment":{}}
            ]}"#,
        );
        let registry = Registry::open(&fixture.path("registry")).unwrap();
        Self {
            fixture,
            primary,
            registry,
        }
    }

    fn member(&self, name: &str, agents: &[&str]) -> AdmissionMember {
        let candidate = self.fixture.candidate(name);
        write_file(
            &candidate.join("SKILL.md"),
            &format!("---\nname: {name}\ndescription: Test skill.\n---\n\nBody.\n"),
        );
        write_file(&candidate.join("scripts/run"), "#!/bin/sh\nexit 0\n");
        fs::set_permissions(
            candidate.join("scripts/run"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        AdmissionMember {
            package: self.fixture.capture(&candidate).unwrap().0.digest,
            depth: ReviewDepth::Read,
            agents: agents.iter().map(|agent| (*agent).to_owned()).collect(),
        }
    }

    fn admit(&self, members: Vec<AdmissionMember>) -> GenerationRecord {
        admission::admit(
            &self.fixture.store(),
            &Policy::embedded(),
            &AdmissionRequest {
                members,
                signer: &SshKeygenSigner::new(self.primary.private_key_path()),
                admitted_at_ms: 2,
            },
        )
        .unwrap()
    }

    fn activate(&self, record: &GenerationRecord) {
        let witness = GitWitness::new(
            &self.fixture.witness_remote(),
            "generations",
            &self.fixture.path("witness-work"),
        );
        admission::witness(&self.fixture.store(), &record.digest(), &witness, 3).unwrap();
        admission::activate(&self.fixture.store(), &record.digest(), 4).unwrap();
    }

    fn views(
        &self,
    ) -> Result<std::collections::BTreeMap<String, InstructionView>, instruction_view::ViewError>
    {
        instruction_view::materialize(&self.fixture.store(), &Policy::embedded(), &self.registry)
    }
}

fn entries(path: &Path) -> Vec<String> {
    let mut names = fs::read_dir(path)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    names.sort();
    names
}

fn rewrite_record(supply: &Supply, record: &GenerationRecord) {
    fs::write(
        admission::record_path(&supply.fixture.store(), &record.digest()),
        serde_json::to_vec(record).unwrap(),
    )
    .unwrap();
}

#[test]
fn views_route_shared_and_system_packages_by_signed_agent_membership() {
    let supply = Supply::new();
    let shared = supply.member("shared", &["claude", "codex"]);
    let system = supply.member("system", &["codex"]);
    let foreign = supply.member("foreign", &["unregistered"]);
    let unadmitted = supply.member("candidate-only", &["claude"]);
    let record = supply.admit(vec![shared.clone(), system.clone(), foreign]);
    supply.activate(&record);

    let views = supply.views().unwrap();
    assert_eq!(
        views.keys().cloned().collect::<Vec<_>>(),
        ["claude", "codex", "unused"]
    );
    assert_eq!(
        entries(views["claude"].skills_root()),
        [shared.package.directory_name()]
    );
    let mut expected = vec![
        shared.package.directory_name(),
        system.package.directory_name(),
    ];
    expected.sort();
    assert_eq!(entries(views["codex"].skills_root()), expected);
    assert!(entries(views["unused"].skills_root()).is_empty());
    assert_eq!(
        views["unused"].digest(),
        instruction_view::empty(&supply.fixture.store())
            .unwrap()
            .digest()
    );
    assert!(
        !views["claude"]
            .skills_root()
            .join(unadmitted.package.directory_name())
            .exists()
    );

    let skill_root = views["codex"]
        .skills_root()
        .join(system.package.directory_name());
    assert_eq!(
        fs::read(skill_root.join("scripts/run")).unwrap(),
        b"#!/bin/sh\nexit 0\n"
    );
    assert_eq!(
        fs::metadata(skill_root.join("scripts/run"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o555
    );
    assert_eq!(
        fs::metadata(skill_root.join("SKILL.md"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o444
    );
    assert_eq!(
        fs::metadata(&skill_root).unwrap().permissions().mode() & 0o777,
        0o555
    );
    assert_ne!(views["claude"].digest(), views["codex"].digest());
    let again = supply.views().unwrap();
    assert_eq!(again["codex"].digest(), views["codex"].digest());
}

#[test]
fn skills_off_is_one_real_empty_artifact_without_any_generation_or_trust() {
    let first = Fixture::new();
    let second = Fixture::new();
    let view = instruction_view::empty(&first.store()).unwrap();
    assert!(view.skills_root().is_dir());
    assert!(entries(view.skills_root()).is_empty());
    assert_eq!(
        view.digest(),
        instruction_view::empty(&second.store()).unwrap().digest()
    );
    assert_eq!(
        view.digest(),
        &Digest::of(&fs::read(view.root().join("view.json")).unwrap())
    );
}

#[test]
fn noncurrent_unwitnessed_invalid_and_unknown_generations_publish_nothing() {
    let supply = Supply::new();
    assert!(supply.views().is_err());
    let record = supply.admit(vec![supply.member("skill", &["claude"])]);
    for state in [
        GenerationState::PendingWitness,
        GenerationState::Superseded,
        GenerationState::Quarantined,
        GenerationState::Invalid,
        GenerationState::Current,
    ] {
        let mut edited = record.clone();
        edited.state = state;
        rewrite_record(&supply, &edited);
        assert!(
            supply.views().is_err(),
            "state {state:?} without a witness must refuse"
        );
        assert!(!supply.fixture.path("store/views").exists());
    }
    let mut raw = serde_json::to_value(&record).unwrap();
    raw["state"] = "unknown".into();
    fs::write(
        admission::record_path(&supply.fixture.store(), &record.digest()),
        serde_json::to_vec(&raw).unwrap(),
    )
    .unwrap();
    assert!(supply.views().is_err());
}

#[test]
fn current_records_still_require_valid_signatures_policy_and_no_quarantine() {
    let supply = Supply::new();
    let member = supply.member("skill", &["claude"]);
    let record = supply.admit(vec![member.clone()]);
    supply.activate(&record);
    let current = admission::current(&supply.fixture.store())
        .unwrap()
        .unwrap();
    let mut changed = current.clone();
    changed.signature = "invalid signature".into();
    rewrite_record(&supply, &changed);
    assert!(supply.views().is_err());
    rewrite_record(&supply, &current);
    let wrong_policy = support::policy_with(&[(
        "\"version\": \"2026-09-03.1\"",
        "\"version\": \"different\"",
    )]);
    assert!(
        instruction_view::materialize(&supply.fixture.store(), &wrong_policy, &supply.registry)
            .is_err()
    );
    quarantine::exclude(
        &supply.fixture.store(),
        &[member.package.to_string()],
        "test",
        5,
    )
    .unwrap();
    assert!(
        supply.views().is_err(),
        "quarantine must refuse the whole view, never drop one member"
    );
    assert!(instruction_view::empty(&supply.fixture.store()).is_ok());
}

#[test]
fn foreign_content_at_a_destination_is_never_replaced_before_or_after_publication() {
    let supply = Supply::new();
    let record = supply.admit(vec![supply.member("skill", &["claude"])]);
    supply.activate(&record);
    let view = supply.views().unwrap().remove("claude").unwrap();
    let original = view.root().with_file_name(".original-view");
    fs::rename(view.root(), &original).unwrap();
    write_file(
        &view.skills_root().join("foreign/SKILL.md"),
        "foreign before publication",
    );
    assert!(supply.views().is_err());
    assert_eq!(
        fs::read_to_string(view.skills_root().join("foreign/SKILL.md")).unwrap(),
        "foreign before publication"
    );
    fs::rename(view.root(), view.root().with_file_name(".injected-view")).unwrap();
    fs::rename(original, view.root()).unwrap();
    fs::set_permissions(view.skills_root(), fs::Permissions::from_mode(0o755)).unwrap();
    write_file(
        &view.skills_root().join("foreign/SKILL.md"),
        "foreign after publication",
    );
    fs::set_permissions(view.skills_root(), fs::Permissions::from_mode(0o555)).unwrap();
    assert!(supply.views().is_err());
    assert_eq!(
        fs::read_to_string(view.skills_root().join("foreign/SKILL.md")).unwrap(),
        "foreign after publication"
    );
}

#[test]
fn published_content_changes_and_symlinks_are_refused() {
    let supply = Supply::new();
    let member = supply.member("skill", &["claude"]);
    let record = supply.admit(vec![member.clone()]);
    supply.activate(&record);
    let view = supply.views().unwrap().remove("claude").unwrap();
    let package_root = view.skills_root().join(member.package.directory_name());
    let path = package_root.join("SKILL.md");
    let original = fs::read(&path).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&path, "changed").unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o444)).unwrap();
    assert!(supply.views().is_err());
    fs::set_permissions(&package_root, fs::Permissions::from_mode(0o755)).unwrap();
    fs::remove_file(&path).unwrap();
    let target = supply.fixture.path("same-bytes");
    fs::write(&target, original).unwrap();
    std::os::unix::fs::symlink(target, &path).unwrap();
    fs::set_permissions(package_root, fs::Permissions::from_mode(0o555)).unwrap();
    assert!(
        supply.views().is_err(),
        "same bytes via a mutable alias still refuse"
    );
}

#[test]
fn tampered_store_members_cannot_supply_a_partial_view() {
    let supply = Supply::new();
    let member = supply.member("skill", &["claude"]);
    let record = supply.admit(vec![member.clone()]);
    supply.activate(&record);
    let package = supply
        .fixture
        .store()
        .open_package(&member.package, &Policy::embedded())
        .unwrap();
    let path = package.file_path("SKILL.md");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(path, "tampered").unwrap();
    assert!(supply.views().is_err());
}

#[test]
fn the_same_generation_and_agent_have_the_same_view_digest_on_another_host() {
    let supply = Supply::new();
    let member = supply.member("skill", &["claude"]);
    let record = supply.admit(vec![member.clone()]);
    supply.activate(&record);
    let first = supply.views().unwrap();
    let other = Fixture::new();
    let other_store = other.store();
    // Copy the exact signed authority into an isolated host fixture; no new signature.
    fs::create_dir_all(other.path("store/trust")).unwrap();
    fs::copy(
        supply.fixture.path("store/trust/roles.json"),
        other.path("store/trust/roles.json"),
    )
    .unwrap();
    fs::create_dir_all(other.path("store/generations")).unwrap();
    fs::copy(
        admission::record_path(&supply.fixture.store(), &record.digest()),
        admission::record_path(&other_store, &record.digest()),
    )
    .unwrap();
    other.capture(&supply.fixture.path("skill")).unwrap();
    let second =
        instruction_view::materialize(&other_store, &Policy::embedded(), &supply.registry).unwrap();
    assert_ne!(first["claude"].root(), second["claude"].root());
    assert_eq!(first["claude"].digest(), second["claude"].digest());
}

#[test]
fn the_cli_materializes_current_views_and_the_empty_mask() {
    let supply = Supply::new();
    let member = supply.member("skill", &["claude"]);
    let record = supply.admit(vec![member.clone()]);
    supply.activate(&record);
    let invoke = |arguments: &[&str]| {
        std::process::Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
            .args(arguments)
            .arg("--store")
            .arg(supply.fixture.store_root())
            .arg("--robot-json")
            .output()
            .unwrap()
    };
    let result = invoke(&[
        "view",
        "materialize",
        "--registry",
        supply.registry.root().to_str().unwrap(),
    ]);
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let views: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    let skills = Path::new(views["claude"]["skills_root"].as_str().unwrap());
    assert_eq!(entries(skills), [member.package.directory_name()]);
    assert!(Digest::parse(views["claude"]["digest"].as_str().unwrap()).is_ok());
    assert!(
        !invoke(&["view", "materialize"]).status.success(),
        "materialization requires an explicit registry"
    );
    let result = invoke(&["view", "empty"]);
    assert!(result.status.success());
    let view: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
    assert!(entries(Path::new(view["skills_root"].as_str().unwrap())).is_empty());
    fs::set_permissions(skills, fs::Permissions::from_mode(0o755)).unwrap();
    write_file(&skills.join("\u{1b}[31m"), "untrusted addition");
    fs::set_permissions(skills, fs::Permissions::from_mode(0o555)).unwrap();
    let result = invoke(&[
        "view",
        "materialize",
        "--registry",
        supply.registry.root().to_str().unwrap(),
    ]);
    assert!(!result.status.success());
    assert!(result.stdout.is_empty());
    assert!(
        !result.stderr.contains(&0x1b),
        "untrusted filenames must not control the terminal"
    );
}

#[test]
fn store_directory_and_manifest_aliases_cannot_supply_a_view() {
    for alias_manifest in [false, true] {
        let supply = Supply::new();
        let member = supply.member("skill", &["claude"]);
        let record = supply.admit(vec![member.clone()]);
        supply.activate(&record);
        let package = supply
            .fixture
            .store()
            .open_package(&member.package, &Policy::embedded())
            .unwrap();
        let source = if alias_manifest {
            package.root.join("manifest.json")
        } else {
            package.root.clone()
        };
        let target = source.with_file_name("moved-source");
        fs::rename(&source, &target).unwrap();
        std::os::unix::fs::symlink(&target, &source).unwrap();
        assert!(
            supply.views().is_err(),
            "a store alias must refuse, even with identical bytes"
        );
        assert!(!supply.fixture.path("store/views").exists());
    }
}

#[test]
fn current_supply_is_rechecked_even_when_a_view_already_exists() {
    let supply = Supply::new();
    let record = supply.admit(vec![supply.member("skill", &["claude"])]);
    supply.activate(&record);
    let view = supply.views().unwrap().remove("claude").unwrap();
    let mut changed = admission::current(&supply.fixture.store())
        .unwrap()
        .unwrap();
    changed.state = GenerationState::Superseded;
    rewrite_record(&supply, &changed);
    assert!(
        supply.views().is_err(),
        "existing view must not provide a stale fallback"
    );
    assert!(
        view.skills_root().is_dir(),
        "refusal does not destroy an immutable artifact"
    );
}

#[test]
fn empty_publication_races_converge_without_overwriting_or_leaving_staging() {
    let fixture = Fixture::new();
    let store = fixture.store();
    let barrier = std::sync::Barrier::new(4);
    let views = std::thread::scope(|scope| {
        let handles = (0..4)
            .map(|_| {
                scope.spawn(|| {
                    barrier.wait();
                    instruction_view::empty(&store).unwrap()
                })
            })
            .collect::<Vec<_>>();
        handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert!(views.iter().all(|view| view.digest() == views[0].digest()));
    assert_eq!(
        entries(&fixture.path("store/views")),
        [views[0].digest().directory_name()]
    );
}
