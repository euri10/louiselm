//! Disposable-VM composition of the installed Brokered launch handoff.

use super::*;
use crate::conformance::{
    ReportResult,
    admission::Enforcement,
    installed::{certify, measure},
};
use crate::{
    broker::{
        BrokerSession, lifecycle::LifecycleCaller, provider_credentials::ProviderCredentialStore,
    },
    launch_protocol::{LIFECYCLE_REQUEST_SCHEMA, LifecycleAction, LifecycleRequest},
    launch_receipt::SessionState,
};
use openssl::{
    asn1::{Asn1Integer, Asn1Time},
    bn::BigNum,
    hash::MessageDigest,
    pkey::{PKey, Private},
    rsa::Rsa,
    ssl::{NameType, SslAcceptor, SslMethod},
    x509::{
        X509, X509NameBuilder,
        extension::{BasicConstraints, KeyUsage, SubjectAlternativeName},
    },
};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener},
};

pub(super) fn approval(now_ms: u64) -> crate::provider_request::ApprovedProviderRequests {
    crate::provider_request::ApprovedProviderRequests {
        disclosure_profile: crate::provider_request::disclosure::profile_digest(),
        provider: "openai".into(),
        upstream: "https://api.openai.com/v1/responses".into(),
        addresses: vec!["192.0.2.1".parse().unwrap()],
        max_run_requests: 1,
        models: vec!["fixture-model".into()],
        max_effort: crate::provider_request::ReasoningEffort::Low,
        expires_at_ms: now_ms + 120_000,
    }
}

fn signed_certificate(
    common_name: &str,
    key: &PKey<Private>,
    issuer: Option<(&X509, &PKey<Private>)>,
    serial: u32,
) -> X509 {
    let mut name = X509NameBuilder::new().unwrap();
    name.append_entry_by_text("CN", common_name).unwrap();
    let name = name.build();
    let mut certificate = X509::builder().unwrap();
    certificate.set_version(2).unwrap();
    let serial = Asn1Integer::from_bn(&BigNum::from_u32(serial).unwrap()).unwrap();
    certificate.set_serial_number(&serial).unwrap();
    certificate.set_subject_name(&name).unwrap();
    certificate.set_pubkey(key).unwrap();
    certificate
        .set_not_before(&Asn1Time::days_from_now(0).unwrap())
        .unwrap();
    certificate
        .set_not_after(&Asn1Time::days_from_now(1).unwrap())
        .unwrap();
    if let Some((parent, parent_key)) = issuer {
        certificate.set_issuer_name(parent.subject_name()).unwrap();
        certificate
            .append_extension(BasicConstraints::new().critical().build().unwrap())
            .unwrap();
        let san = SubjectAlternativeName::new()
            .dns(common_name)
            .build(&certificate.x509v3_context(Some(parent), None))
            .unwrap();
        certificate.append_extension(san).unwrap();
        certificate
            .sign(parent_key, MessageDigest::sha256())
            .unwrap();
    } else {
        certificate.set_issuer_name(&name).unwrap();
        certificate
            .append_extension(BasicConstraints::new().critical().ca().build().unwrap())
            .unwrap();
        certificate
            .append_extension(
                KeyUsage::new()
                    .critical()
                    .key_cert_sign()
                    .crl_sign()
                    .build()
                    .unwrap(),
            )
            .unwrap();
        certificate.sign(key, MessageDigest::sha256()).unwrap();
    }
    certificate.build()
}

fn start_provider_upstream(root: &std::path::Path) -> (u16, thread::JoinHandle<()>) {
    let root_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let root_certificate = signed_certificate("LouiseLM test root", &root_key, None, 1);
    fs::write(
        root.join("guard-provider-root.pem"),
        root_certificate.to_pem().unwrap(),
    )
    .unwrap();
    let server_key = PKey::from_rsa(Rsa::generate(2048).unwrap()).unwrap();
    let server_certificate = signed_certificate(
        "api.openai.com",
        &server_key,
        Some((&root_certificate, &root_key)),
        2,
    );
    let mut builder = SslAcceptor::mozilla_intermediate(SslMethod::tls()).unwrap();
    builder.set_private_key(&server_key).unwrap();
    builder.set_certificate(&server_certificate).unwrap();
    builder.check_private_key().unwrap();
    let acceptor = builder.build();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    listener.set_nonblocking(true).unwrap();
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(20);
        for _ in 0..2 {
            let stream = loop {
                assert!(Instant::now() < deadline, "upstream was not reached");
                match listener.accept() {
                    Ok((stream, _)) => break stream,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(error) => panic!("upstream accept: {error}"),
                }
            };
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut tls = acceptor.accept(stream).unwrap();
            assert_eq!(
                tls.ssl().servername(NameType::HOST_NAME),
                Some("api.openai.com")
            );
            let mut request = Vec::new();
            loop {
                let mut buffer = [0; 4096];
                let count = tls.read(&mut buffer).unwrap();
                assert!(count > 0, "upstream request ended before its body");
                request.extend_from_slice(&buffer[..count]);
                if let Some(header_end) = request.windows(4).position(|bytes| bytes == b"\r\n\r\n")
                {
                    let header =
                        String::from_utf8_lossy(&request[..header_end]).to_ascii_lowercase();
                    let length = header
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length: "))
                        .unwrap()
                        .parse::<usize>()
                        .unwrap();
                    if request.len() >= header_end + 4 + length {
                        assert!(header.contains("authorization: bearer fixture-secret"));
                        assert!(
                            request[header_end + 4..header_end + 4 + length]
                                .windows(b"fixture-model".len())
                                .any(|bytes| bytes == b"fixture-model")
                        );
                        break;
                    }
                }
            }
            let first = b"event: first\n\n";
            let last = b"event: last\n\n";
            write!(
                tls,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                first.len() + last.len(),
            )
            .unwrap();
            tls.write_all(first).unwrap();
            tls.flush().unwrap();
            thread::sleep(Duration::from_millis(20));
            tls.write_all(last).unwrap();
            tls.flush().unwrap();
        }
    });
    (port, worker)
}

pub(super) fn park_and_dispose(broker: &InstalledBroker, session: &mut BrokerSession, uid: u32) {
    let caller = LifecycleCaller::Operator { uid };
    let request = |request_id: &str, action, state, sequence| LifecycleRequest {
        schema: LIFECYCLE_REQUEST_SCHEMA.into(),
        protocol_version: PROTOCOL_VERSION,
        request_id: request_id.into(),
        session_id: "session".into(),
        run_id: "run".into(),
        authorization_id: format!("operator-{request_id}"),
        action,
        expected_state: state,
        expected_receipt_sequence: Some(sequence),
        envelope_revision: 1,
    };
    broker
        .request_lifecycle(
            session,
            &caller,
            &request(
                "guard-park",
                LifecycleAction::Park,
                SessionState::Running,
                1,
            ),
        )
        .unwrap();
    while broker.inspect("session").unwrap().unwrap().state != SessionState::Parked {
        assert!(!broker.step(session).unwrap());
    }
    println!("BROKER_PARKED");
    assert!(
        broker
            .request_lifecycle(
                session,
                &caller,
                &request(
                    "guard-stale-resume",
                    LifecycleAction::Resume,
                    SessionState::Parked,
                    2,
                ),
            )
            .is_err()
    );
    broker
        .request_lifecycle(
            session,
            &caller,
            &request(
                "guard-dispose",
                LifecycleAction::Disposal,
                SessionState::Parked,
                2,
            ),
        )
        .unwrap();
    assert_eq!(
        broker.inspect("session").unwrap().unwrap().state,
        SessionState::Terminal
    );
    println!("BROKER_TERMINAL");
}

#[test]
fn privileged_installed_brokered_guard_start_and_disposal() {
    installed_guard_case(false, false, false, false);
}

#[test]
fn privileged_installed_brokered_guard_lost_close_ack_poison() {
    installed_guard_case(true, false, false, false);
}

#[test]
fn privileged_installed_brokered_guard_park_closes_before_receipt() {
    installed_guard_case(false, true, false, false);
}

#[test]
fn privileged_installed_brokered_guard_broker_outage_poison() {
    installed_guard_case(false, false, true, false);
}

#[test]
fn privileged_installed_brokered_provider_stream() {
    installed_guard_case(false, true, false, true);
}

#[expect(
    clippy::too_many_lines,
    reason = "One disposable fixture owns the certificate, non-root broker, launch and terminal containment."
)]
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "Independent fault flags select one installed fixture case."
)]
fn installed_guard_case(hold_close_ack: bool, park: bool, broker_outage: bool, provider: bool) {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_GUARD").is_none() {
        eprintln!("requires disposable root and a current installed guard certificate");
        return;
    }
    assert!(rustix::process::geteuid().is_root());
    let _account = BrokerAccount::create();
    let root = tempfile::Builder::new()
        .prefix("louiselm-broker-guard-")
        .tempdir_in("/var/lib")
        .unwrap();
    let (paths, mut config, registry_root) = install_fixture_with_slots(root.path(), 3);
    fs::write(root.path().join("brokered"), b"").unwrap();
    let tls_server = provider.then(|| start_provider_upstream(root.path()));
    if let Some((port, _)) = &tls_server {
        super::provider_credentials::provision_empty_state(root.path());
        fs::write(root.path().join("guard-provider-port"), port.to_string()).unwrap();
        let custody = ProviderCredentialStore::root_in(&root.path().join("state"));
        chown(&custody, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
        fs::set_permissions(&custody, fs::Permissions::from_mode(0o700)).unwrap();
        let credential = custody.join("openai");
        fs::write(&credential, b"fixture-secret").unwrap();
        chown(&credential, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
        fs::set_permissions(&credential, fs::Permissions::from_mode(0o600)).unwrap();
    }
    if hold_close_ack {
        fs::write(root.path().join("guard-close-no-ack"), b"").unwrap();
    }
    if park {
        fs::write(root.path().join("guard-park"), b"").unwrap();
    }
    registry_record(
        &registry_root.join("envelopes.json"),
        &serde_json::json!([{
            "id":"envelope","network":"brokered","description":"guarded fixture"
        }]),
    );
    config.conformance = Enforcement::Enforced;
    write_json(&paths.state_root.join("config.json"), &config);
    write_json(&paths.state_root.join("public-config.json"), &config);
    let parent = rustix::process::getppid()
        .unwrap()
        .as_raw_nonzero()
        .get()
        .cast_unsigned();
    measure(&paths, &config, Instant::now() + Duration::from_mins(3)).unwrap();
    let certificate = certify(&paths, Instant::now() + Duration::from_mins(3), parent).unwrap();
    assert_eq!(
        certificate.observations.result().unwrap(),
        ReportResult::Passed,
        "{:?}",
        certificate.observations
    );

    let (mut broker_child, lines) = broker_process(root.path(), None);
    marker(&lines, "BROKER_READY");
    let mut platform =
        SystemLaunchPlatform::new(paths.clone(), config.clone(), Duration::from_secs(5)).unwrap();
    platform.registry_root = registry_root.clone();
    platform.guarded_start_allowed = true;
    let sessions = root.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o711)).unwrap();
    let supervisor = LaunchSupervisor::new(
        connect_control_broker(&config, Duration::from_secs(5)).unwrap(),
        Arc::new(InstalledLaunchSigner::open(&paths, Duration::from_secs(5)).unwrap()),
        Arc::new(platform),
        Arc::new(Registry::open_trusted(&registry_root).unwrap()),
        sessions,
        Duration::from_secs(5),
    );
    let (sent, result) = mpsc::channel();
    let now_ms = u64::try_from(
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
            now_ms,
            Box::new(move |outcome| sent.send(outcome).unwrap()),
        )
        .unwrap();
    let session = result
        .recv_timeout(Duration::from_secs(30))
        .unwrap()
        .unwrap();
    marker(&lines, "BROKER_GUARD_ACKED");
    let agent_pid: u32 = marker(&lines, "BROKER_RUNNING ")
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse()
        .unwrap();
    let provider_address = provider.then(|| {
        marker(&lines, "BROKER_PROVIDER_ADDR ")
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse::<SocketAddr>()
            .unwrap()
    });
    if hold_close_ack {
        marker(&lines, "BROKER_GUARD_HELD");
    }
    if broker_outage {
        broker_child.0.kill().unwrap();
        assert!(!broker_child.0.wait().unwrap().success());
    }
    let (mut controller_peer_input, controller_input) =
        std::os::unix::net::UnixStream::pair().unwrap();
    let (controller_output, mut controller_peer_output) =
        std::os::unix::net::UnixStream::pair().unwrap();
    let (done, finished) = mpsc::channel();
    thread::spawn(move || {
        let relay = RelayStdio::new(
            BufReader::new(fs::File::from(OwnedFd::from(controller_input))),
            fs::File::from(OwnedFd::from(controller_output)),
        )
        .unwrap();
        let _ = done.send(session.relay_stdio(relay));
    });
    if let Some(address) = provider_address {
        controller_peer_output
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let invalid_model = provider_exchange(
            &mut controller_peer_input,
            &mut controller_peer_output,
            address,
            "bad-model",
        );
        assert!(
            invalid_model.contains("capability_denied"),
            "{invalid_model}"
        );
        let invalid_disclosure = provider_exchange(
            &mut controller_peer_input,
            &mut controller_peer_output,
            address,
            "bad-disclosure",
        );
        assert!(
            invalid_disclosure.contains("provider_disclosure_denied"),
            "{invalid_disclosure}"
        );
        let answer = provider_exchange(
            &mut controller_peer_input,
            &mut controller_peer_output,
            address,
            "valid-batch",
        );
        assert_eq!(answer.matches("HTTP/1.1 200 OK").count(), 2, "{answer}");
        assert_eq!(answer.matches("event: first").count(), 2, "{answer}");
        assert_eq!(answer.matches("event: last").count(), 2, "{answer}");
        let exhausted = provider_exchange(
            &mut controller_peer_input,
            &mut controller_peer_output,
            address,
            "valid-once",
        );
        assert!(
            exhausted.contains("HTTP/1.1 429 Too Many Requests"),
            "{exhausted}"
        );
        assert!(exhausted.contains("capability_denied"), "{exhausted}");
        fs::write(root.path().join("guard-provider-done"), b"").unwrap();
        tls_server.unwrap().1.join().unwrap();
    }
    if !park {
        rustix::process::kill_process(
            rustix::process::Pid::from_raw(i32::try_from(agent_pid).unwrap()).unwrap(),
            rustix::process::Signal::TERM,
        )
        .unwrap();
    }
    let outcome = finished.recv_timeout(Duration::from_secs(20)).unwrap();
    if hold_close_ack || broker_outage {
        assert_eq!(outcome.err(), Some(SupervisorError::CleanupUnproven));
        if hold_close_ack {
            marker(&lines, "BROKER_NO_TERMINAL");
        }
    } else {
        outcome.unwrap();
        if park {
            marker(&lines, "BROKER_PARKED");
        }
        marker(&lines, "BROKER_TERMINAL");
    }
    if !broker_outage {
        assert!(broker_child.0.wait().unwrap().success());
    }
    if hold_close_ack || broker_outage {
        assert!(matches!(
            crate::launcher_install::acquire_identity(&paths, 0),
            Err(crate::launcher_install::LauncherError::Poisoned { slot: 0 })
        ));
    } else {
        crate::launcher_install::acquire_identity(&paths, 0)
            .unwrap()
            .release()
            .unwrap();
    }
}

fn provider_exchange(
    input: &mut std::os::unix::net::UnixStream,
    output: &mut std::os::unix::net::UnixStream,
    address: SocketAddr,
    variant: &str,
) -> String {
    writeln!(input, "\x1b{address} {variant}").unwrap();
    input.flush().unwrap();
    let mut marker = [0];
    output.read_exact(&mut marker).unwrap();
    assert_eq!(marker, [0x1b]);
    let mut length = [0; 4];
    output.read_exact(&mut length).unwrap();
    let length = u32::from_be_bytes(length) as usize;
    assert!(length <= 64 * 1024);
    let mut answer = vec![0; length];
    output.read_exact(&mut answer).unwrap();
    String::from_utf8(answer).unwrap()
}
