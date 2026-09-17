//! Faults at both real signing/durability boundaries of the installed composition.

use super::*;
use crate::launch_receipt::{ReceiptOutcome, ReceiptPayload};

#[test]
fn production_prepare_rejects_bubblewrap_changed_after_runtime_config_before_spawn() {
    if std::env::var_os("LOUISELM_TEST_ROOT_LAUNCHER").is_none() {
        eprintln!("skipping: measured backend rejection requires the disposable launcher VM");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    assert!(
        fs::read_to_string("/proc/self/uid_map")
            .unwrap()
            .split_whitespace()
            .eq(["0", "0", "4294967295"])
    );
    let _account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-bwrap-change-")
        .tempdir_in("/var/lib")
        .unwrap();
    let mut fixture_paths = paths(root.path());
    fixture_paths.bwrap = root.path().join("bwrap");
    fs::copy("/usr/bin/bwrap", &fixture_paths.bwrap).unwrap();
    fs::set_permissions(&fixture_paths.bwrap, fs::Permissions::from_mode(0o755)).unwrap();
    let (paths, config, registry_root) = install_fixture_at(root.path(), 1, fixture_paths);
    let mut platform =
        SystemLaunchPlatform::new(paths.clone(), config, Duration::from_secs(5)).unwrap();
    platform.registry_root = registry_root.clone();
    let sessions = root.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o711)).unwrap();
    let request = request();
    let plan = crate::launch::resolve(
        &request,
        &Registry::open_trusted(&registry_root).unwrap(),
        &sessions,
        crate::sandbox::IdentityPlan::HostIdentity {
            uid: AGENT_UID,
            gid: AGENT_UID,
        },
    )
    .unwrap()
    .plan;
    let marker_directory = root.path().join("mutation-marker");
    fs::create_dir(&marker_directory).unwrap();
    chown(&marker_directory, Some(AGENT_UID), Some(AGENT_UID)).unwrap();
    let marker = marker_directory.join("spawned");
    fs::write(
        &paths.bwrap,
        format!(
            "#!/bin/sh\nprintf spawned > '{}'\nexit 97\n",
            marker.display()
        ),
    )
    .unwrap();

    // Valid staged inputs must reach the measured-backend check, not fail
    // earlier during workspace resolution (louiselm-8f2wa).
    let result = platform.prepare(&request, plan);
    assert!(
        !marker.exists(),
        "a changed measured backend must be rejected before execution"
    );
    assert_eq!(result.err(), Some(SupervisorError::SpawnFailed));
    let session_root = sessions.join(&request.session_id);
    assert!(session_root.join("workspace").is_dir());
    assert_eq!(
        fs::metadata(session_root).unwrap().mode() & 0o7777,
        0o700,
        "failed preparation seals the materialized workspace"
    );
}

struct FaultSigner {
    signer: InstalledLaunchSigner,
    scenario: &'static str,
    root: PathBuf,
}

impl LaunchSigner for FaultSigner {
    fn complete_session(
        &self,
        terminal: ReceiptPayload,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        self.signer.complete_session(terminal, complete)
    }
    fn check_authority(&self, complete: SupervisorCompletion<()>) -> Result<(), SupervisorError> {
        self.signer.check_authority(complete)
    }
    fn record_containment(
        &self,
        session_id: String,
        containment: crate::launcher_install::KeyContainment,
        complete: SupervisorCompletion<()>,
    ) -> Result<(), SupervisorError> {
        self.signer
            .record_containment(session_id, containment, complete)
    }
    fn release_id(&self) -> &str {
        self.signer.release_id()
    }
    fn signing_key_id(&self) -> &str {
        self.signer.signing_key_id()
    }
    fn sign(
        &self,
        payload: Vec<u8>,
        complete: SupervisorCompletion<String>,
    ) -> Result<(), SupervisorError> {
        let mut receipt = ReceiptPayload::parse_canonical(&payload).unwrap();
        let at = u64::from(!self.scenario.ends_with('0'));
        if receipt.sequence == at {
            if self.scenario.starts_with("signer") {
                complete(Err(SupervisorError::SigningUnavailable));
                return Ok(());
            }
            if self.scenario.starts_with("storage") {
                let directory = self.root.join("state/receipts/sessions");
                let directory = if at == 0 {
                    directory
                } else {
                    directory.join("session")
                };
                fs::set_permissions(directory, fs::Permissions::from_mode(0o500)).unwrap();
            }
            if self.scenario == "signature0" {
                let ReceiptOutcome::Launch { evidence, .. } = &mut receipt.outcome else {
                    panic!("Launch");
                };
                evidence.kernel_identity = "altered-signing-input".into();
                return self.signer.sign(receipt.canonical_bytes(), complete);
            }
        }
        self.signer.sign(payload, complete)
    }
}

#[test]
fn privileged_installed_launch_failures_never_acknowledge_success() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_LAUNCH").is_none() {
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    assert!(
        fs::read_to_string("/proc/self/uid_map")
            .unwrap()
            .split_whitespace()
            .eq(["0", "0", "4294967295"])
    );
    let _account = BrokerAccount::create();
    for scenario in ["signer0", "signer1", "storage0", "storage1", "signature0"] {
        failure_case(scenario);
        eprintln!("installed launch fault: {scenario} passed");
    }
}

fn failure_case(scenario: &'static str) {
    let root = tempfile::Builder::new()
        .prefix("louiselm-broker-fault-")
        .tempdir_in("/var/lib")
        .unwrap();
    let (paths, config, registry_root) = install_fixture(root.path());
    let (mut broker_process, lines) = broker_process(root.path(), Some(scenario));
    marker(&lines, "BROKER_READY");
    let mut platform =
        SystemLaunchPlatform::new(paths.clone(), config.clone(), Duration::from_secs(5)).unwrap();
    platform.registry_root = registry_root.clone();
    let sessions = root.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o711)).unwrap();
    let signer = FaultSigner {
        signer: InstalledLaunchSigner::open(&paths, Duration::from_secs(5)).unwrap(),
        scenario,
        root: root.path().to_owned(),
    };
    let supervisor = LaunchSupervisor::new(
        connect_control_broker(&config, Duration::from_secs(5)).unwrap(),
        Arc::new(signer),
        Arc::new(platform),
        Arc::new(Registry::open_trusted(&registry_root).unwrap()),
        sessions.clone(),
        Duration::from_secs(5),
    );
    let (tx, rx) = mpsc::channel();
    let now = u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis(),
    )
    .unwrap();
    supervisor
        .launch(
            request(),
            config.operator_uid,
            now,
            Box::new(move |result| {
                tx.send(result).unwrap();
            }),
        )
        .unwrap();
    let failed = rx
        .recv_timeout(Duration::from_secs(20))
        .unwrap()
        .err()
        .unwrap();
    assert_eq!(
        failed,
        if scenario.starts_with("signer") {
            SupervisorError::SigningUnavailable
        } else {
            SupervisorError::DurabilityUnavailable
        }
    );
    marker(&lines, "BROKER_REJECTED");
    assert!(broker_process.0.wait().unwrap().success());
    assert!(!sessions.join("session/workspace/effect").exists());
    assert!(
        !Path::new(SYSTEM_CGROUP_ROOT)
            .join("louiselm-session-session")
            .exists()
    );
    crate::launcher_install::acquire_identity(&paths, 0)
        .unwrap()
        .release()
        .unwrap();
}
