//! Real installed daemon and measured Agent relay against a disposable upstream tracker.
use super::*;
use crate::beads_mutation::{
    ApprovedBeadsComments, BeadsMutationKind, BeadsMutationOutcome, BeadsMutationRequest,
};
use crate::launch_protocol::COMMAND_SCHEMA;

const TRACKER_CONFIG: &str = "/etc/louiselm-broker-beads.json";

pub(super) fn subjects() -> &'static [&'static str] {
    if Path::new(TRACKER_CONFIG).exists() {
        &["tracker", "unapproved"]
    } else {
        &["session", "sibling", "failure"]
    }
}

pub(super) fn permission(name: &str, now: u64) -> Option<ApprovedBeadsComments> {
    if name != "tracker" || !Path::new(TRACKER_CONFIG).exists() {
        return None;
    }
    let record: serde_json::Value =
        serde_json::from_slice(&fs::read(TRACKER_CONFIG).unwrap()).unwrap();
    let workspace = Path::new(record["workspace"].as_str().unwrap());
    Some(ApprovedBeadsComments {
        project_digest: Digest::of(workspace.as_os_str().as_encoded_bytes()).to_string(),
        issue_ids: vec![fs::read_to_string(workspace.join("fixture-issue")).unwrap()],
        max_comments: 1,
        expires_at_ms: now + 180_000,
    })
}

fn upstream(program: &Path, workspace: &Path, arguments: &[&str]) -> Vec<u8> {
    let output = Command::new("/usr/bin/setpriv")
        .args([
            "--reuid",
            &BROKER_UID.to_string(),
            "--regid",
            &BROKER_UID.to_string(),
            "--clear-groups",
        ])
        .arg(program)
        .args(arguments)
        .env_clear()
        .current_dir(workspace)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "upstream br {arguments:?}: {output:?}"
    );
    output.stdout
}

fn provision(root: &Path) -> (PathBuf, PathBuf, String) {
    let program = root.join("br");
    fs::copy(
        std::env::var_os("LOUISELM_TEST_BR").unwrap_or_else(|| "/usr/bin/true".into()),
        &program,
    )
    .unwrap();
    fs::set_permissions(&program, fs::Permissions::from_mode(0o555)).unwrap();
    // Private tmpfs supplied by mounts(): broker writes must not depend on the
    // disposable VM disk retaining non-root reserved space.
    let workspace = PathBuf::from("/var/lib/louiselm/beads-project");
    fs::create_dir(&workspace).unwrap();
    chown(&workspace, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    let issue = if std::env::var_os("LOUISELM_TEST_BR").is_some() {
        upstream(
            &program,
            &workspace,
            &[
                "init",
                "--prefix",
                "fixture",
                "--actor",
                "fixture/setup",
                "--json",
            ],
        );
        let created: serde_json::Value = serde_json::from_slice(&upstream(
            &program,
            &workspace,
            &[
                "create",
                "Installed relay target",
                "--actor",
                "fixture/setup",
                "--json",
            ],
        ))
        .unwrap();
        created["id"].as_str().unwrap().to_owned()
    } else {
        // CI exercises installed wiring without acquiring another executable.
        // Real upstream behavior is separately accepted with LOUISELM_TEST_BR.
        fs::create_dir(workspace.join(".beads")).unwrap();
        fs::write(workspace.join(".beads/beads.db"), []).unwrap();
        for path in [workspace.join(".beads"), workspace.join(".beads/beads.db")] {
            chown(&path, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
        }
        "fixture-comment".into()
    };
    fs::write(workspace.join("fixture-issue"), &issue).unwrap();
    chown(&workspace, Some(0), Some(0)).unwrap();
    let installer = std::env::var_os("LOUISELM_TEST_BEADS_INSTALLER").expect("explicit installer");
    let digest = Digest::of(&fs::read(&program).unwrap()).to_string();
    let install = || {
        Command::new("/usr/bin/python3")
            .arg(&installer)
            .arg("--workspace")
            .arg(&workspace)
            .arg("--br")
            .arg(&program)
            .args(["--sha256", digest.strip_prefix("sha256:").unwrap()])
            .output()
            .unwrap()
    };
    let result = install();
    assert!(result.status.success(), "{result:?}");
    let original = fs::read(TRACKER_CONFIG).unwrap();
    assert!(install().status.success());
    assert_eq!(fs::read(TRACKER_CONFIG).unwrap(), original);
    for path in [
        &workspace,
        &workspace.join(".beads"),
        &workspace.join(".beads/beads.db"),
        &program,
        Path::new(TRACKER_CONFIG),
    ] {
        assert!(
            Command::new("/usr/bin/setpriv")
                .args([
                    "--reuid",
                    &AGENT_UID.to_string(),
                    "--regid",
                    &AGENT_UID.to_string(),
                    "--clear-groups",
                    "/usr/bin/test",
                    "!",
                    "-w"
                ])
                .arg(path)
                .status()
                .unwrap()
                .success(),
            "Session can write {path:?}"
        );
    }
    (program, workspace, issue)
}

fn relay(session: LaunchedSession, issue: &str, allowed: bool) {
    let id = session.receipt().payload.session_id.clone();
    let (mut input, controller_input) = std::os::unix::net::UnixStream::pair().unwrap();
    let (controller_output, output) = std::os::unix::net::UnixStream::pair().unwrap();
    output
        .set_read_timeout(Some(Duration::from_secs(40)))
        .unwrap();
    let worker = thread::spawn(move || {
        session.relay_stdio(
            RelayStdio::new(
                BufReader::new(fs::File::from(OwnedFd::from(controller_input))),
                fs::File::from(OwnedFd::from(controller_output)),
            )
            .unwrap(),
        )
    });
    let mut output = BufReader::new(output);
    let query = CommandMessage {
        schema: COMMAND_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: "relay-comment".into(),
        session_id: id,
        run_id: "run".into(),
        envelope_revision: 1,
        operation: CommandOperation::BeadsMutation {
            request: BeadsMutationRequest {
                request_id: "comment-once".into(),
                kind: BeadsMutationKind::CommentAdd {
                    issue_id: issue.into(),
                    text: "--actor=forged\n$(literal)".into(),
                },
            },
        },
    };
    let mut first = None;
    for _ in 0..2 {
        input.write_all(&[0x1e]).unwrap();
        input.write_all(&query.canonical_bytes()).unwrap();
        input.write_all(b"\n").unwrap();
        input.flush().unwrap();
        let mut reply = Vec::new();
        output.read_until(b'\n', &mut reply).unwrap();
        assert_eq!(reply.first(), Some(&0x1e), "{reply:?}");
        let crate::launch_protocol::ProtocolMessage::Command(reply) =
            crate::launch_protocol::decode_message(&reply[1..]).unwrap()
        else {
            panic!("expected command reply")
        };
        if allowed {
            let CommandOperation::BeadsMutationResult { status } = reply.operation else {
                panic!("expected comment success: {reply:?}")
            };
            assert_eq!(status.outcome, BeadsMutationOutcome::Completed);
            if let Some(first) = &first {
                assert_eq!(first, &status);
            }
            first = Some(status);
        } else {
            assert!(matches!(
                reply.operation,
                CommandOperation::BeadsMutationRefused { .. }
            ));
        }
    }
    drop(input);
    worker.join().unwrap().unwrap();
}

fn refuse_bad_configuration(manager: &OwnedFd) {
    let config_bytes = fs::read(TRACKER_CONFIG).unwrap();
    fs::set_permissions(TRACKER_CONFIG, fs::Permissions::from_mode(0o666)).unwrap();
    assert!(
        !process(manager, BROKER_UID, false)
            .0
            .wait()
            .unwrap()
            .success()
    );
    fs::set_permissions(TRACKER_CONFIG, fs::Permissions::from_mode(0o644)).unwrap();
    let mut record: serde_json::Value = serde_json::from_slice(&config_bytes).unwrap();
    record["program_digest"] = Digest::of(b"wrong binary").to_string().into();
    write_json(Path::new(TRACKER_CONFIG), &record);
    assert!(
        !process(manager, BROKER_UID, false)
            .0
            .wait()
            .unwrap()
            .success()
    );
    fs::write(TRACKER_CONFIG, &config_bytes).unwrap();
}

#[test]
fn privileged_installed_tracker_routes_only_approved_comments() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_BEADS").is_none() {
        eprintln!("skipping: requires disposable VM and private mounts");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let root = tempfile::Builder::new()
        .prefix("louiselm-beads-")
        .tempdir_in("/var/lib")
        .unwrap();
    mounts(root.path());
    let _account = BrokerAccount::create();
    let (paths, config, _) = install_fixture_at(root.path(), 5, LauncherPaths::system());
    inspection::provision();
    fs::create_dir(STATE).unwrap();
    chown(STATE, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(STATE, fs::Permissions::from_mode(0o700)).unwrap();
    fs::create_dir(root.path().join("sessions")).unwrap();
    fs::set_permissions(
        root.path().join("sessions"),
        fs::Permissions::from_mode(0o711),
    )
    .unwrap();
    fs::remove_file(TRACKER_CONFIG)
        .or_else(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                Ok(())
            } else {
                Err(error)
            }
        })
        .unwrap();
    let manager = socket_with(
        AddressFamily::UNIX,
        SocketType::SEQPACKET,
        SocketFlags::CLOEXEC,
        None,
    )
    .unwrap();
    rustix::net::sockopt::set_socket_passcred(&manager, true).unwrap();
    bind(
        &manager,
        &SocketAddrUnix::new(&config.broker_socket_path).unwrap(),
    )
    .unwrap();
    listen(&manager, 8).unwrap();
    assert!(
        process(&manager, BROKER_UID, true)
            .0
            .wait()
            .unwrap()
            .success()
    );
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    relay(
        launch(&paths, &config, root.path(), "sibling").unwrap(),
        "fixture-no-grant",
        false,
    );
    terminate(&mut daemon);
    // Seed a fresh installation's approvals only after explicit provisioning.
    let (program, workspace, issue) = provision(root.path());
    refuse_bad_configuration(&manager);
    assert!(
        process(&manager, BROKER_UID, true)
            .0
            .wait()
            .unwrap()
            .success()
    );
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    relay(
        launch(&paths, &config, root.path(), "tracker").unwrap(),
        &issue,
        true,
    );
    relay(
        launch(&paths, &config, root.path(), "unapproved").unwrap(),
        &issue,
        false,
    );
    assert_eq!(
        fs::read_dir(Path::new(STATE).join("authorizations/beads-mutations/outcomes"))
            .unwrap()
            .count(),
        1
    );
    if std::env::var_os("LOUISELM_TEST_BR").is_some() {
        let comments: serde_json::Value = serde_json::from_slice(&upstream(
            &program,
            &workspace,
            &["comments", "list", &issue, "--json"],
        ))
        .unwrap();
        assert_eq!(comments.as_array().unwrap().len(), 1);
        assert_eq!(comments[0]["author"], "agent/tracker");
        assert_eq!(comments[0]["text"], "--actor=forged\n$(literal)");
    }
    terminate(&mut daemon);
}
