//! Offline upgrade and state adoption retain exact history under installed authority.

use super::*;

#[test]
fn privileged_activated_daemon_upgrades_and_adopts_state() {
    if std::env::var_os("LOUISELM_REQUIRE_CONTROL_DAEMON").is_none() {
        eprintln!("skipping: requires disposable VM and private mount namespace");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let started = Instant::now();
    let root = tempfile::Builder::new()
        .prefix("louiselm-daemon-state-")
        .tempdir_in("/var/lib")
        .unwrap();
    mounts(root.path());
    let _account = BrokerAccount::create();
    let (paths, config, manager) = install_daemon(root.path());
    let mut daemon = process(&manager, BROKER_UID, false);
    ready(&config);
    let session = launch(&paths, &config, root.path(), "session").unwrap();
    let retired = session.receipt().payload.signing_key_id.clone();
    let original = session.receipt().canonical_bytes();
    crate::launcher_install::rotate(
        &paths,
        &SystemCommandRunner,
        &crate::launcher_install::RotationRequest {
            rotation_id: "before-upgrade".into(),
            expected_active_key_id: retired.clone(),
        },
        4000,
    )
    .unwrap();
    session.dispose().unwrap();
    terminate(&mut daemon);
    assert!(
        !paths
            .state_root
            .join("private/keys")
            .join(Digest::parse(&retired).unwrap().directory_name())
            .join("key")
            .exists(),
        "upgrade must verify history without the retired private key"
    );
    eprintln!("daemon: retired history at {:?}", started.elapsed());
    // Upgrade and restart validate exact old history using only public authority.
    super::super::receipt_history::upgrade(&paths, &config);
    let verifier =
        crate::launcher_install::LauncherVerifier::open(&paths, Path::new(STATE)).unwrap();
    let receipt = crate::launch_receipt::SignedReceipt::parse_canonical(&original).unwrap();
    verifier
        .verify(
            &retired,
            &receipt.payload.canonical_bytes(),
            &receipt.signature,
        )
        .unwrap();
    let mut after_cleanup = process(&manager, BROKER_UID, false);
    ready(&config);
    inspection::refusal(
        config.operator_uid,
        "session",
        crate::broker::operator::InspectError::StatusUnavailable,
    );
    terminate(&mut after_cleanup);
    inspection::refusal(
        config.operator_uid,
        "session",
        crate::broker::operator::InspectError::BrokerUnavailable,
    );
    fs::set_permissions(STATE, fs::Permissions::from_mode(0o750)).unwrap();
    assert!(
        !process(&manager, BROKER_UID, false)
            .0
            .wait()
            .unwrap()
            .success()
    );
    fs::set_permissions(STATE, fs::Permissions::from_mode(0o700)).unwrap();
    let marker = Path::new(STATE).join("identity.json");
    let mut identity: serde_json::Value =
        serde_json::from_slice(&fs::read(&marker).unwrap()).unwrap();
    identity["uid"] = (BROKER_UID + 1).into();
    write_json(&marker, &identity);
    assert!(
        !process(&manager, BROKER_UID, false)
            .0
            .wait()
            .unwrap()
            .success()
    );
    eprintln!("daemon: upgrade and refusals at {:?}", started.elapsed());
    adoption(&manager, &config, &original, &marker);
    eprintln!("daemon: adoption complete at {:?}", started.elapsed());
}

fn adopt_command(operator_uid: Option<u32>, arguments: &[&str]) -> std::process::Output {
    let mut command = Command::new("/usr/bin/setpriv");
    command
        .args([
            "--reuid",
            &BROKER_UID.to_string(),
            "--regid",
            &BROKER_UID.to_string(),
            "--clear-groups",
        ])
        .arg("/usr/local/lib/louiselm/current/bin/louiselm-control")
        .args(arguments)
        .env_clear();
    if let Some(uid) = operator_uid {
        command.env("SUDO_UID", uid.to_string());
    }
    command.output().unwrap()
}

fn adoption(manager: &OwnedFd, config: &LauncherConfig, receipt: &[u8], marker: &Path) {
    let before = fs::read(marker).unwrap();
    for (operator, arguments) in [
        (None, vec!["adopt-state", "--confirm"]),
        (
            Some(config.operator_uid + 1),
            vec!["adopt-state", "--confirm"],
        ),
        (Some(config.operator_uid), vec!["adopt-state"]),
    ] {
        let refused = adopt_command(operator, &arguments);
        assert!(!refused.status.success(), "{refused:?}");
        assert_eq!(fs::read(marker).unwrap(), before);
    }
    // Exercise sudo's actual caller attribution and dedicated-account switch.
    // Give only this disposable fixture the exact cross-account permission.
    // The product's fixed run/certify policy grants no adoption permission.
    let sudoers = Path::new("/etc/sudoers.d/adoption-fixture");
    fs::write(sudoers, format!(
        "#{} ALL=(#{BROKER_UID}:#{BROKER_UID}) NOPASSWD: NOSETENV: /usr/local/lib/louiselm/current/bin/louiselm-control adopt-state --confirm\n",
        config.operator_uid,
    )).unwrap();
    fs::set_permissions(sudoers, fs::Permissions::from_mode(0o440)).unwrap();
    assert!(
        Command::new("/usr/sbin/visudo")
            .args(["-c", "-f"])
            .arg(sudoers)
            .status()
            .unwrap()
            .success()
    );
    let adopted = Command::new("/usr/bin/sudo")
        .args([
            "-n",
            "-u",
            &format!("#{}", config.operator_uid),
            "/usr/bin/sudo",
            "-n",
            "-u",
            &format!("#{BROKER_UID}"),
            "/usr/local/lib/louiselm/current/bin/louiselm-control",
            "adopt-state",
            "--confirm",
        ])
        .env_clear()
        .output()
        .unwrap();
    assert!(adopted.status.success(), "{adopted:?}");
    assert_eq!(adopted.stdout, b"broker state identity adopted\n");
    let identity: serde_json::Value = serde_json::from_slice(&fs::read(marker).unwrap()).unwrap();
    assert_eq!(identity["uid"], BROKER_UID);
    let before = fs::metadata(marker).unwrap();
    let unchanged = adopt_command(Some(config.operator_uid), &["adopt-state", "--confirm"]);
    assert!(unchanged.status.success(), "{unchanged:?}");
    assert_eq!(
        unchanged.stdout,
        b"broker state identity already matches; unchanged\n"
    );
    assert_eq!(fs::metadata(marker).unwrap().ino(), before.ino());
    let mut daemon = process(manager, BROKER_UID, false);
    ready(config);
    let busy = adopt_command(Some(config.operator_uid), &["adopt-state", "--confirm"]);
    assert!(!busy.status.success());
    assert!(
        String::from_utf8(busy.stderr)
            .unwrap()
            .contains("state is in use")
    );
    terminate(&mut daemon);
    assert_eq!(
        fs::read(
            Path::new(STATE).join("receipts/sessions/session/00000000000000000001.receipt.json")
        )
        .unwrap(),
        receipt
    );
    let entries: Vec<_> = fs::read_dir(Path::new(STATE).join("identity-adoptions"))
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(entries.len(), 1);
    let entry: serde_json::Value = serde_json::from_slice(&fs::read(&entries[0]).unwrap()).unwrap();
    assert_eq!(entry["decision"]["operator_uid"], config.operator_uid);
    assert_eq!(entry["decision"]["previous_uid"], BROKER_UID + 1);
    assert_eq!(entry["decision"]["new_uid"], BROKER_UID);
}
