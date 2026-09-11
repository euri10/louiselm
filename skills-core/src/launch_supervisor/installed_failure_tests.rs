//! Faults at both real signing/durability boundaries of the installed composition.

use super::*;
use crate::launch_receipt::{ReceiptOutcome, ReceiptPayload};

struct FaultSigner {
    signer: InstalledLaunchSigner,
    scenario: &'static str,
    root: PathBuf,
}

impl LaunchSigner for FaultSigner {
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
