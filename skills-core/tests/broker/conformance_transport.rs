//! Exact observations travel on the authenticated launch transaction before ACK.

use super::*;
use louiselm_skills::conformance::{
    Check, Cleanup, Outcome, REPORT_SCHEMA, REQUIRED_CHECKS, Report, Scope,
};
use louiselm_skills::{
    launch_supervisor::{SupervisorCompletion, SupervisorError, connect_control_broker},
    launcher_install::{CONFIG_SCHEMA, LauncherConfig},
};

// Only the socket and credential pins are consumed by this transport fixture;
// this is not an installed-host authority or a certification fixture.
fn transport_config(socket: &Path) -> LauncherConfig {
    LauncherConfig {
        conformance: louiselm_skills::conformance::admission::Enforcement::PreCutover,
        schema: CONFIG_SCHEMA.into(),
        operator: "fixture".into(),
        operator_uid: CONTROLLER_UID,
        broker_uid: getuid().as_raw(),
        broker_gid: getgid().as_raw(),
        broker_socket_path: socket.into(),
        release_id: Digest::of(b"release").to_string(),
        launcher_digest: Digest::of(b"launcher").to_string(),
        launcher_path: "/unused/launcher".into(),
        ssh_keygen_path: "/unused/ssh-keygen".into(),
        ssh_keygen_digest: Digest::of(b"ssh-keygen").to_string(),
        getent_path: "/unused/getent".into(),
        getent_digest: Digest::of(b"getent").to_string(),
        bwrap_path: "/unused/bwrap".into(),
        bwrap_digest: Digest::of(b"bwrap").to_string(),
        pool: pool(4),
    }
}

fn supervisor_result<T: Send + 'static>(
    start: impl FnOnce(SupervisorCompletion<T>) -> Result<(), SupervisorError>,
) -> T {
    let (sender, receiver) = mpsc::sync_channel(1);
    start(Box::new(move |result| sender.send(result).unwrap())).unwrap();
    receiver
        .recv_timeout(Duration::from_secs(3))
        .unwrap()
        .unwrap()
}

fn report_receipt(authorization: &LaunchAuthorization, bytes: &[u8]) -> SignedReceipt {
    let mut launch = launch_receipt(authorization);
    let ReceiptOutcome::Launch { evidence, .. } = &mut launch.payload.outcome else {
        panic!("launch fixture");
    };
    evidence.conformance = ConformanceEvidence::Certified {
        report_digest: Digest::of(bytes).to_string(),
    };
    signed(launch.payload)
}

fn service(root: &Path, request: &LaunchRequest) -> BrokerService {
    let authorizations = AuthorizationStore::open(&root.join("authorizations"), pool(4)).unwrap();
    authorizations.authorize(&grant(request), 1000).unwrap();
    BrokerService::bind(
        &root.join("control.sock"),
        authorizations,
        ReceiptStore::open(&root.join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap()
}

fn report_bytes() -> Vec<u8> {
    Report {
        schema: REPORT_SCHEMA.into(),
        scope: Scope::InstalledHost,
        checks: REQUIRED_CHECKS
            .iter()
            .map(|name| Check {
                name: (*name).into(),
                control: Outcome::Allowed,
                // Escaping makes this valid report larger than one packet.
                confined: Outcome::Denied("\u{0001}".repeat(384)),
            })
            .collect(),
        completed: true,
        cleanup: Cleanup::Confirmed,
    }
    .canonical_bytes()
    .unwrap()
}

#[derive(serde::Serialize)]
struct WireChunk<'a> {
    schema: &'a str,
    receipt_digest: String,
    offset: usize,
    total_bytes: usize,
    bytes: &'a [u8],
}

fn report_packets(receipt: &SignedReceipt, bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes
        .chunks(8192)
        .enumerate()
        .map(|(index, chunk)| {
            serde_json::to_vec(&WireChunk {
                schema: "louiselm.launch.conformance-report-chunk/1",
                receipt_digest: receipt.digest().to_string(),
                offset: index * 8192,
                total_bytes: bytes.len(),
                bytes: chunk,
            })
            .unwrap()
        })
        .collect()
}

#[test]
fn authorization_explicitly_retains_conformance_policy() {
    let root = TempDir::new().unwrap();
    let request = request("conformance-policy");
    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
    let pending = authorizations.authorize(&grant(&request), 1000).unwrap();
    let expected = serde_json::json!({"attendance": "unattended", "waiver": null});
    assert_eq!(
        serde_json::to_value(pending).unwrap()["conformance"],
        expected
    );
    let consumed = authorizations
        .consume(&request, CONTROLLER_UID, 2000)
        .unwrap();
    assert_eq!(
        serde_json::to_value(consumed).unwrap()["conformance"],
        expected
    );
}

#[test]
fn conformance_waiver_expiry_survives_authorization_restart() {
    use louiselm_skills::{
        conformance::admission::{Attendance, Condition},
        launch_protocol::ConformanceWaiver,
    };
    let root = TempDir::new().unwrap();
    let request = request("expiring-waiver");
    let mut grant = grant(&request);
    grant.conformance.attendance = Attendance::Interactive;
    grant.conformance.waiver = Some(ConformanceWaiver {
        session_id: request.session_id.clone(),
        request_digest: request.digest().to_string(),
        operator_uid: CONTROLLER_UID,
        condition: Condition::Missing,
        expires_at_ms: 2000,
        receipt_digest: Digest::of(b"durable-waiver-receipt").to_string(),
    });
    let path = root.path().join("authorizations");
    let store = AuthorizationStore::open(&path, pool(4)).unwrap();
    assert!(store.authorize(&grant, 2000).is_err());
    let pending = store.authorize(&grant, 1000).unwrap();
    assert_eq!(pending.conformance, grant.conformance);
    drop(store);
    let store = AuthorizationStore::open(&path, pool(4)).unwrap();
    assert!(store.consume(&request, CONTROLLER_UID, 2000).is_err());
    assert!(
        store
            .consumed_for_session(&request.session_id)
            .unwrap()
            .is_none()
    );
    // An expired waiver neither grants a Session nor renews on restart. The
    // durable pending bytes retain the original approval for inspection.
    assert_eq!(
        serde_json::to_value(pending).unwrap()["conformance"]["waiver"]["expires_at_ms"],
        2000
    );
}

#[test]
fn signed_waiver_without_matching_broker_approval_cannot_receive_an_ack() {
    let root = TempDir::new().unwrap();
    let authorization = consumed_authorization(root.path(), &request("unapproved-waiver"));
    let receipts = ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap();
    let mut receipt = launch_receipt(&authorization);
    let ReceiptOutcome::Launch { evidence, .. } = &mut receipt.payload.outcome else {
        panic!("launch fixture");
    };
    evidence.conformance = ConformanceEvidence::Waived {
        condition: louiselm_skills::conformance::admission::Condition::Missing,
        report_digest: None,
    };
    assert!(
        receipts
            .append(
                &authorization,
                &signed(receipt.payload).canonical_bytes(),
                None,
                verify_fixture_signature,
            )
            .is_err(),
        "a supervisor signature cannot manufacture operator waiver authority"
    );
}

#[test]
fn certified_report_crosses_authenticated_launch_transaction() {
    let root = TempDir::new().unwrap();
    let request = request("report-transport");
    let service = service(root.path(), &request);
    let socket = root.path().join("control.sock");
    let bytes = report_bytes();
    assert!(bytes.len() > louiselm_skills::launch_transport::MAX_PACKET_BYTES);
    let expected = bytes.clone();
    let supervisor = thread::spawn(move || {
        let broker =
            connect_control_broker(&transport_config(&socket), Duration::from_secs(3)).unwrap();
        let authorization = supervisor_result(|done| broker.consume_authorization(request, done));
        let launch = report_receipt(&authorization, &bytes);
        let ack = supervisor_result(|done| {
            broker.append_receipt(launch.canonical_bytes(), Some(bytes), done)
        });
        assert_eq!(ack.sequence, 0);
        assert_eq!(ack.receipt_digest, launch.digest().to_string());
        let start = start_receipt(&authorization, &launch);
        let ack =
            supervisor_result(|done| broker.append_receipt(start.canonical_bytes(), None, done));
        assert_eq!(ack.sequence, 1);
        (authorization, launch, start, broker)
    });
    let result = service.serve_launch(2000, verify_fixture_signature);
    let (authorization, launch, start, broker) = supervisor.join().unwrap();
    let session = result.unwrap();
    let reopened = ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap();
    assert_eq!(
        reopened
            .conformance_report(&authorization, verify_fixture_signature)
            .unwrap(),
        Some(expected)
    );
    assert_eq!(
        reopened.stored_bytes(&authorization.session_id).unwrap(),
        vec![launch.canonical_bytes(), start.canonical_bytes()]
    );
    session.close();
    broker.close();
}

fn damaged_transfer(case: &str, launch: &SignedReceipt, bytes: &[u8]) -> Vec<Vec<u8>> {
    let mut packets = report_packets(launch, bytes);
    match case {
        "missing" => packets.clear(),
        "truncated" => packets.truncate(1),
        "replayed" => packets.insert(1, packets[0].clone()),
        "foreign" | "changed-total" | "changed-bytes" => {
            let mut chunk: louiselm_skills::launch_protocol::ConformanceReportChunk =
                serde_json::from_slice(&packets[1]).unwrap();
            match case {
                "foreign" => chunk.receipt_digest = Digest::of(b"foreign-receipt").to_string(),
                "changed-total" => chunk.total_bytes += 1,
                "changed-bytes" => chunk.bytes[0] ^= 1,
                _ => unreachable!(),
            }
            packets[1] = chunk.canonical_bytes().unwrap();
        }
        "storage" => {}
        _ => unreachable!(),
    }
    packets
}

#[test]
fn missing_foreign_replayed_truncated_or_unstored_reports_never_receive_an_ack() {
    for case in [
        "missing",
        "truncated",
        "replayed",
        "foreign",
        "changed-total",
        "changed-bytes",
        "storage",
    ] {
        let root = TempDir::new().unwrap();
        let request = request("refused-report");
        let service = service(root.path(), &request);
        if case == "storage" {
            fs::write(
                root.path().join("receipts/conformance"),
                b"owned obstruction",
            )
            .unwrap();
        }
        let socket = root.path().join("control.sock");
        let supervisor = thread::spawn(move || {
            let (authorization, channel) = supervisor_authorization(&socket, &request, 2000);
            let bytes = report_bytes();
            let launch = report_receipt(&authorization, &bytes);
            settle(|done| channel.send(launch.canonical_bytes(), done));
            for packet in damaged_transfer(case, &launch, &bytes) {
                let (sender, receiver) = mpsc::sync_channel(1);
                if channel
                    .send(
                        packet,
                        Box::new(move |result| {
                            let _ = sender.send(result);
                        }),
                    )
                    .is_err()
                    || receiver
                        .recv_timeout(Duration::from_secs(3))
                        .unwrap()
                        .is_err()
                {
                    break;
                }
            }
            if matches!(case, "missing" | "truncated") {
                // Peer cancellation wakes the bounded receive without waiting
                // out its deadline, and cannot publish a partial report.
                channel.close();
            } else {
                let (sender, receiver) = mpsc::sync_channel(1);
                if channel
                    .receive(Box::new(move |result| sender.send(result).unwrap()))
                    .is_ok()
                    && let Ok(packet) = receiver.recv_timeout(Duration::from_secs(3)).unwrap()
                {
                    assert!(
                        !matches!(
                            packet.packet,
                            LauncherPacket::Request(ProtocolMessage::ReceiptAcknowledgement(_))
                        ),
                        "{case}"
                    );
                }
                channel.close();
            }
            authorization
        });
        assert!(
            service
                .serve_launch(2000, verify_fixture_signature)
                .is_err(),
            "{case}"
        );
        let authorization = supervisor.join().unwrap();
        let receipts =
            ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap();
        assert!(
            receipts.head(&authorization.session_id).unwrap().is_none(),
            "{case}"
        );
        assert!(
            !root
                .path()
                .join("receipts/conformance/refused-report.json")
                .exists(),
            "{case}"
        );
    }
}
