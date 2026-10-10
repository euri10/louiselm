//! Two real software-signed fixture installations, not hardware/clean-build acceptance.
use super::*;
use crate::{release, signer::SshKeygenSigner, sshsig::SkPolicy, trust::TrustStore};

pub(super) struct Releases {
    store: crate::Store,
    pub(super) baseline: String,
    pub(super) first: PathBuf,
    pub(super) second: PathBuf,
    pub(super) first_id: String,
    pub(super) second_id: String,
}

impl Releases {
    pub(super) fn prepare(paths: &LauncherPaths, root: &Path) -> Self {
        let original = release::read_manifest(&paths.release_prefix.join("current")).unwrap();
        let keys = root.join("software-release-keys");
        fs::create_dir(&keys).unwrap();
        fs::set_permissions(&keys, fs::Permissions::from_mode(0o700)).unwrap();
        for name in ["primary", "release"] {
            assert!(
                Command::new("/usr/bin/ssh-keygen")
                    .args(["-q", "-t", "ed25519", "-N", "", "-f"])
                    .arg(keys.join(name))
                    .status()
                    .unwrap()
                    .success()
            );
        }
        let store = crate::Store::open(&keys.join("provisional-fixture-store")).unwrap();
        TrustStore::bootstrap(
            &store,
            "louiselm/qa-software-fixture",
            &fs::read_to_string(keys.join("primary.pub")).unwrap(),
            &fs::read_to_string(keys.join("release.pub")).unwrap(),
            SkPolicy::none(),
            1,
        )
        .unwrap();
        let mut bundles = Vec::new();
        let mut ids = Vec::new();
        for index in 1..=2 {
            let bundle = root.join(format!("signed-qa-fixture-{index}"));
            let mut source = original.source.clone();
            source.commit = "0123456789abcdef0123456789abcdef01234567".into();
            source.describe =
                "explicit toy-source software-key fixture, not clean-build/hardware evidence"
                    .into();
            let manifest = release::assemble(
                &release::AssembleRequest {
                    source,
                    toolchain: original.toolchain.clone(),
                    policy: &crate::Policy::embedded(),
                    built_at_ms: original.built_at_ms + index,
                    components: original
                        .components
                        .iter()
                        .map(|component| release::ComponentInput {
                            name: component.name.clone(),
                            path: paths.release_prefix.join("current").join(&component.path),
                            kind: release::ComponentKind::Executable,
                        })
                        .collect(),
                },
                &bundle,
            )
            .unwrap();
            release::sign_bundle(
                &store,
                &bundle,
                &SshKeygenSigner::new(&keys.join("release")),
            )
            .unwrap();
            bundles.push(bundle);
            ids.push(manifest.release_id);
        }
        Self {
            store,
            baseline: original.release_id,
            first: bundles.remove(0),
            second: bundles.remove(0),
            first_id: ids.remove(0),
            second_id: ids.remove(0),
        }
    }

    pub(super) fn install(&self, paths: &LauncherPaths, second: bool) -> LauncherConfig {
        let old = crate::launcher_install::public_runtime_config(paths).unwrap();
        crate::install::install(
            &self.store,
            if second { &self.second } else { &self.first },
            &paths.release_prefix,
            10,
        )
        .unwrap();
        let installed = crate::launcher_install::install(
            paths,
            &SystemCommandRunner,
            &InstallRequest {
                operator: old.operator,
                broker_uid: old.broker_uid,
                broker_gid: old.broker_gid,
                pool: old.pool,
            },
            11,
        )
        .unwrap();
        assert!(installed.failures.is_empty(), "{:?}", installed.failures);
        crate::launcher_install::public_runtime_config(paths).unwrap()
    }
}

fn operator_read(config: &LauncherConfig, session: &str) -> serde_json::Value {
    let output = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &config.operator_uid.to_string(),
            "--regid",
            &config.operator_uid.to_string(),
            "--clear-groups",
            "/usr/local/lib/louiselm/current/bin/louiselm-control",
            "session",
            "conformance",
            session,
            "--json",
        ])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).unwrap()
}

fn fault_command(
    config: &LauncherConfig,
    operation: &str,
    arguments: &[&str],
) -> std::process::Output {
    Command::new("/usr/local/lib/louiselm/current/bin/louiselm-skills")
        .args(["launcher", "qa-fault", operation])
        .arg("--exclusive-qa-window")
        .args(arguments)
        .arg("--robot-json")
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("SUDO_UID", config.operator_uid.to_string())
        .output()
        .unwrap()
}

fn wait_observation(config: &LauncherConfig, session: &str, suspended: bool) -> serde_json::Value {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let value = operator_read(config, session);
        if value["last_check"]["suspended"] == suspended {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "expected suspension={suspended}: {value}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[expect(
    clippy::too_many_lines,
    reason = "One installed guest transaction checks default continuity, exact reviewed fault, retained bytes/history, restore and no implicit thaw."
)]
pub(super) fn exercise(
    paths: &LauncherPaths,
    releases: &Releases,
    artifact: &Path,
    selection: &serde_json::Value,
) -> LauncherConfig {
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        if let Ok(bytes) = fs::read(artifact.join("journal.json")) {
            let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            if value["spent"] == 1 && value["pending"].is_null() {
                break;
            }
        }
        assert!(
            Instant::now() < deadline,
            "first exact command did not settle"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let session = selection["grants"][0]["request"]["session_id"]
        .as_str()
        .unwrap();
    let run = selection["envelope"]["run_id"].as_str().unwrap();
    let before = operator_read(
        &crate::launcher_install::public_runtime_config(paths).unwrap(),
        session,
    );
    assert_eq!(before["last_check"]["suspended"], false);
    let config = releases.install(paths, true);
    assert_eq!(config.release_id, releases.second_id);
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        let continued = operator_read(&config, session);
        assert_eq!(
            continued["last_check"]["suspended"], false,
            "default replacement must preserve the retired pin"
        );
        if continued["last_check"]["sequence"].as_u64().unwrap()
            > before["last_check"]["sequence"].as_u64().unwrap()
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "retired pin never produced another current check"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
    let newer = paths
        .release_prefix
        .join("releases")
        .join(&releases.second_id)
        .join("bin/louiselm-launch");
    let retained = paths
        .release_prefix
        .join("releases")
        .join(&releases.first_id)
        .join("bin/louiselm-launch");
    let original = fs::read(&retained).unwrap();
    let binding = paths
        .state_root
        .join("receipt-bindings")
        .join(format!("{}.json", Digest::of(session.as_bytes()).hex()));
    let genesis = fs::read(&binding).unwrap();
    for refused in [&releases.baseline, &releases.second_id] {
        assert!(
            !fault_command(
                &config,
                "plan",
                &[
                    "--baseline",
                    &releases.baseline,
                    "--retired",
                    refused,
                    "--run",
                    run,
                    "--session",
                    session
                ]
            )
            .status
            .success()
        );
    }
    let args = [
        "--baseline",
        &releases.baseline,
        "--retired",
        &releases.first_id,
        "--run",
        run,
        "--session",
        session,
    ];
    let proposed = fault_command(&config, "plan", &args);
    assert!(
        proposed.status.success(),
        "{}",
        String::from_utf8_lossy(&proposed.stderr)
    );
    let plan: serde_json::Value = serde_json::from_slice(&proposed.stdout).unwrap();
    let reviewed = plan["reviewed"].as_str().unwrap();
    let recovery = paths
        .state_root
        .join("private")
        .join(format!("qa-fault-{}.json", &reviewed[7..]));
    assert!(!recovery.exists());
    let mut apply_args = args.to_vec();
    apply_args.extend([
        "--reviewed",
        "sha256:0000000000000000000000000000000000000000000000000000000000000000",
    ]);
    assert!(
        !fault_command(&config, "apply", &apply_args)
            .status
            .success()
    );
    assert!(!recovery.exists(), "stale review must have no effect");
    *apply_args.last_mut().unwrap() = reviewed;
    let applied = fault_command(&config, "apply", &apply_args);
    assert!(
        applied.status.success(),
        "{}",
        String::from_utf8_lossy(&applied.stderr)
    );
    assert_eq!(retained.metadata().unwrap().mode() & 0o777, 0o444);
    assert_eq!(newer.metadata().unwrap().mode() & 0o777, 0o555);
    assert_eq!(fs::read(&retained).unwrap(), original);
    let record: serde_json::Value = serde_json::from_slice(&fs::read(&recovery).unwrap()).unwrap();
    assert_eq!(record["plan"], plan["plan"]);
    assert_eq!(record["restored"], false);
    assert_eq!(recovery.metadata().unwrap().mode() & 0o777, 0o600);
    let suspended = wait_observation(&config, session, true);
    assert_eq!(suspended["last_check"]["check"]["kind"], "invalid");
    assert_eq!(suspended["admission"], before["admission"]);
    assert_eq!(
        operator_read(&config, session)["last_check"]["last_success_at_ms"],
        suspended["last_check"]["last_success_at_ms"],
        "inspection cannot refresh failure history"
    );
    assert!(
        !fault_command(&config, "apply", &apply_args)
            .status
            .success()
    );
    let preserved = retained.with_file_name("qa-preserved-exact-launcher");
    fs::rename(&retained, &preserved).unwrap();
    symlink(&preserved, &retained).unwrap();
    assert!(
        !fault_command(&config, "restore", &["--reviewed", reviewed])
            .status
            .success()
    );
    assert_eq!(preserved.metadata().unwrap().mode() & 0o777, 0o444);
    // Remove only the new hostile-fixture symlink; the original inode is preserved.
    assert!(
        fs::symlink_metadata(&retained)
            .unwrap()
            .file_type()
            .is_symlink()
    );
    fs::remove_file(&retained).unwrap();
    fs::rename(preserved, &retained).unwrap();
    let restored = fault_command(&config, "restore", &["--reviewed", reviewed]);
    assert!(
        restored.status.success(),
        "{}",
        String::from_utf8_lossy(&restored.stderr)
    );
    assert_eq!(retained.metadata().unwrap().mode() & 0o777, 0o555);
    assert_eq!(fs::read(&retained).unwrap(), original);
    assert_eq!(fs::read(binding).unwrap(), genesis);
    let record: serde_json::Value = serde_json::from_slice(&fs::read(recovery).unwrap()).unwrap();
    assert_eq!(record["plan"], plan["plan"]);
    assert_eq!(record["restored"], true);
    assert!(
        !fault_command(&config, "apply", &apply_args)
            .status
            .success(),
        "retained recovery record blocks re-applying the fault"
    );
    assert_eq!(retained.metadata().unwrap().mode() & 0o777, 0o555);
    assert_eq!(
        crate::launcher_install::public_runtime_config(paths).unwrap(),
        config
    );
    let parent = rustix::process::getppid()
        .unwrap()
        .as_raw_nonzero()
        .get()
        .cast_unsigned();
    let certificate = certify(paths, Instant::now() + Duration::from_mins(3), parent).unwrap();
    assert_eq!(
        certificate.observations.result().unwrap(),
        ReportResult::Passed
    );
    assert_eq!(
        operator_read(&config, session)["last_check"]["suspended"],
        true,
        "restore/recertification must not thaw"
    );
    println!("RETIRED_QA_RELEASE_FAULT_RESTORED_NOT_RESUMED (software-key fixture only)");
    config
}
