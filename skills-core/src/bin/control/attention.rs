//! Process-owned outbox delivery, independent of Session workers.

use louiselm_skills::broker::{BrokerError, InstalledBroker, attention::AttentionEndpoint};
use rustix::fs::{Mode, OFlags, open};
use std::{
    fs::File,
    io::{self, Read},
    os::unix::fs::MetadataExt,
    path::Path,
    sync::Arc,
    thread::{self, JoinHandle},
    time::Duration,
};

const CONFIG: &str = "/etc/louiselm-broker-attention.json";
const POLL: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(30);
const CONFIG_BYTES: u64 = 4096;

pub(super) fn start(broker: Arc<InstalledBroker>) -> Result<JoinHandle<()>, BrokerError> {
    thread::Builder::new()
        .name("louiselm-broker-attention".into())
        .spawn(move || {
            let mut retry = POLL;
            let mut unavailable = false;
            // This worker and its bounded transport worker belong to the daemon
            // process. Native SIGTERM closes both immediately, even mid-read.
            // Only deliver_attention can persist an exact receiver ACK.
            loop {
                let result = read_endpoint(Path::new(CONFIG))
                    .map_err(BrokerError::Attention)
                    .and_then(|endpoint| broker.deliver_attention(&endpoint));
                match result {
                    Ok(delivered) => {
                        if unavailable && delivered {
                            eprintln!("louiselm-control: Attention delivery available");
                            unavailable = false;
                        }
                        retry = POLL;
                        if !delivered {
                            thread::sleep(POLL);
                        }
                    }
                    Err(error) => {
                        if !unavailable {
                            // Do not render transport/parser errors or endpoint
                            // contents: peer input may contain sensitive data.
                            eprintln!("louiselm-control: Attention delivery unavailable ({error}); queued entries retained; check endpoint configuration, receiver and outbox storage");
                            unavailable = true;
                        }
                        thread::sleep(retry);
                        retry = (retry * 2).min(MAX_RETRY);
                    }
                }
            }
        })
        .map_err(BrokerError::Attention)
}

fn read_endpoint(path: &Path) -> io::Result<AttentionEndpoint> {
    // Fixed /etc parent is outside broker/Session write authority. Open the
    // leaf without following links or blocking on a substituted FIFO, then
    // validate the same descriptor we read.
    let fd = open(
        path,
        OFlags::RDONLY | OFlags::CLOEXEC | OFlags::NOFOLLOW | OFlags::NONBLOCK,
        Mode::empty(),
    )?;
    let file = File::from(fd);
    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.uid() != 0
        || metadata.mode() & 0o022 != 0
        || metadata.len() > CONFIG_BYTES
    {
        return Err(invalid_config());
    }
    let mut bytes = Vec::new();
    file.take(CONFIG_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() > usize::try_from(CONFIG_BYTES).map_err(|_| invalid_config())? {
        return Err(invalid_config());
    }
    parse_endpoint(&bytes)
}

fn parse_endpoint(bytes: &[u8]) -> io::Result<AttentionEndpoint> {
    let endpoint: AttentionEndpoint =
        serde_json::from_slice(bytes).map_err(|_| invalid_config())?;
    if !endpoint.socket.is_absolute() || !endpoint.capability_file.is_absolute() {
        return Err(invalid_config());
    }
    Ok(endpoint)
}

fn invalid_config() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "invalid Attention endpoint configuration",
    )
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "Fixtures assert endpoint configuration boundaries."
)]
mod tests {
    use super::*;
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };

    #[test]
    fn endpoint_requires_exact_fields_and_absolute_paths() {
        let record = serde_json::json!({
            "socket": "/run/capture/attention.sock",
            "capability_file": "/var/lib/louiselm/broker/attention-capability",
            "receiver_uid": 1000
        });
        let parse = |value: &serde_json::Value| parse_endpoint(&serde_json::to_vec(value).unwrap());
        assert_eq!(parse(&record).unwrap().receiver_uid, 1000);
        for field in ["socket", "capability_file", "receiver_uid"] {
            let mut missing = record.clone();
            missing.as_object_mut().unwrap().remove(field);
            assert!(parse(&missing).is_err());
        }
        for field in ["socket", "capability_file"] {
            let mut relative = record.clone();
            relative[field] = "relative".into();
            assert!(parse(&relative).is_err());
        }
        let mut unknown = record.clone();
        unknown["token"] = "must-not-be-inline".into();
        assert!(parse(&unknown).is_err());
        let mut invalid_uid = record;
        invalid_uid["receiver_uid"] = (-1).into();
        assert!(parse(&invalid_uid).is_err());
    }

    #[test]
    fn configuration_refuses_missing_link_nonregular_and_untrusted_files() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("endpoint.json");
        assert!(read_endpoint(&path).is_err());
        assert!(read_endpoint(root.path()).is_err());
        rustix::fs::mkfifoat(rustix::fs::CWD, &path, Mode::RUSR | Mode::WUSR).unwrap();
        assert!(read_endpoint(&path).is_err());
        fs::remove_file(&path).unwrap();
        let target = root.path().join("target");
        fs::write(&target, b"{}").unwrap();
        symlink(&target, &path).unwrap();
        assert!(read_endpoint(&path).is_err());
        fs::remove_file(&path).unwrap();
        fs::write(&path, vec![b' '; 4097]).unwrap();
        assert!(read_endpoint(&path).is_err());
        fs::write(
            &path,
            br#"{"socket":"/socket","capability_file":"/capability","receiver_uid":1000}"#,
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        assert!(read_endpoint(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        assert_eq!(
            read_endpoint(&path).is_ok(),
            rustix::process::geteuid().is_root()
        );
    }
}
