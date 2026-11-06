//! Ordinary Agent slots receive only verified, admitted Operator profile bytes.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Fixtures assert public CLI and filesystem behavior."
)]

mod support;

use std::{fs, io::Read, os::unix::fs::PermissionsExt, process::Command};

use louiselm_skills::{
    Digest, Policy, ReviewDepth, SshKeygenSigner, TrustStore,
    admission::{self, AdmissionMember, AdmissionRequest},
    instruction_view,
    registry::Registry,
    sshsig::SkPolicy,
    witness::GitWitness,
};
use serde_json::{Value, json};
use support::{Fixture, SshKey, write_file};

const PROFILE: &str = "---\ntype: louiselm/operator-profile\n---\n\nPrivate operator defaults.\n";
const AGENTS: &[&str] = &["claude", "codex", "opencode", "undeclared"];

struct World {
    fixture: Fixture,
    profile: Option<Digest>,
    skill: Digest,
}

impl World {
    fn new(with_profile: bool) -> Self {
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
        let candidate = fixture.candidate("skill");
        write_file(
            &candidate.join("SKILL.md"),
            "---\nname: example\ndescription: Example.\n---\nSkill.\n",
        );
        let skill = fixture.capture(&candidate).unwrap().0.digest;
        let profile = with_profile.then(|| {
            let candidate = fixture.candidate("profile");
            write_file(&candidate.join("PROFILE.md"), PROFILE);
            fixture.capture(&candidate).unwrap().0.digest
        });
        let agents: Vec<String> = AGENTS.iter().map(|agent| (*agent).to_owned()).collect();
        let members = std::iter::once(&skill)
            .chain(profile.iter())
            .map(|package| AdmissionMember {
                package: package.clone(),
                depth: ReviewDepth::Read,
                agents: agents.clone(),
            })
            .collect();
        let record = admission::admit(
            &fixture.store(),
            &Policy::embedded(),
            &AdmissionRequest {
                members,
                signer: &SshKeygenSigner::new(primary.private_key_path()),
                admitted_at_ms: 2,
                configured_agents: Some(agents),
            },
        )
        .unwrap();
        let witness = GitWitness::new(
            &fixture.witness_remote(),
            "generations",
            &fixture.path("witness-work"),
        );
        admission::witness(&fixture.store(), &record.digest(), &witness, 3).unwrap();
        admission::activate(&fixture.store(), &record.digest(), 4).unwrap();
        let world = Self {
            fixture,
            profile,
            skill,
        };
        world.registry(AGENTS);
        world
    }

    fn slot(&self, agent: &str) -> std::path::PathBuf {
        self.fixture.path(&format!("home/{agent}/AGENTS.md"))
    }

    fn registry(&self, agents: &[&str]) {
        let entries: Vec<Value> = agents.iter().map(|agent| {
            let mut entry = json!({"id":agent, "provider":"test", "runtime_id":"test", "arguments":[], "environment":{}});
            if *agent != "undeclared" {
                entry["always_on_path"] = json!(self.slot(agent));
            }
            entry
        }).collect();
        write_file(
            &self.fixture.path("registry/agents.json"),
            &json!({
                "schema":"louiselm.launch.registry/1", "entries":entries,
            })
            .to_string(),
        );
    }

    fn run(&self, verb: &str, action: &str) -> std::process::Output {
        Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
            .args([verb, action, "--robot-json", "--registry"])
            .arg(self.fixture.path("registry"))
            .env("LOUISELM_SKILLS_STORE", self.fixture.store_root())
            .output()
            .unwrap()
    }

    fn json(&self, verb: &str, action: &str) -> Value {
        let output = self.run(verb, action);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!String::from_utf8_lossy(&output.stdout).contains("Private operator defaults"));
        serde_json::from_slice(&output.stdout).unwrap()
    }
}

#[test]
fn materialize_delivers_identical_private_bytes_and_status_checks_actual_slots() {
    let world = World::new(true);
    let before = world.json("generation", "status");
    assert_eq!(before["operator_profile"]["undelivered"], json!(AGENTS));

    let output = world.json("view", "materialize");
    assert_eq!(
        output["operator_profile"]["profile"],
        world.profile.as_ref().unwrap().to_string()
    );
    assert_eq!(
        output["operator_profile"]["undelivered"],
        json!(["undeclared"])
    );
    for agent in &AGENTS[..3] {
        let slot = world.slot(agent);
        assert_eq!(fs::read(&slot).unwrap(), PROFILE.as_bytes());
        assert_eq!(
            fs::metadata(&slot).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let skills = std::path::Path::new(output["views"][*agent]["skills_root"].as_str().unwrap());
        assert!(
            skills
                .join(world.skill.directory_name())
                .join("SKILL.md")
                .is_file()
        );
        assert!(
            !skills
                .join(world.profile.as_ref().unwrap().directory_name())
                .exists()
        );
    }
    assert_eq!(
        world.json("generation", "status")["operator_profile"]["undelivered"],
        json!(["undeclared"])
    );

    fs::write(world.slot("codex"), "hand edit").unwrap();
    let mut old_inode = fs::File::open(world.slot("codex")).unwrap();
    assert_eq!(
        world.json("generation", "status")["operator_profile"]["undelivered"],
        json!(["codex", "undeclared"])
    );
    world.json("view", "materialize");
    assert_eq!(fs::read(world.slot("codex")).unwrap(), PROFILE.as_bytes());
    let mut previous = String::new();
    old_inode.read_to_string(&mut previous).unwrap();
    assert_eq!(
        previous, "hand edit",
        "replacement must be atomic, not truncate an open file"
    );
}

#[test]
fn preparing_launch_views_never_writes_host_slots_or_exposes_profiles_as_skills() {
    let world = World::new(true);
    let registry = Registry::open(&world.fixture.path("registry")).unwrap();
    let views =
        instruction_view::materialize(&world.fixture.store(), &Policy::embedded(), &registry)
            .unwrap();
    for agent in AGENTS {
        assert!(!world.slot(agent).exists());
        assert!(
            !views[*agent]
                .skills_root()
                .join(world.profile.as_ref().unwrap().directory_name())
                .exists()
        );
    }
}

#[test]
fn absent_profile_leaves_existing_instructions_untouched() {
    let world = World::new(false);
    write_file(&world.slot("claude"), "existing personal instructions");
    let output = world.json("view", "materialize");
    assert!(output["operator_profile"]["profile"].is_null());
    assert_eq!(output["operator_profile"]["undelivered"], json!(AGENTS));
    assert_eq!(
        fs::read_to_string(world.slot("claude")).unwrap(),
        "existing personal instructions"
    );
    assert!(!world.slot("codex").exists());
}

#[test]
fn newly_registered_agents_need_a_new_admission_even_with_a_declared_slot() {
    let world = World::new(true);
    world.registry(&["claude", "codex", "opencode", "undeclared", "new-agent"]);
    let output = world.json("view", "materialize");
    assert_eq!(
        output["operator_profile"]["undelivered"],
        json!(["new-agent", "undeclared"])
    );
    assert!(!world.slot("new-agent").exists());
}

#[test]
fn tampered_supply_is_refused_before_any_instruction_is_replaced() {
    let world = World::new(true);
    write_file(&world.slot("claude"), "existing instructions");
    let profile = world
        .fixture
        .store()
        .open_package(world.profile.as_ref().unwrap(), &Policy::embedded())
        .unwrap();
    let path = profile.file_path("PROFILE.md");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(path, "tampered profile").unwrap();
    let output = world.run("view", "materialize");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("file_type_size_or_mode"));
    assert_eq!(
        fs::read_to_string(world.slot("claude")).unwrap(),
        "existing instructions"
    );
    assert!(!world.slot("codex").exists());
}

#[test]
fn delivery_refuses_symlink_slots_without_touching_their_target() {
    let world = World::new(true);
    let target = world.fixture.path("other-instructions");
    write_file(&target, "other instructions");
    fs::create_dir_all(world.slot("claude").parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&target, world.slot("claude")).unwrap();
    let output = world.run("view", "materialize");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("profile_destination_not_regular"));
    assert_eq!(fs::read_to_string(target).unwrap(), "other instructions");
    assert_eq!(
        world.json("generation", "status")["operator_profile"]["undelivered"],
        json!(AGENTS)
    );
}

#[test]
fn delivery_refuses_symlink_parents_and_status_reports_them_undelivered() {
    let world = World::new(true);
    let target = world.fixture.path("outside");
    fs::create_dir(&target).unwrap();
    fs::create_dir(world.fixture.path("home")).unwrap();
    std::os::unix::fs::symlink(&target, world.fixture.path("home/claude")).unwrap();
    let output = world.run("view", "materialize");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("managed_supply: I/O"));
    assert!(!target.join("AGENTS.md").exists());
    assert_eq!(
        world.json("generation", "status")["operator_profile"]["undelivered"],
        json!(AGENTS)
    );
}

#[test]
fn quarantined_profile_is_refused_before_any_instruction_is_replaced() {
    let world = World::new(true);
    write_file(&world.slot("claude"), "existing instructions");
    louiselm_skills::quarantine::exclude(
        &world.fixture.store(),
        &[world.profile.as_ref().unwrap().to_string()],
        "test quarantine",
        5,
    )
    .unwrap();
    let output = world.run("view", "materialize");
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("generation_quarantined"));
    assert_eq!(
        fs::read_to_string(world.slot("claude")).unwrap(),
        "existing instructions"
    );
    assert!(!world.slot("codex").exists());
}
