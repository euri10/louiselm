//! Real maintained no-Provider entrypoint, only inside the disposable guest.
use super::*;
use crate::{
    conformance::{
        ReportResult,
        admission::Enforcement,
        installed::{certify, measure},
    },
    session_manifest::SkillsMode,
};

fn share(path: &Path, uid: u32) {
    if path.is_dir() {
        for entry in fs::read_dir(path).unwrap() {
            share(&entry.unwrap().path(), uid);
        }
    }
    chown(path, Some(uid), Some(BROKER_UID)).unwrap();
    fs::set_permissions(
        path,
        fs::Permissions::from_mode(if path.is_dir() {
            0o750
        } else if path.metadata().unwrap().permissions().mode() & 0o111 != 0 {
            0o550
        } else {
            0o440
        }),
    )
    .unwrap();
}

fn policy_command(config: &LauncherConfig, target: &str) -> std::process::Output {
    let expected = crate::launcher_install::qa_policy::policy_digest(config)
        .unwrap()
        .to_string();
    Command::new("/usr/local/lib/louiselm/current/bin/louiselm-skills")
        .args([
            "launcher",
            "qa-policy",
            "--reviewed-policy",
            &expected,
            "--to",
            target,
            "--exclusive-no-contained-sessions",
            "--robot-json",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SUDO_UID", config.operator_uid.to_string())
        .output()
        .unwrap()
}

#[test]
#[expect(
    clippy::too_many_lines,
    reason = "One guest-only transaction consumes the real metadata/input owners, operator caller, launcher, and terminal cleanup."
)]
fn privileged_installed_no_provider_qa_entrypoint() {
    if std::env::var_os("LOUISELM_REQUIRE_QA_ENTRYPOINT").is_none() {
        eprintln!("skipping: requires disposable VM, private mounts and the maintained scripts");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let source = PathBuf::from(std::env::var_os("LOUISELM_TEST_LUA_ROOT").unwrap());
    let root = tempfile::Builder::new()
        .prefix("louiselm-qa-entrypoint-")
        .tempdir_in("/var/lib")
        .unwrap();
    mounts(root.path());
    let _account = BrokerAccount::create();
    let (paths, config, manager) =
        install_unseeded_daemon(root.path(), Enforcement::PreCutover, 12);
    let registry = root.path().join("registry");
    fs::create_dir(SYSTEM_REGISTRY_ROOT).unwrap();
    for name in ["agents.json", "runtimes.json", "envelopes.json"] {
        fs::copy(
            registry.join(name),
            Path::new(SYSTEM_REGISTRY_ROOT).join(name),
        )
        .unwrap();
    }
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o755)).unwrap();
    let operator = root.path().join("operator");
    fs::create_dir(&operator).unwrap();
    chown(&operator, Some(config.operator_uid), Some(BROKER_UID)).unwrap();
    fs::set_permissions(&operator, fs::Permissions::from_mode(0o750)).unwrap();
    let metadata = operator.join("runtime-proposal");
    let proposal =
        crate::workspace::qa_runtime::prepare(&paths.release_prefix.join("current"), &metadata)
            .unwrap();
    let reviewed = Digest::of(&fs::read(metadata.join("proposal.json")).unwrap()).to_string();
    let published = Command::new("/usr/bin/python3")
        .arg(source.join("scripts/install-qa-runtime.py"))
        .arg(&metadata)
        .args(["--reviewed", &reviewed])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SUDO_UID", config.operator_uid.to_string())
        .output()
        .unwrap();
    assert!(
        published.status.success(),
        "{}",
        String::from_utf8_lossy(&published.stderr)
    );
    let registered: serde_json::Value = serde_json::from_slice(&published.stdout).unwrap();
    assert_eq!(registered["state"], "registered_not_authorized");
    assert_eq!(registered["runtime_id"], proposal.runtime.id);
    let trusted = Registry::open_trusted(Path::new(SYSTEM_REGISTRY_ROOT)).unwrap();
    assert_eq!(
        trusted.agent_ids().len(),
        2,
        "the old Agent must remain unchanged"
    );
    let repo = operator.join("source");
    fs::create_dir(&repo).unwrap();
    fs::write(repo.join("AGENTS.md"), b"offline QA instructions\n").unwrap();
    for args in [
        vec!["init", "--quiet"],
        vec!["add", "AGENTS.md"],
        vec![
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@invalid",
            "commit",
            "--quiet",
            "-m",
            "fixture",
        ],
    ] {
        assert!(
            Command::new("/usr/bin/git")
                .args(args)
                .current_dir(&repo)
                .env_clear()
                .env("PATH", "/usr/bin:/bin")
                .env("GIT_CONFIG_NOSYSTEM", "1")
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .status()
                .unwrap()
                .success()
        );
    }
    let snapshot = operator.join("snapshot");
    let preview = crate::workspace::prepare(&repo, &[], &snapshot).unwrap();
    let cache = operator.join("empty-cache");
    fs::create_dir(&cache).unwrap();
    let cache_base = crate::cache::CacheBase::capture(&cache).unwrap();
    let inputs = operator.join("inputs");
    let store = crate::Store::open(&operator.join("empty-mask-store")).unwrap();
    let prepared = crate::workspace::launch_inputs::prepare(
        &store,
        &crate::Policy::embedded(),
        &trusted,
        &crate::workspace::launch_inputs::Preparation {
            agent_id: registered["agent_id"].as_str().unwrap(),
            skills: SkillsMode::Off,
            envelope_id: "desktop-qa-denied",
            snapshot: &snapshot,
            snapshot_digest: &Digest::parse(&preview.snapshot_digest).unwrap(),
            cache: &cache,
            cache_digest: cache_base.digest(),
            project_instructions: &["AGENTS.md".into()],
        },
        &inputs,
    )
    .unwrap();
    share(&inputs, config.operator_uid);
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    let parent = rustix::process::getppid()
        .unwrap()
        .as_raw_nonzero()
        .get()
        .cast_unsigned();
    let previous = certify(&paths, Instant::now() + Duration::from_mins(3), parent).unwrap();
    assert_eq!(
        previous.observations.result().unwrap(),
        ReportResult::Passed
    );
    let cached =
        SystemLaunchPlatform::new(paths.clone(), config.clone(), Duration::from_secs(5)).unwrap();
    let occupied = crate::launcher_install::acquire_identity(&paths, 0).unwrap();
    let refused = policy_command(&config, "enforced");
    assert!(
        !refused.status.success() && String::from_utf8_lossy(&refused.stderr).contains("occupied")
    );
    assert_eq!(
        crate::launcher_install::public_runtime_config(&paths).unwrap(),
        config
    );
    occupied.release().unwrap();
    let switched = policy_command(&config, "enforced");
    assert!(
        switched.status.success(),
        "{}",
        String::from_utf8_lossy(&switched.stderr)
    );
    let before = config.clone();
    let config = crate::launcher_install::public_runtime_config(&paths).unwrap();
    let mut unchanged = config.clone();
    unchanged.conformance = before.conformance;
    assert_eq!(unchanged, before);
    assert_eq!(config.conformance, Enforcement::Enforced);
    let stale = policy_command(&before, "pre_cutover");
    assert!(
        !stale.status.success()
            && String::from_utf8_lossy(&stale.stderr).contains("review mismatch")
    );
    let stale_start = cached.acquire_identity(config.pool.identity(0).unwrap());
    let refusal = match stale_start {
        Ok(identity) => {
            identity.release().unwrap();
            None
        }
        Err(error) => Some(error),
    };
    assert_eq!(
        refusal,
        Some(SupervisorError::ConformanceUnavailable),
        "a constructor captured before the policy change must not acquire launch authority afterward"
    );
    let changed_host = measure(&paths, &config, Instant::now() + Duration::from_mins(3)).unwrap();
    assert!(
        !previous.is_current(&changed_host),
        "old policy certification must not certify the QA window"
    );
    let certificate = certify(&paths, Instant::now() + Duration::from_mins(3), parent).unwrap();
    assert_eq!(
        certificate.observations.result().unwrap(),
        ReportResult::Passed
    );
    let artifact = operator.join("round");
    // The second ID represents another reviewed release only for this control-composition fixture;
    // no default switch, hardware ceremony or retained-pin acceptance is inferred.
    let current = crate::release::read_manifest(&paths.release_prefix.join("current"))
        .unwrap()
        .release_id;
    let other = Digest::of(b"second release not exercised by this fixture").to_string();
    let run_as_operator = |script: &str| {
        let mut command = Command::new("/usr/bin/setpriv");
        command
            .args([
                "--reuid",
                &config.operator_uid.to_string(),
                "--regid",
                &config.operator_uid.to_string(),
                "--clear-groups",
                "/usr/bin/python3",
            ])
            .arg(source.join(script))
            .env_clear()
            .env("PATH", "/usr/bin:/bin");
        command
    };
    let prepared_round = run_as_operator("scripts/conformance-qa.py")
        .args(["prepare", "--input"])
        .arg(&inputs)
        .args([
            "--digest",
            &prepared.manifest_digest,
            "--releases",
            &current,
            &other,
            "--output",
        ])
        .arg(&artifact)
        .output()
        .unwrap();
    assert!(
        prepared_round.status.success(),
        "{}",
        String::from_utf8_lossy(&prepared_round.stderr)
    );
    let selection_path = artifact.join("selection.json");
    let selection: serde_json::Value =
        serde_json::from_slice(&fs::read(&selection_path).unwrap()).unwrap();
    // Python preserves its selection wire order; the exact file bytes are the reviewed identity.
    let selection_digest = Digest::of(&fs::read(&selection_path).unwrap()).to_string();
    let mut coordinator = run_as_operator("scripts/conformance-qa.py")
        .arg("run")
        .arg(&selection_path)
        .args([
            "--reviewed",
            &selection_digest,
            "--exclusive-enforced-window",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    coordinator.stdin.take().unwrap().write_all(b"start\nstatus\ncommand\npark\nstatus\nresume\ncommand\ndispose\nstart\nstatus\ncommand\ndispose\nquit\n").unwrap();
    let completed = coordinator.wait_with_output().unwrap();
    assert!(
        completed.status.success(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&completed.stdout),
        String::from_utf8_lossy(&completed.stderr)
    );
    let journal: serde_json::Value =
        serde_json::from_slice(&fs::read(artifact.join("journal.json")).unwrap()).unwrap();
    assert_eq!(journal["next"], 2);
    assert!(journal["active"].is_null() && journal["pending"].is_null());
    let first: GrantRequest = serde_json::from_value(selection["grants"][0].clone()).unwrap();
    assert!(first.provider_requests.is_none() && first.beads_mutations.is_none());
    let restored = policy_command(&config, "pre_cutover");
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    assert_eq!(
        crate::launcher_install::public_runtime_config(&paths).unwrap(),
        before
    );
    println!(
        "INSTALLED_NO_PROVIDER_QA_ENTRYPOINT_PASSED (two Sessions; terminal cleanup; no desktop/hardware acceptance)"
    );
    terminate(&mut daemon);
}
