//! Actual root export -> dedicated broker -> non-root operator checkout writes.

use super::*;
use crate::{
    broker::{
        promotion::{PromotionRequest, PromotionStatus, Reply},
        verification::VerificationRecord,
    },
    workspace::promotion::{DestinationIdentity, PromotionClient, transfer},
};
use std::os::unix::{
    fs::MetadataExt,
    net::{UnixListener, UnixStream},
};

const OPERATOR: &str = "launch_supervisor::system::installed_tests::verification::promotion::installed_promotion_operator";
const ATTEMPTS: usize = 9;

pub(super) fn prepare(root: &Path, uid: u32) {
    let channel = root.join("promotion-channel");
    fs::create_dir(&channel).unwrap();
    chown(&channel, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
    fs::set_permissions(&channel, fs::Permissions::from_mode(0o711)).unwrap();
    for directory in ["operator-checkout", "operator-journal"] {
        let path = root.join(directory);
        fs::create_dir(&path).unwrap();
        chown(&path, Some(uid), Some(uid)).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let git = root.join("operator-checkout/.git");
    fs::create_dir_all(git.join("hooks")).unwrap();
    let sentinel = format!("#!/bin/sh\ntouch {}/HOOK_EXECUTED\n", root.display());
    fs::write(git.join("hooks/post-checkout"), sentinel).unwrap();
    fs::set_permissions(
        git.join("hooks/post-checkout"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
}

pub(super) fn broker_round(broker: &InstalledBroker, producer: &mut BrokerSession, root: &Path) {
    let socket = root.join("promotion-channel/socket");
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).unwrap();
    println!("PROMOTION_READY");
    for attempt in 0..ATTEMPTS {
        if attempt == 8 {
            crate::broker::lifecycle::LifecycleStore::open(
                &root.join("state/authorizations/lifecycle"),
            )
            .unwrap()
            .quarantine("verifier-0")
            .unwrap();
        }
        let (stream, _) = listener.accept().unwrap();
        let result = broker.serve_promotion(producer, stream);
        if (6..=7).contains(&attempt) {
            assert!(matches!(result.unwrap(), PromotionStatus::Completed { .. }));
        } else {
            assert!(
                result.is_err(),
                "invalid selection or changed checkout must refuse: {attempt}"
            );
        }
    }
    assert!(matches!(
        broker.promotion_status("promote-good").unwrap(),
        PromotionStatus::Completed { .. }
    ));
}

pub(super) fn operator_round(root: &Path, uid: u32) {
    let record: VerificationRecord = serde_json::from_slice(
        &fs::read(root.join("state/authorizations/verification/result-verifier-0.json")).unwrap(),
    )
    .unwrap();
    write_json(&root.join("promotion-evidence.json"), &record);
    fs::set_permissions(
        root.join("promotion-evidence.json"),
        fs::Permissions::from_mode(0o444),
    )
    .unwrap();
    assert!(
        Command::new("/usr/bin/setpriv")
            .args([
                "--reuid",
                &uid.to_string(),
                "--regid",
                &uid.to_string(),
                "--clear-groups"
            ])
            .arg(std::env::current_exe().unwrap())
            .args([OPERATOR, "--exact", "--nocapture"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LOUISELM_PROMOTION_FIXTURE", root)
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(
        fs::read(root.join("operator-checkout/effect")).unwrap(),
        b"authorized"
    );
    assert_eq!(
        fs::metadata(root.join("operator-checkout/effect"))
            .unwrap()
            .uid(),
        uid
    );
    assert!(!root.join("HOOK_EXECUTED").exists());
    assert!(
        root.join("operator-checkout/.git/hooks/post-checkout")
            .exists()
    );
    let readable = root.join("sessions/session/promotion-transfers");
    assert!(readable.is_dir());
    for transfer in fs::read_dir(readable).unwrap() {
        let mode = transfer.unwrap().metadata().unwrap().mode();
        assert_eq!(mode & 0o022, 0, "broker cannot modify transferred bytes");
    }
}

pub(super) fn broker_denial(
    broker: &InstalledBroker,
    producer: &mut BrokerSession,
    root: &Path,
    index: usize,
) {
    let socket = root.join(format!("promotion-channel/socket-{index}"));
    let listener = UnixListener::bind(&socket).unwrap();
    fs::set_permissions(&socket, fs::Permissions::from_mode(0o666)).unwrap();
    println!("PROMOTION_DENIAL_{index}");
    let (stream, _) = listener.accept().unwrap();
    assert!(matches!(
        broker.serve_promotion(producer, stream),
        Err(crate::broker::BrokerError::RequestMismatch)
    ));
}

pub(super) fn operator_denial(root: &Path, uid: u32, index: usize) {
    let source = root.join(format!(
        "state/authorizations/verification/result-verifier-{index}.json"
    ));
    let destination = root.join(format!("denied-evidence-{index}.json"));
    fs::copy(source, &destination).unwrap();
    fs::set_permissions(destination, fs::Permissions::from_mode(0o444)).unwrap();
    assert!(
        Command::new("/usr/bin/setpriv")
            .args([
                "--reuid",
                &uid.to_string(),
                "--regid",
                &uid.to_string(),
                "--clear-groups"
            ])
            .arg(std::env::current_exe().unwrap())
            .args([OPERATOR, "--exact", "--nocapture"])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("LOUISELM_PROMOTION_FIXTURE", root)
            .env("LOUISELM_PROMOTION_DENIED", index.to_string())
            .status()
            .unwrap()
            .success()
    );
}

fn denied_client(root: &Path, index: usize) {
    let record: VerificationRecord = serde_json::from_slice(
        &fs::read(root.join(format!("denied-evidence-{index}.json"))).unwrap(),
    )
    .unwrap();
    assert!(!record.execution.commands_passed());
    let journal = root.join("operator-journal");
    let checkout = journal.join(format!("denied-checkout-{index}"));
    fs::create_dir(&checkout).unwrap();
    fs::set_permissions(&checkout, fs::Permissions::from_mode(0o700)).unwrap();
    let request = PromotionRequest {
        schema: "louiselm.workspace.promotion/1".into(),
        request_id: format!("denied-{index}"),
        verifier_session_id: format!("verifier-{index}"),
        verification_digest: Digest::of(&serde_json::to_vec(&record).unwrap()).to_string(),
        job: record.execution.job,
        destination: DestinationIdentity::inspect(&checkout).unwrap(),
        expires_at_ms: clock_ms() + 60000,
    };
    let stream =
        UnixStream::connect(root.join(format!("promotion-channel/socket-{index}"))).unwrap();
    assert!(PromotionClient::prepare(stream, BROKER_UID, request, &checkout, &journal).is_err());
    assert!(!checkout.join("effect").exists());
}

#[test]
fn installed_promotion_operator() {
    let Some(root) = std::env::var_os("LOUISELM_PROMOTION_FIXTURE") else {
        return;
    };
    let root = PathBuf::from(root);
    if let Ok(index) = std::env::var("LOUISELM_PROMOTION_DENIED") {
        denied_client(&root, index.parse().unwrap());
        return;
    }
    let record: VerificationRecord =
        serde_json::from_slice(&fs::read(root.join("promotion-evidence.json")).unwrap()).unwrap();
    let checkout = root.join("operator-checkout");
    let journal = root.join("operator-journal");
    let request = PromotionRequest {
        schema: "louiselm.workspace.promotion/1".into(),
        request_id: "promote-good".into(),
        verifier_session_id: "verifier-0".into(),
        verification_digest: Digest::of(&serde_json::to_vec(&record).unwrap()).to_string(),
        job: record.execution.job,
        destination: DestinationIdentity::inspect(&checkout).unwrap(),
        expires_at_ms: clock_ms() + 120_000,
    };
    for attempt in 0..6 {
        let mut refused = request.clone();
        refused.request_id = format!("refused-{attempt}");
        match attempt {
            0 => refused.verification_digest = Digest::of(b"forged").to_string(),
            1 => refused.job.base_digest = Digest::of(b"changed-base").to_string(),
            2 => refused.job.plan_digest = Digest::of(b"changed-plan").to_string(),
            3 => refused.expires_at_ms = 1,
            4 => refused.job.result_digest = Digest::of(b"changed-result").to_string(),
            _ => (),
        }
        let stream = UnixStream::connect(root.join("promotion-channel/socket")).unwrap();
        let client = PromotionClient::prepare(stream, BROKER_UID, refused, &checkout, &journal);
        if attempt == 5 {
            let client = client.unwrap();
            fs::write(checkout.join("local-edit"), b"preserve").unwrap();
            assert!(client.commit().is_err());
            assert_eq!(fs::read(checkout.join("local-edit")).unwrap(), b"preserve");
            fs::remove_file(checkout.join("local-edit")).unwrap();
        } else {
            assert!(client.is_err());
        }
        assert!(!checkout.join("effect").exists());
    }
    let stream = UnixStream::connect(root.join("promotion-channel/socket")).unwrap();
    let client =
        PromotionClient::prepare(stream, BROKER_UID, request.clone(), &checkout, &journal).unwrap();
    assert_eq!(client.preview().added, ["effect"]);
    assert!(
        !checkout.join("effect").exists(),
        "preview alone grants no write"
    );
    assert!(client.commit().unwrap().complete);
    // The same operation returns only its historical result, never a second effect.
    let mut stream = UnixStream::connect(root.join("promotion-channel/socket")).unwrap();
    transfer::send(&mut stream, &request).unwrap();
    assert!(matches!(
        transfer::receive::<Reply>(&mut stream).unwrap(),
        Reply::Finished {
            status: PromotionStatus::Completed { .. }
        }
    ));
    let second = journal.join("second-checkout");
    fs::create_dir(&second).unwrap();
    fs::set_permissions(&second, fs::Permissions::from_mode(0o700)).unwrap();
    let mut quarantined = request;
    quarantined.request_id = "quarantined".into();
    quarantined.destination = DestinationIdentity::inspect(&second).unwrap();
    let stream = UnixStream::connect(root.join("promotion-channel/socket")).unwrap();
    assert!(PromotionClient::prepare(stream, BROKER_UID, quarantined, &second, &journal).is_err());
    assert!(!second.join("effect").exists());
}
