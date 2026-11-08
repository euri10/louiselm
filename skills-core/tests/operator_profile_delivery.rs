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

    fn native_paths(&self, agent: &str, paths: &[std::path::PathBuf]) {
        let path = self.fixture.path("registry/agents.json");
        let mut registry: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        let entry = registry["entries"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .find(|entry| entry["id"] == agent)
            .unwrap();
        entry["native_instruction_paths"] = json!(paths);
        write_file(&path, &registry.to_string());
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
        assert_eq!(
            views[*agent].operator_profile().unwrap().bytes(),
            PROFILE.as_bytes()
        );
        assert_eq!(
            views[*agent].operator_profile().unwrap().package_digest(),
            world.profile.as_ref().unwrap()
        );
        assert!(!format!("{:?}", views[*agent]).contains("Private operator defaults"));
        assert!(
            !views[*agent]
                .skills_root()
                .join(world.profile.as_ref().unwrap().directory_name())
                .exists()
        );
    }
}

#[test]
fn contained_resolution_binds_admitted_bytes_and_off_never_resolves_a_profile() {
    use louiselm_skills::session_manifest::{SessionInputs, SkillsMode};
    let world = World::new(true);
    let runtime = world.fixture.path("runtime");
    write_file(&runtime.join("agent"), "offline runtime");
    write_file(
        &world.fixture.path("registry/runtimes.json"),
        &json!({
            "schema":"louiselm.launch.registry/1", "entries":[{
                "id":"test", "root":runtime, "executable":"agent",
                "executable_sha256":Digest::of(b"offline runtime").hex(),
                "adapters":[], "version":"1", "origin":"offline fixture",
            }],
        })
        .to_string(),
    );
    let registry = || Registry::open(&world.fixture.path("registry")).unwrap();
    let off = SessionInputs::resolve(
        &world.fixture.store(),
        &Policy::embedded(),
        &registry(),
        "codex",
        SkillsMode::Off,
    )
    .unwrap();
    assert!(off.operator_profile.is_none());
    assert!(off.skills.unwrap().generation_digest().is_none());
    assert!(matches!(
        SessionInputs::resolve(
            &world.fixture.store(),
            &Policy::embedded(),
            &registry(),
            "codex",
            SkillsMode::Enabled
        ),
        Err(louiselm_skills::session_manifest::InputResolutionError::ProfileSlotUndeclared)
    ));
    let path = world.fixture.path("registry/agents.json");
    let mut agents: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
    agents["entries"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|agent| agent["id"] == "codex")
        .unwrap()["contained_always_on_path"] = "AGENTS.md".into();
    write_file(&path, &agents.to_string());
    let inputs = SessionInputs::resolve(
        &world.fixture.store(),
        &Policy::embedded(),
        &registry(),
        "codex",
        SkillsMode::Enabled,
    )
    .unwrap();
    let profile = inputs.operator_profile.unwrap();
    assert_eq!(profile.bytes(), PROFILE.as_bytes());
    assert_eq!(profile.package_digest(), world.profile.as_ref().unwrap());
    assert!(!world.slot("codex").exists());
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

#[test]
fn native_drift_reports_edited_slot_without_contents_and_omits_rendered_slots() {
    let world = World::new(true);
    world.json("view", "materialize");
    assert_eq!(
        world.json("generation", "status")["native_instructions"]["drift"],
        json!([])
    );
    fs::write(world.slot("codex"), "private hand edit").unwrap();
    let output = world.json("generation", "status");
    let drift = output["native_instructions"]["drift"].as_array().unwrap();
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0]["agent"], "codex");
    assert_eq!(drift[0]["path"], json!(world.slot("codex")));
    assert_eq!(
        drift[0]["sha256"],
        Digest::of(b"private hand edit").to_string()
    );
    assert!(!output.to_string().contains("private hand edit"));
    assert_eq!(fs::read(world.slot("codex")).unwrap(), b"private hand edit");
    assert_eq!(output["native_instructions"]["undeclared"], json!(AGENTS));
}

#[test]
fn native_drift_scans_only_declared_trees_and_deduplicates_overlapping_roots() {
    let world = World::new(true);
    world.json("view", "materialize");
    let memory = world.fixture.path("memory/topics/private.md");
    write_file(&memory, "private memory fact");
    write_file(&world.fixture.path("memory/index.md"), PROFILE);
    write_file(
        &world.fixture.path("not-declared/transcript.jsonl"),
        "private transcript",
    );
    world.native_paths(
        "claude",
        &[
            world.fixture.path("memory"),
            memory.clone(),
            world.slot("claude"),
            world.fixture.path("missing-memory"),
        ],
    );
    let output = world.json("generation", "status");
    let drift = output["native_instructions"]["drift"].as_array().unwrap();
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0]["path"], json!(memory));
    assert_eq!(
        drift[0]["sha256"],
        Digest::of(b"private memory fact").to_string()
    );
    assert_eq!(
        output["native_instructions"]["undeclared"],
        json!(["codex", "opencode", "undeclared"])
    );
    assert!(!output.to_string().contains("private memory fact"));
    assert!(!output.to_string().contains("private transcript"));
}

#[test]
fn native_drift_without_a_profile_reports_all_declared_content() {
    let world = World::new(false);
    write_file(&world.slot("claude"), PROFILE);
    let output = world.json("generation", "status");
    assert_eq!(
        output["native_instructions"]["drift"][0]["sha256"],
        Digest::of(PROFILE.as_bytes()).to_string()
    );
    assert!(
        !world.slot("codex").exists(),
        "status must not create paths"
    );
}

#[test]
fn native_drift_does_not_follow_links_or_block_on_fifos_or_oversize_files() {
    let world = World::new(true);
    world.json("view", "materialize");
    let root = world.fixture.path("memory");
    fs::create_dir(&root).unwrap();
    let outside = world.fixture.path("outside/secret.md");
    write_file(&outside, "not authorized to read");
    std::os::unix::fs::symlink(&outside, root.join("linked.md")).unwrap();
    std::os::unix::fs::symlink(outside.parent().unwrap(), root.join("linked-directory")).unwrap();
    rustix::fs::mkfifoat(rustix::fs::CWD, root.join("pipe"), rustix::fs::Mode::RUSR).unwrap();
    fs::File::create(root.join("oversize.md"))
        .unwrap()
        .set_len(8 * 1024 * 1024 + 1)
        .unwrap();
    let alias = world.fixture.path("aliased");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    world.native_paths("claude", &[root.clone(), alias.join("linked.md")]);
    let output = world.json("generation", "status");
    let drift = output["native_instructions"]["drift"].as_array().unwrap();
    assert_eq!(drift.len(), 5);
    assert!(drift.iter().all(|entry| entry["sha256"].is_null()));
    assert!(drift.iter().any(|entry| entry["reason"] == "file_limit"));
    assert!(drift.iter().any(|entry| entry["reason"] == "not_regular"));
    assert!(
        !output
            .to_string()
            .contains(&Digest::of(b"not authorized to read").to_string())
    );
    assert!(!output.to_string().contains("not authorized to read"));
    assert_eq!(output["native_instructions"]["complete"], false);
}

#[test]
fn native_drift_does_not_trust_quarantined_or_out_of_scope_profile_bytes() {
    let world = World::new(true);
    world.json("view", "materialize");
    world.registry(&["claude", "codex", "opencode", "undeclared", "new-agent"]);
    write_file(&world.slot("new-agent"), PROFILE);
    let output = world.json("generation", "status");
    let drift = output["native_instructions"]["drift"].as_array().unwrap();
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0]["agent"], "new-agent");
    louiselm_skills::quarantine::exclude(
        &world.fixture.store(),
        &[world.profile.as_ref().unwrap().to_string()],
        "test quarantine",
        5,
    )
    .unwrap();
    let output = world.run("generation", "status");
    assert_eq!(
        output.status.code(),
        Some(louiselm_skills::cli::EXIT_NOT_ADMISSIBLE)
    );
    let output: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        output["native_instructions"]["drift"]
            .as_array()
            .unwrap()
            .len(),
        4
    );
}

#[test]
fn native_drift_before_admission_and_without_registry_are_explicit() {
    let fixture = Fixture::new();
    let file = fixture.path("memory.md");
    write_file(&file, "private native default");
    write_file(&fixture.path("registry/agents.json"), &json!({
        "schema":"louiselm.launch.registry/1", "entries":[{
            "id":"test", "provider":"test", "runtime_id":"test", "arguments":[], "environment":{},
            "native_instruction_paths":[file]
        }]
    }).to_string());
    let mut command = Command::new(env!("CARGO_BIN_EXE_louiselm-skills"));
    command
        .args(["generation", "status", "--robot-json"])
        .env("LOUISELM_SKILLS_STORE", fixture.store_root());
    let output = command.output().unwrap();
    assert_eq!(
        output.status.code(),
        Some(louiselm_skills::cli::EXIT_NOT_ADMISSIBLE)
    );
    let output: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert!(output.get("native_instructions").is_none());
    let output = command
        .arg("--registry")
        .arg(fixture.path("registry"))
        .output()
        .unwrap();
    assert_eq!(
        output.status.code(),
        Some(louiselm_skills::cli::EXIT_NOT_ADMISSIBLE)
    );
    let output: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        output["native_instructions"]["drift"][0]["sha256"],
        Digest::of(b"private native default").to_string()
    );
    assert_eq!(output["native_instructions"]["complete"], true);
    assert!(!output.to_string().contains("private native default"));
}

#[test]
fn native_drift_bounds_directory_depth_and_total_entries() {
    let world = World::new(false);
    let root = world.fixture.path("memory");
    let deep = root.join("deep/".repeat(34));
    write_file(&deep.join("unreachable.md"), "unreachable");
    world.native_paths("claude", std::slice::from_ref(&root));
    let output = world.json("generation", "status");
    assert_eq!(output["native_instructions"]["complete"], false);
    assert_eq!(
        output["native_instructions"]["drift"][0]["reason"],
        "depth_limit"
    );
    let many = world.fixture.path("many");
    fs::create_dir(&many).unwrap();
    for index in 0..4100 {
        fs::File::create(many.join(format!("{index}.md"))).unwrap();
    }
    world.native_paths("claude", &[many]);
    let output = world.json("generation", "status");
    assert_eq!(output["native_instructions"]["complete"], false);
    let drift = output["native_instructions"]["drift"].as_array().unwrap();
    assert!(drift.len() < 4100);
    assert!(drift.iter().any(|entry| entry["reason"] == "scan_limit"));
}

#[test]
fn native_drift_never_expands_a_file_only_slot_into_a_directory_scan() {
    let world = World::new(true);
    let slot = world.slot("claude");
    let child = slot.join("not-declared.md");
    write_file(&child, "outside declared file authority");
    let output = world.json("generation", "status");
    let drift = output["native_instructions"]["drift"].as_array().unwrap();
    assert_eq!(drift.len(), 1);
    assert_eq!(drift[0]["path"], json!(slot));
    assert_eq!(drift[0]["reason"], "not_regular");
    assert!(drift[0]["sha256"].is_null());
    assert!(
        !output
            .to_string()
            .contains(&Digest::of(b"outside declared file authority").to_string())
    );
    world.native_paths("claude", &[slot]);
    let output = world.json("generation", "status");
    assert_eq!(
        output["native_instructions"]["drift"][0]["path"],
        json!(child)
    );
    assert_eq!(
        output["native_instructions"]["drift"][0]["sha256"],
        Digest::of(b"outside declared file authority").to_string()
    );
}

#[test]
fn native_drift_bounds_total_bytes_and_escapes_human_paths() {
    let world = World::new(false);
    let root = world.fixture.path("memory");
    let odd = root.join("line\nname.md");
    write_file(&odd, "private fact never printed");
    world.native_paths("claude", std::slice::from_ref(&root));
    let output = Command::new(env!("CARGO_BIN_EXE_louiselm-skills"))
        .args(["generation", "status", "--registry"])
        .arg(world.fixture.path("registry"))
        .env("LOUISELM_SKILLS_STORE", world.fixture.store_root())
        .output()
        .unwrap();
    assert!(output.status.success());
    let output = String::from_utf8(output.stdout).unwrap();
    assert!(output.contains("line\\nname.md"));
    assert!(!output.contains("line\nname.md"));
    assert!(!output.contains("private fact never printed"));
    for index in 0..5 {
        fs::File::create(root.join(format!("{index}.md")))
            .unwrap()
            .set_len(8 * 1024 * 1024)
            .unwrap();
    }
    let output = world.json("generation", "status");
    assert_eq!(output["native_instructions"]["complete"], false);
    let drift = output["native_instructions"]["drift"].as_array().unwrap();
    assert!(
        drift
            .iter()
            .any(|entry| entry["reason"] == "scan_limit" && entry["sha256"].is_null())
    );
    assert!(
        drift
            .iter()
            .filter(|entry| entry["sha256"].is_string())
            .count()
            <= 4
    );
}
