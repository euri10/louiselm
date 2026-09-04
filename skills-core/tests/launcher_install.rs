//! Persistent authority needed by the fixed privileged launcher entrypoint.

use std::{
    cell::{Cell, RefCell},
    ffi::OsString,
    fs, io,
    os::unix::fs::{MetadataExt, PermissionsExt, symlink},
    path::{Path, PathBuf},
    sync::{Arc, Barrier},
    time::Duration,
};

use louiselm_skills::{
    Digest,
    install::{InstalledState, STATE_SCHEMA as RELEASE_STATE_SCHEMA},
    launch_supervisor::{LaunchPlatform, SupervisorError, SystemLaunchPlatform},
    launcher_install::{
        CommandInvocation, CommandOutput, CommandRunner, IdentityPool, InstallRequest,
        LauncherPaths, RotationRequest, acquire_identity, install, public_keyring, rotate,
        runtime_config, status, sudo_invocation,
    },
    registry::NetworkPolicy,
    release::{
        Component, MANIFEST_SCHEMA, PolicyIdentity, ReleaseManifest, SourceIdentity,
        ToolchainIdentity,
    },
    sandbox::{ConfinementPlan, IdentityPlan},
};
use tempfile::TempDir;

struct Fixture {
    root: TempDir,
    paths: LauncherPaths,
    launcher_digest: String,
    release_id: String,
}

impl Fixture {
    fn new() -> Self {
        let root = TempDir::new().expect("temporary root is creatable");
        let root_path = root.path();
        let paths = LauncherPaths {
            release_prefix: root_path.join("usr/local/lib/louiselm"),
            state_root: root_path.join("usr/local/lib/louiselm/launcher"),
            sudoers: root_path.join("etc/sudoers.d/louiselm-launch"),
            subuid: root_path.join("etc/subuid"),
            subgid: root_path.join("etc/subgid"),
            passwd: root_path.join("etc/passwd"),
            group: root_path.join("etc/group"),
            nsswitch: root_path.join("etc/nsswitch.conf"),
            ssh_keygen: root_path.join("usr/bin/ssh-keygen"),
            getent: root_path.join("usr/bin/getent"),
            visudo: root_path.join("usr/sbin/visudo"),
            bwrap: root_path.join("usr/bin/bwrap"),
            broker_socket: root_path.join("run/louiselm/control.sock"),
        };
        for path in [
            &paths.subuid,
            &paths.subgid,
            &paths.passwd,
            &paths.group,
            &paths.nsswitch,
            &paths.ssh_keygen,
            &paths.getent,
            &paths.visudo,
            &paths.bwrap,
        ] {
            fs::create_dir_all(path.parent().expect("fixture path has a parent"))
                .expect("fixture parent is creatable");
        }
        fs::write(&paths.subuid, "existing:100000:1000\n").expect("subuid is writable");
        fs::write(&paths.subgid, "existing:100000:1000\n").expect("subgid is writable");
        fs::write(
            &paths.passwd,
            "root:x:0:0:root:/root:/bin/sh\nlouise:x:1000:1000::/home/louise:/bin/sh\n",
        )
        .expect("passwd is writable");
        fs::write(&paths.group, "root:x:0:\nlouise:x:1000:\n").expect("group is writable");
        fs::write(
            &paths.nsswitch,
            "passwd: files\ngroup: files\nsubid: files\n",
        )
        .expect("nsswitch is writable");
        fs::write(
            &paths.ssh_keygen,
            "#!/bin/sh\nif [ \"$1\" = -y ] && [ \"$2\" = -f ]; then IFS= read -r line < \"$3\"; serial=${line##* }; printf 'ssh-ed25519 AAAATESTKEY%02d\\n' \"$serial\"; exit 0; fi\nexit 1\n",
        )
        .expect("ssh-keygen fixture is writable");
        fs::write(&paths.getent, "#!/bin/sh\nexit 2\n").expect("getent fixture is writable");
        fs::write(&paths.visudo, "visudo fixture\n").expect("visudo fixture is writable");
        fs::write(&paths.bwrap, "#!/bin/sh\nprintf 'bubblewrap fixture\\n'\n")
            .expect("bwrap fixture is writable");
        for tool in [
            &paths.ssh_keygen,
            &paths.getent,
            &paths.visudo,
            &paths.bwrap,
        ] {
            fs::set_permissions(tool, fs::Permissions::from_mode(0o755))
                .expect("tool mode is settable");
        }

        let launcher = b"measured louiselm-launch\n";
        let launcher_digest = Digest::of(launcher).to_string();
        let mut manifest = ReleaseManifest {
            schema: MANIFEST_SCHEMA.to_owned(),
            release_id: String::new(),
            version: "0.1.0".to_owned(),
            built_at_ms: 1_756_800_000_000,
            source: SourceIdentity {
                commit: "0123456789abcdef0123456789abcdef01234567".to_owned(),
                clean: true,
                describe: "test".to_owned(),
                dependencies_digest: Digest::of(b"Cargo.lock").to_string(),
            },
            toolchain: ToolchainIdentity {
                rustc: "1.97.1".to_owned(),
                cargo: "1.97.1".to_owned(),
                target: "x86_64-linux".to_owned(),
            },
            policy: PolicyIdentity {
                version: "1".to_owned(),
                digest: Digest::of(b"policy").to_string(),
            },
            schemas: Vec::new(),
            components: vec![Component {
                name: "louiselm-launch".to_owned(),
                path: "bin/louiselm-launch".to_owned(),
                sha256: Digest::of(launcher).hex().to_owned(),
                size: launcher.len() as u64,
                executable: true,
            }],
        };
        manifest.release_id = manifest.digest().to_string();
        let release_id = manifest.release_id.clone();
        let release_root = paths.release_prefix.join("releases").join(&release_id);
        fs::create_dir_all(release_root.join("bin")).expect("release root is creatable");
        fs::write(release_root.join("bin/louiselm-launch"), launcher)
            .expect("launcher is writable");
        fs::set_permissions(
            release_root.join("bin/louiselm-launch"),
            fs::Permissions::from_mode(0o555),
        )
        .expect("launcher mode is settable");
        fs::write(
            release_root.join("manifest.json"),
            serde_json::to_vec(&manifest).expect("manifest serializes"),
        )
        .expect("manifest is writable");
        symlink(
            Path::new("releases").join(&release_id),
            paths.release_prefix.join("current"),
        )
        .expect("current symlink is creatable");
        fs::write(
            paths.release_prefix.join("state.json"),
            serde_json::to_vec(&InstalledState {
                schema: RELEASE_STATE_SCHEMA.to_owned(),
                release_id: release_id.clone(),
                built_at_ms: manifest.built_at_ms,
                installed_at_ms: 1_756_800_000_001,
                source_commit: manifest.source.commit,
                policy_version: manifest.policy.version,
            })
            .expect("state serializes"),
        )
        .expect("release state is writable");

        Self {
            root,
            paths,
            launcher_digest,
            release_id,
        }
    }

    fn request(&self) -> InstallRequest {
        InstallRequest {
            operator: "louise".to_owned(),
            broker_uid: 1_500,
            broker_gid: 1_500,
            pool: IdentityPool {
                uid_start: 200_000,
                gid_start: 300_000,
                slots: 4,
            },
        }
    }

    fn root(&self) -> &Path {
        self.root.path()
    }
}

#[derive(Default)]
struct FakeRunner {
    calls: RefCell<Vec<CommandInvocation>>,
    generated: Cell<u32>,
    fail_keygen: Cell<bool>,
    fail_visudo: Cell<bool>,
    validated_sudoers: RefCell<Vec<(Vec<u8>, u32)>>,
    getent_collision: Cell<bool>,
    getent_response: RefCell<Option<CommandOutput>>,
    make_state_readonly_after_keygen: RefCell<Option<PathBuf>>,
}

impl FakeRunner {
    fn calls(&self) -> Vec<CommandInvocation> {
        self.calls.borrow().clone()
    }

    fn keygen_calls(&self) -> usize {
        self.calls()
            .iter()
            .filter(|call| argument_strings(call).iter().any(|arg| arg == "-t"))
            .count()
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, invocation: &CommandInvocation) -> io::Result<CommandOutput> {
        self.calls.borrow_mut().push(invocation.clone());
        let arguments = argument_strings(invocation);
        if invocation
            .program
            .file_name()
            .and_then(|name| name.to_str())
            == Some("ssh-keygen")
            && arguments.iter().any(|argument| argument == "-t")
        {
            if self.fail_keygen.get() {
                return Ok(CommandOutput::failure("entropy unavailable"));
            }
            let key_path = PathBuf::from(
                arguments
                    .iter()
                    .position(|argument| argument == "-f")
                    .and_then(|index| arguments.get(index + 1))
                    .expect("keygen invocation has -f"),
            );
            let serial = self.generated.get() + 1;
            self.generated.set(serial);
            fs::write(&key_path, format!("PRIVATE KEY {serial}\n"))?;
            fs::write(
                key_path.with_extension("pub"),
                format!("ssh-ed25519 AAAATESTKEY{serial:02} louiselm-launch\n"),
            )?;
            if let Some(state_root) = self.make_state_readonly_after_keygen.borrow_mut().take() {
                fs::set_permissions(state_root, fs::Permissions::from_mode(0o555))?;
            }
            return Ok(CommandOutput::success());
        }
        if invocation
            .program
            .file_name()
            .and_then(|name| name.to_str())
            == Some("ssh-keygen")
            && arguments.first().is_some_and(|argument| argument == "-y")
        {
            let key_path = PathBuf::from(
                arguments
                    .iter()
                    .position(|argument| argument == "-f")
                    .and_then(|index| arguments.get(index + 1))
                    .expect("public-key derivation has -f"),
            );
            let private = fs::read_to_string(key_path)?;
            let serial = private
                .split_whitespace()
                .last()
                .expect("fake private key has a serial");
            return Ok(CommandOutput {
                success: true,
                exit_code: Some(0),
                stdout: format!("ssh-ed25519 AAAATESTKEY{serial:0>2}\n").into_bytes(),
                stderr: Vec::new(),
            });
        }
        if invocation
            .program
            .file_name()
            .and_then(|name| name.to_str())
            == Some("visudo")
        {
            if arguments.len() != 2 || arguments[0] != "-cf" {
                return Ok(CommandOutput::failure("unexpected visudo arguments"));
            }
            let candidate = PathBuf::from(&arguments[1]);
            let metadata = fs::symlink_metadata(&candidate)?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.mode() & 0o777 != 0o600
            {
                return Ok(CommandOutput::failure("unsafe sudoers candidate"));
            }
            self.validated_sudoers
                .borrow_mut()
                .push((fs::read(candidate)?, metadata.mode() & 0o777));
            return Ok(if self.fail_visudo.get() {
                CommandOutput::failure("invalid sudoers")
            } else {
                CommandOutput::success()
            });
        }
        if invocation
            .program
            .file_name()
            .and_then(|name| name.to_str())
            == Some("getent")
        {
            if let Some(output) = self.getent_response.borrow().clone() {
                return Ok(output);
            }
            return Ok(if self.getent_collision.get() {
                CommandOutput {
                    success: true,
                    exit_code: Some(0),
                    stdout: b"remote:x:200002:300002::/:/bin/false\n".to_vec(),
                    stderr: Vec::new(),
                }
            } else {
                CommandOutput {
                    success: false,
                    exit_code: Some(2),
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                }
            });
        }
        Ok(CommandOutput::success())
    }
}

fn argument_strings(invocation: &CommandInvocation) -> Vec<String> {
    invocation
        .arguments
        .iter()
        .map(|argument: &OsString| argument.to_string_lossy().into_owned())
        .collect()
}

fn mode(path: &Path) -> u32 {
    fs::symlink_metadata(path)
        .expect("path has metadata")
        .mode()
        & 0o777
}

#[test]
fn install_pins_one_release_key_pool_and_exact_sudo_command_idempotently() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();

    let first = install(
        &fixture.paths,
        &runner,
        &fixture.request(),
        1_756_800_100_000,
    )
    .expect("first install succeeds");
    let first_key = first.active_key_id.clone().expect("active key is reported");
    let second = install(
        &fixture.paths,
        &runner,
        &fixture.request(),
        1_756_800_200_000,
    )
    .expect("reinstall succeeds");

    assert_eq!(second.active_key_id.as_deref(), Some(first_key.as_str()));
    assert_eq!(
        runner.keygen_calls(),
        1,
        "reinstall must not replace the key"
    );
    assert_eq!(
        fs::read_to_string(&fixture.paths.subuid).unwrap(),
        "existing:100000:1000\n0:200000:4\n"
    );
    assert_eq!(
        fs::read_to_string(&fixture.paths.subgid).unwrap(),
        "existing:100000:1000\n0:300000:4\n"
    );

    let config = second.config.as_ref().expect("configuration is reported");
    assert_eq!(config.release_id, fixture.release_id);
    assert_eq!(config.launcher_digest, fixture.launcher_digest);
    assert_eq!(config.operator, "louise");
    assert_eq!(config.operator_uid, 1000);
    assert_eq!(config.broker_uid, 1_500);
    assert_eq!(config.broker_gid, 1_500);
    assert_eq!(config.broker_socket_path, fixture.paths.broker_socket);
    assert_eq!(config.pool, fixture.request().pool);
    assert_eq!(config.bwrap_path, fixture.paths.bwrap);
    assert_eq!(
        config.bwrap_digest,
        Digest::of(&fs::read(&fixture.paths.bwrap).unwrap()).to_string(),
    );
    assert_eq!(config.ssh_keygen_path, fixture.paths.ssh_keygen);
    assert_eq!(
        config.ssh_keygen_digest,
        Digest::of(&fs::read(&fixture.paths.ssh_keygen).unwrap()).to_string(),
    );

    let sudoers = fs::read_to_string(&fixture.paths.sudoers).expect("sudoers is readable");
    let launcher_path = fixture
        .paths
        .release_prefix
        .join("current/bin/louiselm-launch");
    assert_eq!(
        sudoers,
        format!(
            "# Managed by louiselm-skills. Do not edit.\nDefaults!{} fdexec=digest_only\n#1000 ALL=(root:root) NOPASSWD: NOSETENV: sha256:{} {} run\n",
            launcher_path.display(),
            Digest::parse(&fixture.launcher_digest).unwrap().hex(),
            launcher_path.display(),
        )
    );
    assert!(
        runner
            .validated_sudoers
            .borrow()
            .iter()
            .all(|(candidate, candidate_mode)| {
                candidate == sudoers.as_bytes() && *candidate_mode == 0o600
            }),
        "visudo validates the exact bytes before publication"
    );
    assert!(
        !sudoers.contains(&fixture.release_id),
        "the release identity is root configuration, not caller-controlled argv: {sudoers}"
    );
    assert_eq!(mode(&fixture.paths.sudoers), 0o440);
    assert_eq!(mode(&fixture.paths.state_root), 0o711);
    assert_eq!(mode(&fixture.paths.state_root.join("config.json")), 0o600);
    assert_eq!(mode(&fixture.paths.state_root.join("keyring.json")), 0o444);
    assert_eq!(mode(&fixture.paths.state_root.join("private")), 0o700);
    assert_eq!(mode(&fixture.paths.state_root.join("locks")), 0o700);

    let calls = runner.calls();
    let visudo = calls
        .iter()
        .find(|call| call.program == fixture.paths.visudo)
        .expect("absolute configured visudo is invoked");
    let visudo_arguments = argument_strings(visudo);
    assert_eq!(visudo_arguments[0], "-cf");
    assert_eq!(visudo_arguments.len(), 2);
    assert!(
        Path::new(&visudo_arguments[1])
            .starts_with(fixture.paths.state_root.join("private/scratch"))
    );
    assert!(
        calls
            .iter()
            .filter(|call| argument_strings(call).iter().any(|arg| arg == "-t"))
            .all(|call| call.program == fixture.paths.ssh_keygen)
    );

    let invocation = sudo_invocation(config);
    assert_eq!(invocation.program, PathBuf::from("/usr/bin/sudo"));
    assert_eq!(
        argument_strings(&invocation),
        vec![
            "-n",
            fixture
                .paths
                .release_prefix
                .join("current/bin/louiselm-launch")
                .to_string_lossy()
                .as_ref(),
            "run",
        ]
    );
}

#[test]
fn measured_bubblewrap_and_dedicated_broker_identity_are_required() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    install(&fixture.paths, &runner, &fixture.request(), 10).unwrap();

    fs::write(&fixture.paths.bwrap, "replaced bubblewrap\n").unwrap();
    assert!(
        status(&fixture.paths)
            .failures
            .iter()
            .any(|failure| failure.code == "bwrap_changed"),
        "a changed sandbox helper must make launcher authority untrusted"
    );

    for (broker_uid, broker_gid) in [
        (0, 1_500),
        (1_000, 1_500),
        (200_001, 1_500),
        (1_500, 300_001),
    ] {
        let other = Fixture::new();
        let mut request = other.request();
        request.broker_uid = broker_uid;
        request.broker_gid = broker_gid;
        let error = install(&other.paths, &FakeRunner::default(), &request, 10)
            .expect_err("root, operator, and Session-pool identities cannot be the broker");
        assert!(error.to_string().contains("broker"), "{error}");
    }
}

#[test]
fn production_prepare_rejects_bubblewrap_changed_after_runtime_config_before_spawn() {
    if std::env::var_os("LOUISELM_TEST_ROOT_LAUNCHER").is_none() {
        eprintln!("skipping: set LOUISELM_TEST_ROOT_LAUNCHER in the root fixture");
        return;
    }
    assert!(
        rustix::process::geteuid().is_root(),
        "the production platform requires root"
    );
    if std::env::var_os("LOUISELM_REQUIRE_INITIAL_HOST_IDENTITY").is_some() {
        assert_eq!(
            fs::read_to_string("/proc/self/uid_map").unwrap(),
            format!("{:>10} {:>10} {:>10}\n", 0, 0, u32::MAX),
            "CI must exercise the initial user namespace"
        );
    }

    let fixture = Fixture::new();
    install(
        &fixture.paths,
        &FakeRunner::default(),
        &fixture.request(),
        10,
    )
    .expect("launcher authority installs");
    let config = runtime_config(&fixture.paths).expect("runtime authority validates");
    let platform = SystemLaunchPlatform::new(fixture.paths.clone(), config, Duration::from_secs(5))
        .expect("production platform opens the measured backend");

    let marker = fixture.root().join("mutated-bwrap-spawned");
    fs::write(
        &fixture.paths.bwrap,
        format!(
            "#!/bin/sh\nprintf spawned > '{}'\nexit 97\n",
            marker.display()
        ),
    )
    .expect("measured bwrap is replaced after runtime configuration");
    let result = platform.prepare(ConfinementPlan {
        session_id: "changed-bwrap".to_owned(),
        runtime_root: fixture.root().join("runtime"),
        executable: PathBuf::from("/bin/true"),
        arguments: Vec::new(),
        environment: Default::default(),
        home: fixture.root().join("sessions/changed-bwrap/home"),
        workspace: fixture.root().join("sessions/changed-bwrap/workspace"),
        system_roots: Vec::new(),
        network: NetworkPolicy::Denied,
        identity: IdentityPlan::NamespaceOnly,
        channels: Vec::new(),
    });

    assert!(matches!(result, Err(SupervisorError::SpawnFailed)));
    assert!(
        !marker.exists(),
        "a changed measured backend must be rejected before execution"
    );
}

#[test]
fn rotation_replays_once_and_keeps_every_old_public_and_private_key() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    let installed = install(&fixture.paths, &runner, &fixture.request(), 10).unwrap();
    let first = installed.active_key_id.expect("first key exists");
    let request = RotationRequest {
        rotation_id: "incident-2026-09".to_owned(),
        expected_active_key_id: first.clone(),
    };

    let rotated = rotate(&fixture.paths, &runner, &request, 20).expect("rotation succeeds");
    let replayed = rotate(&fixture.paths, &runner, &request, 30).expect("rotation replay succeeds");

    assert!(rotated.created);
    assert!(!replayed.created);
    assert_eq!(rotated.key_id, replayed.key_id);
    assert_eq!(
        runner.keygen_calls(),
        2,
        "one install and one rotation only"
    );
    let keyring = public_keyring(&fixture.paths).expect("public keyring is readable");
    assert_eq!(keyring.active_key_id, rotated.key_id);
    assert!(keyring.key(&first).is_some(), "retired public key remains");
    assert!(keyring.key(&rotated.key_id).is_some());
    assert_eq!(keyring.retained_key_ids(), vec![first.clone()]);
    let private_keys = fs::read_dir(fixture.paths.state_root.join("private/keys"))
        .expect("private key directory is readable")
        .count();
    assert_eq!(private_keys, 2, "retired private key remains available");

    let conflicting = RotationRequest {
        rotation_id: request.rotation_id,
        expected_active_key_id: rotated.key_id,
    };
    let error = rotate(&fixture.paths, &runner, &conflicting, 40)
        .expect_err("one rotation id cannot describe a different transition");
    assert!(error.to_string().contains("rotation id"), "{error}");
}

#[test]
fn rotation_resumes_the_same_key_after_post_generation_failure() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    let installed = install(&fixture.paths, &runner, &fixture.request(), 10).unwrap();
    let request = RotationRequest {
        rotation_id: "fault-injected-rotation".to_owned(),
        expected_active_key_id: installed.active_key_id.unwrap(),
    };
    *runner.make_state_readonly_after_keygen.borrow_mut() =
        Some(fixture.paths.state_root.join("private"));

    rotate(&fixture.paths, &runner, &request, 20)
        .expect_err("journal update is fault-injected after key generation");
    assert_eq!(runner.keygen_calls(), 2);
    assert!(
        fixture
            .paths
            .state_root
            .join("private/pending-rotation.json")
            .exists()
    );
    assert!(
        status(&fixture.paths)
            .failures
            .iter()
            .any(|failure| failure.code == "rotation_incomplete")
    );

    let resumed = rotate(&fixture.paths, &runner, &request, 30)
        .expect("the pending key is published on exact retry");
    assert!(resumed.created);
    assert_eq!(
        runner.keygen_calls(),
        2,
        "retry must not generate a third key"
    );
    assert!(
        !fixture
            .paths
            .state_root
            .join("private/pending-rotation.json")
            .exists()
    );
}

#[test]
fn rotation_reuses_an_empty_destination_after_a_crash_before_key_move() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    let installed = install(&fixture.paths, &runner, &fixture.request(), 10).unwrap();
    let request = RotationRequest {
        rotation_id: "fault-before-key-move".to_owned(),
        expected_active_key_id: installed.active_key_id.unwrap(),
    };
    *runner.make_state_readonly_after_keygen.borrow_mut() =
        Some(fixture.paths.state_root.join("private"));
    rotate(&fixture.paths, &runner, &request, 20)
        .expect_err("fault leaves the generated pending key durable");
    let key_id = Digest::of(b"ssh-ed25519 AAAATESTKEY02").to_string();
    let empty_destination = fixture
        .paths
        .state_root
        .join("private/keys")
        .join(key_id.replace(':', "-"));
    fs::create_dir(&empty_destination).unwrap();
    fs::set_permissions(&empty_destination, fs::Permissions::from_mode(0o700)).unwrap();
    let unexpected = empty_destination.join("unexpected");
    fs::write(&unexpected, b"not key material").unwrap();
    rotate(&fixture.paths, &runner, &request, 30)
        .expect_err("retry refuses a nonempty destination directory");
    fs::remove_file(unexpected).unwrap();

    let resumed = rotate(&fixture.paths, &runner, &request, 40)
        .expect("retry completes through the durable empty destination directory");

    assert_eq!(resumed.key_id, key_id);
    assert_eq!(runner.keygen_calls(), 2);
}

#[test]
fn conflicting_or_non_file_identity_authority_fails_before_publication() {
    for damage in ["subuid", "passwd", "nsswitch"] {
        let fixture = Fixture::new();
        let runner = FakeRunner::default();
        match damage {
            "subuid" => fs::write(&fixture.paths.subuid, "other:199999:2\n").unwrap(),
            "passwd" => fs::write(
                &fixture.paths.passwd,
                "root:x:0:0:root:/root:/bin/sh\nlouise:x:1000:1000::/home/louise:/bin/sh\ncollision:x:200002:10::/:/bin/false\n",
            )
            .unwrap(),
            "nsswitch" => fs::write(
                &fixture.paths.nsswitch,
                "passwd: files\ngroup: files\nsubid: ldap\n",
            )
            .unwrap(),
            _ => unreachable!(),
        }

        let error = install(&fixture.paths, &runner, &fixture.request(), 10)
            .expect_err("unsafe identity authority is refused");

        assert!(
            error.to_string().contains("identity")
                || error.to_string().contains("subid")
                || error.to_string().contains("overlap"),
            "{damage}: {error}",
        );
        assert!(
            !fixture.paths.sudoers.exists(),
            "{damage}: sudo authority leaked"
        );
        assert!(!fixture.paths.state_root.join("config.json").exists());
        assert_eq!(
            runner.keygen_calls(),
            0,
            "{damage}: key generation ran before validation"
        );
    }
}

#[test]
fn operator_is_pinned_to_one_non_root_uid() {
    for passwd in [
        "root:x:0:0:root:/root:/bin/sh\ntoor:x:0:0::/:/bin/sh\n",
        "root:x:0:0:root:/root:/bin/sh\nlouise:x:1000:1000::/:/bin/sh\nlouise:x:1001:1001::/:/bin/sh\n",
        "root:x:0:0:root:/root:/bin/sh\nlouise:x:1000:1000::/:/bin/sh\nmallory:x:1000:1001::/:/bin/sh\n",
    ] {
        let fixture = Fixture::new();
        let runner = FakeRunner::default();
        fs::write(&fixture.paths.passwd, passwd).unwrap();
        let mut request = fixture.request();
        if passwd.contains("toor:") {
            request.operator = "toor".to_owned();
        }

        let error = install(&fixture.paths, &runner, &request, 10)
            .expect_err("root aliases and duplicate names cannot gain sudo authority");

        assert!(error.to_string().contains("non-root"), "{error}");
        assert!(!fixture.paths.sudoers.exists());
    }
}

#[test]
fn effective_nss_collisions_fail_without_forbidding_systemd_or_sss_sources() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    fs::write(
        &fixture.paths.nsswitch,
        "passwd: files systemd sss\ngroup: files systemd sss\nsubid: files\n",
    )
    .unwrap();

    install(&fixture.paths, &runner, &fixture.request(), 10)
        .expect("additional NSS sources are safe when effective IDs do not collide");

    let collision = Fixture::new();
    let collision_runner = FakeRunner::default();
    collision_runner.getent_collision.set(true);
    fs::write(
        &collision.paths.nsswitch,
        "passwd: files sss\ngroup: files sss\nsubid: files\n",
    )
    .unwrap();
    let error = install(
        &collision.paths,
        &collision_runner,
        &collision.request(),
        10,
    )
    .expect_err("an effective NSS identity collision is refused");

    assert!(error.to_string().contains("effective NSS"), "{error}");
    assert!(!collision.paths.sudoers.exists());

    for output in [
        CommandOutput {
            success: true,
            exit_code: Some(0),
            stdout: Vec::new(),
            stderr: Vec::new(),
        },
        CommandOutput {
            success: false,
            exit_code: Some(1),
            stdout: Vec::new(),
            stderr: b"resolver unavailable".to_vec(),
        },
        CommandOutput {
            success: false,
            exit_code: Some(3),
            stdout: Vec::new(),
            stderr: b"enumeration failed".to_vec(),
        },
    ] {
        let fixture = Fixture::new();
        let runner = FakeRunner::default();
        *runner.getent_response.borrow_mut() = Some(output);
        install(&fixture.paths, &runner, &fixture.request(), 10)
            .expect_err("only getent exit 2 with empty output proves no collision");
        assert!(!fixture.paths.sudoers.exists());
    }
}

#[test]
fn validator_failure_leaves_the_prior_sudo_boundary_and_identity_files_untouched() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    install(&fixture.paths, &runner, &fixture.request(), 5).unwrap();
    let before_uid = fs::read(&fixture.paths.subuid).unwrap();
    let before_gid = fs::read(&fixture.paths.subgid).unwrap();
    let before_sudoers = fs::read(&fixture.paths.sudoers).unwrap();
    let before_config = fs::read(fixture.paths.state_root.join("config.json")).unwrap();
    runner.fail_visudo.set(true);

    let error = install(&fixture.paths, &runner, &fixture.request(), 10)
        .expect_err("invalid sudoers is refused");

    assert!(error.to_string().contains("visudo"), "{error}");
    assert_eq!(fs::read(&fixture.paths.subuid).unwrap(), before_uid);
    assert_eq!(fs::read(&fixture.paths.subgid).unwrap(), before_gid);
    assert_eq!(fs::read(&fixture.paths.sudoers).unwrap(), before_sudoers);
    assert_eq!(
        fs::read(fixture.paths.state_root.join("config.json")).unwrap(),
        before_config
    );
}

#[test]
fn subordinate_id_rewrite_preserves_supported_file_metadata() {
    let fixture = Fixture::new();
    let mut original_metadata = Vec::new();
    for path in [&fixture.paths.subuid, &fixture.paths.subgid] {
        let metadata = fs::symlink_metadata(path).unwrap();
        original_metadata.push((metadata.uid(), metadata.gid(), metadata.mode() & 0o777));
        match rustix::fs::setxattr(
            path,
            c"user.louiselm-test",
            b"retained",
            rustix::fs::XattrFlags::empty(),
        ) {
            Ok(()) => {}
            Err(rustix::io::Errno::NOTSUP) => return,
            Err(error) => panic!("test xattr is settable: {error}"),
        }
    }

    install(
        &fixture.paths,
        &FakeRunner::default(),
        &fixture.request(),
        10,
    )
    .unwrap();

    for (path, expected_metadata) in [&fixture.paths.subuid, &fixture.paths.subgid]
        .into_iter()
        .zip(original_metadata)
    {
        let mut value = [0; 32];
        let length = rustix::fs::getxattr(path, c"user.louiselm-test", &mut value)
            .expect("subid xattr survives atomic rewrite");
        assert_eq!(&value[..length], b"retained");
        let metadata = fs::symlink_metadata(path).unwrap();
        assert_eq!(
            (metadata.uid(), metadata.gid(), metadata.mode() & 0o777),
            expected_metadata
        );
    }
}

#[test]
fn identity_lease_is_bounded_exclusive_and_survives_reinstall() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    install(&fixture.paths, &runner, &fixture.request(), 10).unwrap();

    let first = acquire_identity(&fixture.paths, 0).expect("first slot is leasable");
    let lock_path = fixture.paths.state_root.join("locks/0.lock");
    let inode = fs::metadata(&lock_path).unwrap().ino();
    let error = acquire_identity(&fixture.paths, 0).expect_err("same slot is exclusive");
    assert!(error.to_string().contains("occupied"), "{error}");
    let second = acquire_identity(&fixture.paths, 1).expect("different slot is independent");
    assert_eq!(first.identity().uid, 200_000);
    assert_eq!(first.identity().gid, 300_000);
    assert_eq!(second.identity().uid, 200_001);

    let occupied = status(&fixture.paths).occupied_slots;
    assert_eq!(occupied, vec![0, 1]);
    install(&fixture.paths, &runner, &fixture.request(), 20).unwrap();
    assert_eq!(fs::metadata(&lock_path).unwrap().ino(), inode);
    assert!(acquire_identity(&fixture.paths, 0).is_err());
    first
        .release()
        .expect("a zero-survivor proof releases its lock");
    acquire_identity(&fixture.paths, 0)
        .expect("released slot is reusable")
        .release()
        .unwrap();
    second.release().unwrap();
    assert!(acquire_identity(&fixture.paths, 4).is_err());
}

#[test]
fn writable_identity_lock_blocks_status_and_leasing() {
    let fixture = Fixture::new();
    install(
        &fixture.paths,
        &FakeRunner::default(),
        &fixture.request(),
        10,
    )
    .unwrap();
    let lock_path = fixture.paths.state_root.join("locks/0.lock");
    fs::set_permissions(&lock_path, fs::Permissions::from_mode(0o666)).unwrap();

    let error = acquire_identity(&fixture.paths, 0)
        .expect_err("a writable persistent lock cannot carry a trusted lease");
    assert!(error.to_string().contains("root-only"), "{error}");
    assert!(
        status(&fixture.paths)
            .failures
            .iter()
            .any(|failure| failure.code == "identity_lock_unreadable")
    );
}

#[test]
fn status_is_actionable_and_never_serializes_private_key_material() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    install(&fixture.paths, &runner, &fixture.request(), 10).unwrap();
    fs::write(&fixture.paths.ssh_keygen, "changed tool\n").unwrap();

    let status = status(&fixture.paths);
    let encoded = serde_json::to_string(&status).expect("status serializes");

    assert!(!status.trusted);
    assert!(
        status
            .failures
            .iter()
            .any(|failure| failure.code == "ssh_keygen_changed"),
        "failures were {:?}",
        status.failures,
    );
    assert!(
        status
            .failures
            .iter()
            .all(|failure| !failure.next_action.is_empty())
    );
    assert!(!encoded.contains("PRIVATE KEY"));
    assert!(!encoded.contains("private/keys"));
    assert!(encoded.contains(&fixture.release_id));
    assert!(encoded.contains("200000"));
    assert!(fixture.root().exists());
}

#[test]
fn changed_private_key_never_reports_a_trusted_authority() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    let installed = install(&fixture.paths, &runner, &fixture.request(), 10).unwrap();
    let key_id = installed.active_key_id.expect("installed key exists");
    let private = fixture
        .paths
        .state_root
        .join("private/keys")
        .join(Digest::parse(&key_id).unwrap().directory_name())
        .join("key");
    fs::write(&private, "PRIVATE KEY 99\n").unwrap();
    fs::set_permissions(&private, fs::Permissions::from_mode(0o600)).unwrap();

    let report = status(&fixture.paths);
    let error = rotate(
        &fixture.paths,
        &runner,
        &RotationRequest {
            rotation_id: "refuse-replaced-private-key".to_owned(),
            expected_active_key_id: key_id,
        },
        20,
    )
    .expect_err("rotation refuses a private/public mismatch");

    assert!(!report.trusted);
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.code == "private_key_mismatch"),
        "failures were {:?}",
        report.failures
    );
    assert!(error.to_string().contains("does not match"), "{error}");
}

#[test]
fn missing_identity_reservation_blocks_status_and_leasing() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    install(&fixture.paths, &runner, &fixture.request(), 10).unwrap();
    fs::write(&fixture.paths.subuid, "existing:100000:1000\n").unwrap();

    let report = status(&fixture.paths);
    let error = acquire_identity(&fixture.paths, 0)
        .expect_err("an unreserved host identity must never be leased");

    assert!(!report.trusted);
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.code == "identity_authority_changed")
    );
    assert!(
        error.to_string().contains("reservation is missing"),
        "{error}"
    );
}

#[test]
fn retry_after_key_generation_failure_recovers_without_duplicate_reservations() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    runner.fail_keygen.set(true);

    let error = install(&fixture.paths, &runner, &fixture.request(), 10)
        .expect_err("failed key generation leaves no sudo authority");
    assert!(error.to_string().contains("ssh-keygen"), "{error}");
    assert!(!fixture.paths.sudoers.exists());
    assert!(
        fixture.paths.state_root.join("config.json").exists(),
        "bootstrap intent survives a keygen failure"
    );
    assert!(!fixture.paths.state_root.join("keyring.json").exists());
    let partial = status(&fixture.paths);
    assert!(partial.failures.iter().any(|failure| {
        failure.code == "bootstrap_incomplete" && failure.next_action.contains("installer")
    }));
    runner.fail_keygen.set(false);

    install(&fixture.paths, &runner, &fixture.request(), 20)
        .expect("retry completes the partial install");
    assert_eq!(
        fs::read_to_string(&fixture.paths.subuid)
            .unwrap()
            .matches("0:200000:4")
            .count(),
        1
    );
    assert_eq!(
        fs::read_to_string(&fixture.paths.subgid)
            .unwrap()
            .matches("0:300000:4")
            .count(),
        1
    );
}

#[test]
fn retry_after_keyring_publication_failure_reuses_the_generated_key() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    *runner.make_state_readonly_after_keygen.borrow_mut() = Some(fixture.paths.state_root.clone());

    install(&fixture.paths, &runner, &fixture.request(), 10)
        .expect_err("keyring publication fails after the private key is durable");
    let keys = fixture.paths.state_root.join("private/keys");
    let generated = fs::read_dir(&keys)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect::<Vec<_>>();
    assert_eq!(generated.len(), 1);
    let generated_key_id = generated[0].replacen("sha256-", "sha256:", 1);
    fs::set_permissions(&fixture.paths.state_root, fs::Permissions::from_mode(0o711)).unwrap();
    let unexpected = keys.join(&generated[0]).join("unexpected");
    fs::write(&unexpected, b"not key material").unwrap();
    install(&fixture.paths, &runner, &fixture.request(), 20)
        .expect_err("retry refuses ambiguous un-enrolled key contents");
    fs::remove_file(unexpected).unwrap();

    let report = install(&fixture.paths, &runner, &fixture.request(), 30)
        .expect("retry enrolls the already-durable key");

    assert_eq!(
        report.active_key_id.as_deref(),
        Some(generated_key_id.as_str())
    );
    assert_eq!(runner.keygen_calls(), 1);
    assert_eq!(fs::read_dir(keys).unwrap().count(), 1);
}

#[test]
fn bootstrap_resumes_a_pending_key_after_a_crash_before_key_move() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    runner.fail_keygen.set(true);
    install(&fixture.paths, &runner, &fixture.request(), 10)
        .expect_err("failed key generation leaves bootstrap intent without authority");
    runner.fail_keygen.set(false);

    let public_key = "ssh-ed25519 AAAATESTKEY01";
    let key_id = Digest::of(public_key.as_bytes()).to_string();
    let pending = fixture.paths.state_root.join("private/pending-key");
    assert!(pending.is_dir());
    fs::set_permissions(&pending, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(pending.join("key"), b"PRIVATE KEY 1\n").unwrap();
    fs::set_permissions(pending.join("key"), fs::Permissions::from_mode(0o600)).unwrap();
    fs::write(
        pending.join("key.pub"),
        format!("{public_key} louiselm-launch\n"),
    )
    .unwrap();
    let destination = fixture
        .paths
        .state_root
        .join("private/keys")
        .join(key_id.replace(':', "-"));
    fs::create_dir(&destination).unwrap();
    fs::set_permissions(&destination, fs::Permissions::from_mode(0o700)).unwrap();

    let report = install(&fixture.paths, &runner, &fixture.request(), 20)
        .expect("retry publishes the durable pending key through its empty destination");

    assert_eq!(report.active_key_id.as_deref(), Some(key_id.as_str()));
    assert_eq!(runner.keygen_calls(), 1);
    assert!(!pending.exists());
}

#[test]
fn malformed_config_is_reported_instead_of_panicking() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    install(&fixture.paths, &runner, &fixture.request(), 10).unwrap();
    let config_path = fixture.paths.state_root.join("config.json");
    let mut config: serde_json::Value =
        serde_json::from_slice(&fs::read(&config_path).unwrap()).unwrap();
    config["launcher_digest"] = serde_json::Value::String("not-a-digest".to_owned());
    config["ssh_keygen_path"] = serde_json::Value::String("/should/not/be/read".to_owned());
    config["pool"]["slots"] = serde_json::Value::from(u32::MAX);
    fs::write(&config_path, serde_json::to_vec(&config).unwrap()).unwrap();

    let report = status(&fixture.paths);

    assert!(!report.trusted);
    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.code == "config_invalid")
    );
}

#[test]
fn non_executable_launcher_bytes_are_not_a_trusted_release() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    install(&fixture.paths, &runner, &fixture.request(), 10).unwrap();
    let launcher = fixture
        .paths
        .release_prefix
        .join("current/bin/louiselm-launch");
    fs::set_permissions(&launcher, fs::Permissions::from_mode(0o444)).unwrap();

    let report = status(&fixture.paths);
    let error = acquire_identity(&fixture.paths, 0)
        .expect_err("an identity cannot be leased under unusable launcher bytes");

    assert!(
        report
            .failures
            .iter()
            .any(|failure| failure.code == "release_unusable")
    );
    assert!(error.to_string().contains("regular executable"), "{error}");

    let symlinked = Fixture::new();
    install(
        &symlinked.paths,
        &FakeRunner::default(),
        &symlinked.request(),
        10,
    )
    .unwrap();
    let launcher = symlinked
        .paths
        .release_prefix
        .join("current/bin/louiselm-launch");
    let real_launcher = launcher.with_extension("real");
    fs::rename(&launcher, &real_launcher).unwrap();
    symlink("louiselm-launch.real", &launcher).unwrap();
    assert!(
        status(&symlinked.paths)
            .failures
            .iter()
            .any(|failure| failure.code == "release_unusable"),
        "a symlink is not the measured executable even when its target bytes match"
    );
}

#[test]
fn shadow_locks_reclaim_dead_pids_but_refuse_live_owners() {
    let stale = Fixture::new();
    let runner = FakeRunner::default();
    fs::write(
        PathBuf::from(format!("{}.lock", stale.paths.subuid.display())),
        format!("{}\0", i32::MAX),
    )
    .unwrap();
    install(&stale.paths, &runner, &stale.request(), 10)
        .expect("a dead shadow-utils lock is reclaimed");
    assert!(!PathBuf::from(format!("{}.lock", stale.paths.subuid.display())).exists());

    let live = Fixture::new();
    fs::write(
        PathBuf::from(format!("{}.lock", live.paths.subuid.display())),
        format!("{}\0", std::process::id()),
    )
    .unwrap();
    let error = install(&live.paths, &runner, &live.request(), 10)
        .expect_err("a live shadow-utils lock is never stolen");
    assert!(error.to_string().contains("busy"), "{error}");
    assert!(!live.paths.sudoers.exists());

    let malformed = Fixture::new();
    fs::write(
        PathBuf::from(format!("{}.lock", malformed.paths.subuid.display())),
        b"not-a-pid\0",
    )
    .unwrap();
    let error = install(&malformed.paths, &runner, &malformed.request(), 10)
        .expect_err("a malformed shadow-utils lock is never reclaimed");
    assert!(error.to_string().contains("malformed"), "{error}");
    assert!(!malformed.paths.sudoers.exists());
}

#[test]
fn simultaneous_different_slots_leave_no_shadow_lock_behind() {
    let fixture = Fixture::new();
    let runner = FakeRunner::default();
    install(&fixture.paths, &runner, &fixture.request(), 10).unwrap();
    let paths = Arc::new(fixture.paths.clone());
    let acquired = Arc::new(Barrier::new(3));
    let release = Arc::new(Barrier::new(3));
    let mut threads = Vec::new();
    for slot in [0, 1] {
        let paths = Arc::clone(&paths);
        let acquired = Arc::clone(&acquired);
        let release = Arc::clone(&release);
        threads.push(std::thread::spawn(move || {
            let lease = acquire_identity(&paths, slot).expect("different slot is acquired");
            acquired.wait();
            release.wait();
            lease
        }));
    }
    acquired.wait();
    assert!(!PathBuf::from(format!("{}.lock", paths.subuid.display())).exists());
    assert!(!PathBuf::from(format!("{}.lock", paths.subgid.display())).exists());
    release.wait();
    for thread in threads {
        thread
            .join()
            .expect("lease thread exits")
            .release()
            .expect("proved-empty slot is released");
    }
    acquire_identity(&paths, 0)
        .expect("slot is reusable after both leases release")
        .release()
        .unwrap();
}

#[test]
fn dropping_an_unreleased_lease_fails_closed_across_reacquisition() {
    let fixture = Fixture::new();
    install(
        &fixture.paths,
        &FakeRunner::default(),
        &fixture.request(),
        10,
    )
    .unwrap();

    drop(acquire_identity(&fixture.paths, 3).expect("slot is initially available"));

    assert!(matches!(
        acquire_identity(&fixture.paths, 3),
        Err(louiselm_skills::launcher_install::LauncherError::Poisoned { slot: 3 })
    ));
}

#[test]
fn an_unproven_cleanup_durably_poisons_the_identity_slot() {
    let fixture = Fixture::new();
    install(
        &fixture.paths,
        &FakeRunner::default(),
        &fixture.request(),
        10,
    )
    .unwrap();

    acquire_identity(&fixture.paths, 2)
        .expect("slot is initially available")
        .poison()
        .expect("poison marker is durable");

    assert!(matches!(
        acquire_identity(&fixture.paths, 2),
        Err(louiselm_skills::launcher_install::LauncherError::Poisoned { slot: 2 })
    ));
    let report = status(&fixture.paths);
    assert!(report.failures.iter().any(|failure| {
        failure.code == "identity_slot_poisoned" && failure.detail.contains("slot 2")
    }));
}

#[test]
fn identity_pool_rejects_the_uid_and_gid_sentinel() {
    let uid_error = IdentityPool {
        uid_start: u32::MAX,
        gid_start: 100_000,
        slots: 1,
    }
    .identity(0)
    .expect_err("uid_t -1 is not a usable identity");
    let gid_error = IdentityPool {
        uid_start: 100_000,
        gid_start: u32::MAX,
        slots: 1,
    }
    .identity(0)
    .expect_err("gid_t -1 is not a usable identity");

    assert!(uid_error.to_string().contains("overflows"));
    assert!(gid_error.to_string().contains("overflows"));
}
