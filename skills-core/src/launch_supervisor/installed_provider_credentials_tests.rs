//! Credential custody through the installed broker and a real confined Session.

use super::*;
use crate::{
    broker::{BrokerError, provider_credentials::ProviderCredentialStore},
    launch_protocol::ErrorCode,
};

const CREDENTIAL_WORKER: &str =
    "launch_supervisor::system::installed_tests::provider_credentials::credential_worker";

#[test]
fn privileged_installed_provider_credentials_stay_broker_side() {
    installed_broker_effects(false, None, None, true);
}

#[test]
fn credential_worker() {
    let Some(root) = std::env::var_os("LOUISELM_CREDENTIAL_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    let result = InstalledBroker::bind(&paths(&root), &root.join("state"));
    if std::env::var_os("LOUISELM_CREDENTIAL_INVALID").is_some() {
        let Err(BrokerError::Policy(error)) = result else {
            panic!("malformed custody must refuse installed startup with a protocol error");
        };
        assert_eq!(error.code, ErrorCode::CredentialUnavailable);
        error.validate().unwrap();
        println!("{}", serde_json::to_string(&error).unwrap());
    } else {
        let broker = result.unwrap();
        assert_eq!(
            broker.provider_credential("acme").unwrap_err().code,
            ErrorCode::InvalidRequest
        );
        let directory = ProviderCredentialStore::root_in(&root.join("state"));
        let metadata = fs::metadata(directory).unwrap();
        assert_eq!(
            (metadata.uid(), metadata.gid(), metadata.mode() & 0o7777),
            (BROKER_UID, BROKER_UID, 0o700)
        );
    }
}

fn startup(root: &Path, invalid: bool) {
    let mut command = Command::new("/usr/bin/setpriv");
    command
        .args([
            "--reuid",
            &BROKER_UID.to_string(),
            "--regid",
            &BROKER_UID.to_string(),
            "--clear-groups",
        ])
        .arg(std::env::current_exe().unwrap())
        .args([CREDENTIAL_WORKER, "--exact", "--nocapture"])
        .env_clear()
        .env("PATH", "/usr/bin:/bin")
        .env("LOUISELM_CREDENTIAL_FIXTURE", root);
    if invalid {
        command.env("LOUISELM_CREDENTIAL_INVALID", "1");
    }
    let output = command.output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    if invalid {
        let secret =
            fs::read(ProviderCredentialStore::root_in(&root.join("state")).join("acme")).unwrap();
        if !secret.is_empty() {
            absent(&output.stdout, &secret);
            absent(&output.stderr, &secret);
        }
    }
    let socket = paths(root).broker_socket;
    if socket.exists() {
        fs::remove_file(socket).unwrap();
    }
}

pub(super) fn prepare(root: &Path) {
    // The first real startup provisions empty custody; no Provider is required.
    startup(root, false);
    let directory = ProviderCredentialStore::root_in(&root.join("state"));
    let credential = directory.join("acme");
    let mut entropy = [0_u8; 32];
    rustix::rand::getrandom(&mut entropy, rustix::rand::GetRandomFlags::empty()).unwrap();
    let secret = format!("fixture-{}", Digest::of(&entropy).hex());
    fs::write(&credential, &secret).unwrap();
    chown(&credential, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o640)).unwrap();
    startup(root, true);
    fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
    chown(&credential, Some(AGENT_UID), Some(AGENT_UID)).unwrap();
    startup(root, true);
    chown(&credential, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o750)).unwrap();
    startup(root, true);
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    fs::write(root.join("credential-custody"), b"").unwrap();
}

fn absent(bytes: &[u8], secret: &[u8]) {
    assert!(
        !bytes.windows(secret.len()).any(|window| window == secret),
        "credential escaped custody"
    );
}

fn scan(directory: &Path, secret: &[u8]) {
    for entry in fs::read_dir(directory).unwrap() {
        let entry = entry.unwrap();
        let kind = entry.file_type().unwrap();
        if kind.is_dir() {
            scan(&entry.path(), secret);
        } else if kind.is_file() {
            absent(&fs::read(entry.path()).unwrap(), secret);
        }
    }
}

pub(super) fn assert_session_surfaces(root: &Path, agent_pid: u32, broker_pid: u32) {
    let directory = ProviderCredentialStore::root_in(&root.join("state"));
    let credential = directory.join("acme");
    let secret = fs::read(&credential).unwrap();
    for pid in [agent_pid, broker_pid] {
        for name in ["environ", "cmdline"] {
            absent(&fs::read(format!("/proc/{pid}/{name}")).unwrap(), &secret);
        }
    }
    // A distinct host UID has more filesystem reach than the confined Agent.
    // Even there, direct paths and the broker's process state remain unreadable.
    for path in [
        credential,
        directory,
        PathBuf::from(format!("/proc/{broker_pid}/environ")),
    ] {
        let output = Command::new("/usr/bin/setpriv")
            .args([
                "--reuid",
                &AGENT_UID.to_string(),
                "--regid",
                &AGENT_UID.to_string(),
                "--clear-groups",
            ])
            .args(["/usr/bin/test", "-r"])
            .arg(path)
            .env_clear()
            .output()
            .unwrap();
        assert!(!output.status.success());
        absent(&output.stdout, &secret);
        absent(&output.stderr, &secret);
    }
    scan(&root.join("sessions"), &secret);
    scan(&root.join("registry"), &secret);
}

pub(super) fn assert_records(root: &Path) {
    let custody = ProviderCredentialStore::root_in(&root.join("state"));
    let secret = fs::read(custody.join("acme")).unwrap();
    for entry in fs::read_dir(root.join("state")).unwrap() {
        let entry = entry.unwrap();
        if entry.path() == custody {
            continue;
        }
        if entry.file_type().unwrap().is_dir() {
            scan(&entry.path(), &secret);
        } else if entry.file_type().unwrap().is_file() {
            absent(&fs::read(entry.path()).unwrap(), &secret);
        }
    }
    scan(&root.join("sessions"), &secret);
}
