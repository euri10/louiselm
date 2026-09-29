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
    net::{IpAddr, SocketAddr, TcpListener},
};

const LIVE_OPENAI_KEY: &str = "/root/louiselm-live-openai.key";

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

#[expect(
    clippy::too_many_lines,
    reason = "installed provider fixture keeps the wire assertions together"
)]
fn start_provider_upstream(root: &std::path::Path, stock: bool) -> (u16, thread::JoinHandle<()>) {
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
        for request_index in 0..2 {
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
                        if stock {
                            let body: serde_json::Value = serde_json::from_slice(
                                &request[header_end + 4..header_end + 4 + length],
                            )
                            .unwrap();
                            assert_eq!(body["model"], "gpt-5.6-luna");
                            assert_eq!(body["reasoning"]["effort"], "low");
                            if request_index == 0 {
                                assert!(
                                    body["input"]
                                        .as_array()
                                        .unwrap()
                                        .iter()
                                        .all(|item| { item["type"] != "custom_tool_call_output" })
                                );
                            } else {
                                let output = body["input"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .find(|item| item["type"] == "custom_tool_call_output")
                                    .expect("Codex did not return the exec result");
                                assert_eq!(output["call_id"], "call_tool");
                                assert!(output.to_string().contains("TOOL_OK"), "{output}");
                            }
                        } else {
                            assert!(
                                request[header_end + 4..header_end + 4 + length]
                                    .windows(b"fixture-model".len())
                                    .any(|bytes| bytes == b"fixture-model")
                            );
                        }
                        break;
                    }
                }
            }
            let response = if stock {
                stock_response(request_index == 0)
            } else {
                b"event: first\n\nevent: last\n\n".to_vec()
            };
            write!(
                tls,
                "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                response.len(),
            )
            .unwrap();
            tls.write_all(&response).unwrap();
            tls.flush().unwrap();
        }
    });
    (port, worker)
}

fn stock_response(first: bool) -> Vec<u8> {
    if first {
        return custom_tool_response();
    }
    let item = serde_json::json!({"type":"message","id":"msg_stock","role":"assistant","status":"completed","content":[{"type":"output_text","text":"OFFLINE_STOCK_OK","annotations":[]}]});
    let events = [
        serde_json::json!({"type":"response.created","response":{"id":"resp_stock"}}),
        serde_json::json!({"type":"response.output_item.added","output_index":0,"item":item}),
        serde_json::json!({"type":"response.output_item.done","output_index":0,"item":item}),
        serde_json::json!({"type":"response.completed","response":{"id":"resp_stock","status":"completed","output":[item],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
    ];
    let mut response = String::new();
    for event in events {
        use std::fmt::Write as _;
        write!(
            response,
            "event: {}\ndata: {}\n\n",
            event["type"].as_str().unwrap(),
            event
        )
        .unwrap();
    }
    response.into_bytes()
}

fn custom_tool_response() -> Vec<u8> {
    let item = serde_json::json!({
        "type":"custom_tool_call",
        "id":"item_tool",
        "call_id":"call_tool",
        "name":"exec",
        "status":"completed",
        "input":"text((await tools.exec_command({cmd: 'printf TOOL_OK'})).output)"
    });
    let events = [
        serde_json::json!({"type":"response.created","response":{"id":"resp_tool"}}),
        serde_json::json!({"type":"response.output_item.added","output_index":0,"item":item}),
        serde_json::json!({"type":"response.output_item.done","output_index":0,"item":item}),
        serde_json::json!({"type":"response.completed","response":{"id":"resp_tool","status":"completed","output":[item],"usage":{"input_tokens":1,"output_tokens":1,"total_tokens":2}}}),
    ];
    let mut response = String::new();
    for event in events {
        use std::fmt::Write as _;
        write!(
            response,
            "event: {}\ndata: {}\n\n",
            event["type"].as_str().unwrap(),
            event
        )
        .unwrap();
    }
    response.into_bytes()
}

pub(super) fn park_and_dispose(
    broker: &InstalledBroker,
    session: &mut BrokerSession,
    uid: u32,
    already_parked: bool,
) {
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
    if !already_parked {
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
    }
    while broker.inspect("session").unwrap().unwrap().state != SessionState::Parked {
        assert!(!broker.step(session).unwrap());
    }
    if !already_parked {
        println!("BROKER_PARKED");
    }
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
    installed_guard_case(false, false, false, false, false);
}

#[test]
fn privileged_installed_brokered_guard_lost_close_ack_poison() {
    installed_guard_case(true, false, false, false, false);
}

#[test]
fn privileged_installed_brokered_guard_park_closes_before_receipt() {
    installed_guard_case(false, true, false, false, false);
}

#[test]
fn privileged_installed_brokered_guard_broker_outage_poison() {
    installed_guard_case(false, false, true, false, false);
}

#[test]
fn privileged_installed_brokered_provider_stream() {
    installed_guard_case(false, true, false, true, false);
}

#[test]
fn privileged_installed_brokered_codex_chain_enrolls_descendant() {
    installed_guard_case_for(false, true, false, true, false, CodexCase::Fixture);
}

#[test]
fn privileged_installed_brokered_codex_missing_descendant_times_out() {
    installed_guard_case_for(false, false, false, false, false, CodexCase::Missing);
}

#[test]
fn privileged_installed_brokered_codex_wrong_ancestry_refuses() {
    installed_guard_case_for(false, false, false, false, false, CodexCase::WrongAncestry);
}

#[test]
fn privileged_installed_brokered_stock_codex_completes_prompt() {
    if std::env::var_os("LOUISELM_REQUIRE_BROKER_GUARD").is_none() {
        eprintln!("requires disposable root and the pinned stock chain");
        return;
    }
    let stock = std::env::var_os("LOUISELM_STOCK_CODEX_DIR")
        .expect("CI must provide the hash-pinned stock Codex chain");
    installed_guard_case_for(
        false,
        true,
        false,
        true,
        false,
        CodexCase::Stock(Path::new(&stock)),
    );
}

#[test]
#[ignore = "requires a dedicated hard-capped API project and private VM key"]
fn privileged_installed_brokered_stock_codex_real_openai() {
    let address = live_openai_preconditions();
    let stock = std::env::var_os("LOUISELM_STOCK_CODEX_DIR")
        .expect("provide the hash-pinned stock Codex chain");
    installed_guard_case_for(
        false,
        true,
        false,
        true,
        false,
        CodexCase::Live(Path::new(&stock), address),
    );
}

#[test]
#[ignore = "requires a dedicated hard-capped API project and private VM key"]
fn privileged_installed_brokered_real_openai_refusals_precede_upstream() {
    let address = live_openai_preconditions();
    installed_guard_case_for(
        false,
        true,
        false,
        true,
        false,
        CodexCase::LiveFixture(address),
    );
}

/// Explicit opt-in, the resolved Provider address and a private guest key.
fn live_openai_preconditions() -> IpAddr {
    assert_eq!(
        std::env::var("LOUISELM_REQUIRE_LIVE_OPENAI").as_deref(),
        Ok("1"),
        "live API calls require an explicit opt-in"
    );
    assert!(std::env::var_os("LOUISELM_REQUIRE_BROKER_GUARD").is_some());
    let address: IpAddr = std::env::var("LOUISELM_LIVE_OPENAI_IP")
        .expect("provide the resolved OpenAI address")
        .parse()
        .expect("OpenAI address must be an IP literal");
    assert!(!address.is_loopback() && !address.is_unspecified());
    let key = fs::symlink_metadata(LIVE_OPENAI_KEY).expect("provision the private guest key");
    assert!(
        key.is_file()
            && key.uid() == 0
            && key.mode() & 0o7777 == 0o600
            && key.nlink() == 1
            && (1..=16 * 1024).contains(&key.len())
    );
    address
}

#[test]
fn privileged_installed_brokered_without_provider_permission_refuses() {
    installed_guard_case(false, false, false, false, true);
}

#[expect(
    clippy::fn_params_excessive_bools,
    reason = "Independent fault flags select one installed fixture case."
)]
fn installed_guard_case(
    hold_close_ack: bool,
    park: bool,
    broker_outage: bool,
    provider: bool,
    no_permission: bool,
) {
    installed_guard_case_for(
        hold_close_ack,
        park,
        broker_outage,
        provider,
        no_permission,
        CodexCase::None,
    );
}

#[derive(Clone, Copy)]
enum CodexCase<'a> {
    None,
    Fixture,
    Missing,
    WrongAncestry,
    Stock(&'a Path),
    Live(&'a Path, IpAddr),
    /// Real-key broker aimed at the live Provider, driven by the raw fixture
    /// sender: refusals must precede any upstream attempt (louiselm-4j6lt).
    LiveFixture(IpAddr),
}

#[expect(
    clippy::too_many_lines,
    reason = "One disposable fixture owns the certificate, non-root broker, launch and terminal containment."
)]
#[expect(
    clippy::fn_params_excessive_bools,
    reason = "Independent fault flags select one installed fixture case."
)]
fn installed_guard_case_for(
    hold_close_ack: bool,
    park: bool,
    broker_outage: bool,
    provider: bool,
    no_permission: bool,
    codex: CodexCase<'_>,
) {
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
    if !matches!(codex, CodexCase::None) {
        super::codex_chain::install(
            root.path(),
            &registry_root,
            &config,
            match codex {
                CodexCase::Stock(path) | CodexCase::Live(path, _) => Some(path),
                _ => None,
            },
            match codex {
                CodexCase::Missing => Some(super::codex_chain::MISSING_RUNTIME_SCRIPT),
                CodexCase::WrongAncestry => Some(super::codex_chain::WRONG_ANCESTRY_SCRIPT),
                _ => None,
            },
        );
    }
    if !no_permission {
        fs::write(root.path().join("brokered"), b"").unwrap();
    }
    let live_address = match codex {
        CodexCase::Live(_, address) | CodexCase::LiveFixture(address) => Some(address),
        _ => None,
    };
    let tls_server = (provider && live_address.is_none())
        .then(|| start_provider_upstream(root.path(), matches!(codex, CodexCase::Stock(_))));
    if provider {
        super::provider_credentials::provision_empty_state(root.path());
        if let Some((port, _)) = &tls_server {
            fs::write(root.path().join("guard-provider-port"), port.to_string()).unwrap();
        }
        if let Some(address) = live_address {
            fs::write(
                root.path().join("guard-provider-live-ip"),
                address.to_string(),
            )
            .unwrap();
        }
        let custody = ProviderCredentialStore::root_in(&root.path().join("state"));
        chown(&custody, Some(BROKER_UID), Some(BROKER_UID)).unwrap();
        fs::set_permissions(&custody, fs::Permissions::from_mode(0o700)).unwrap();
        let credential = custody.join("openai");
        if live_address.is_some() {
            fs::copy(LIVE_OPENAI_KEY, &credential).unwrap();
        } else {
            fs::write(&credential, b"fixture-secret").unwrap();
        }
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

    let (mut broker_child, lines) = broker_process(root.path(), no_permission.then_some("refuse"));
    marker(&lines, "BROKER_READY");
    let mut platform =
        SystemLaunchPlatform::new(paths.clone(), config.clone(), Duration::from_secs(5)).unwrap();
    platform.registry_root = registry_root.clone();
    let sessions = root.path().join("sessions");
    fs::create_dir(&sessions).unwrap();
    fs::set_permissions(&sessions, fs::Permissions::from_mode(0o711)).unwrap();
    let supervisor = LaunchSupervisor::new(
        connect_control_broker(&config, Duration::from_secs(5)).unwrap(),
        Arc::new(InstalledLaunchSigner::open(&paths, Duration::from_secs(5)).unwrap()),
        Arc::new(platform),
        Arc::new(Registry::open_trusted(&registry_root).unwrap()),
        sessions.clone(),
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
            request_for(super::codex_chain::runtime_directory(root.path()).as_deref()),
            config.operator_uid,
            now_ms,
            Box::new(move |outcome| sent.send(outcome).unwrap()),
        )
        .unwrap();
    let launched = result.recv_timeout(Duration::from_secs(30)).unwrap();
    if no_permission {
        assert!(matches!(
            launched,
            Err(SupervisorError::BrokeredProviderAuthenticationUnavailable)
        ));
        marker(&lines, "BROKER_REJECTED");
        assert!(broker_child.0.wait().unwrap().success());
        crate::launcher_install::acquire_identity(&paths, 0)
            .unwrap()
            .release()
            .unwrap();
        return;
    }
    let session = launched.unwrap();
    let read_markers = |lines: &mpsc::Receiver<String>| {
        marker(lines, "BROKER_GUARD_ACKED");
        let agent_pid: u32 = marker(lines, "BROKER_RUNNING ")
            .split_whitespace()
            .nth(1)
            .unwrap()
            .parse()
            .unwrap();
        let provider_address = provider.then(|| {
            marker(lines, "BROKER_PROVIDER_ADDR ")
                .split_whitespace()
                .nth(1)
                .unwrap()
                .parse::<SocketAddr>()
                .unwrap()
        });
        (agent_pid, provider_address)
    };
    let (agent_pid, provider_address) = read_markers(&lines);
    if live_address.is_some() {
        super::provider_credentials::assert_session_surfaces(
            root.path(),
            "openai",
            agent_pid,
            broker_child.0.id(),
            true,
        );
        super::provider_credentials::assert_records(root.path(), "openai");
        println!("LIVE_CUSTODY_PROBES_PASSED");
    }
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
    if matches!(codex, CodexCase::Stock(_) | CodexCase::Live(_, _)) {
        if live_address.is_some() {
            stock_acp_live_prompts(
                &mut controller_peer_input,
                &mut controller_peer_output,
                &sessions,
                root.path(),
            );
        } else {
            stock_acp_prompt(
                &mut controller_peer_input,
                &mut controller_peer_output,
                &sessions,
            );
        }
    } else if !matches!(codex, CodexCase::None) {
        // Not a setup message: held until the Codex-shaped descendant is
        // enrolled and policy activated, then the test Agent echoes it.
        let started = Instant::now();
        controller_peer_input
            .write_all(b"enroll-trigger\n")
            .unwrap();
        if matches!(codex, CodexCase::Missing | CodexCase::WrongAncestry) {
            let outcome = finished.recv_timeout(Duration::from_secs(20)).unwrap();
            assert_eq!(outcome, Err(SupervisorError::RelayFailed));
            if matches!(codex, CodexCase::Missing) {
                assert!(
                    started.elapsed() >= Duration::from_secs(1),
                    "missing descendant did not wait"
                );
            }
            marker(&lines, "BROKER_TERMINAL");
            assert!(!Path::new(&format!("/proc/{agent_pid}")).exists());
            assert!(broker_child.0.wait().unwrap().success());
            crate::launcher_install::acquire_identity(&paths, 0)
                .unwrap()
                .release()
                .unwrap();
            return;
        }
        controller_peer_output
            .set_read_timeout(Some(Duration::from_secs(20)))
            .unwrap();
        let mut echoed = [0; 15];
        controller_peer_output.read_exact(&mut echoed).unwrap();
        assert_eq!(&echoed, b"enroll-trigger\n");
    }
    // Run exhaustion reports its Park mid-test; teardown must not await a second.
    let mut parked_seen = false;
    if let Some(address) = provider_address {
        if matches!(codex, CodexCase::LiveFixture(_)) {
            controller_peer_output
                .set_read_timeout(Some(Duration::from_secs(20)))
                .unwrap();
            for variant in ["bad-effort", "bad-model"] {
                let refused = provider_exchange(
                    &mut controller_peer_input,
                    &mut controller_peer_output,
                    address,
                    variant,
                );
                assert!(
                    refused.contains("capability_denied"),
                    "{variant}: {refused}"
                );
                assert_eq!(live_spent(root.path()), 0, "{variant} reached upstream");
            }
            println!("LIVE_REFUSALS=bad-effort,bad-model UPSTREAM_ATTEMPTS=0");
            fs::write(root.path().join("guard-provider-done"), b"").unwrap();
        } else if live_address.is_some() {
            marker(&lines, "BROKER_PARKED");
            parked_seen = true;
            assert_eq!(live_spent(root.path()), 2);
            let outbox = crate::broker::attention::Outbox::open(
                &root.path().join("state/authorizations/attention-outbox"),
            )
            .unwrap();
            let attention = outbox.next().unwrap().unwrap();
            assert_eq!(
                attention.wire()["change"]["attention"]["kind"],
                "run_parked"
            );
            super::provider_credentials::assert_session_surfaces(
                root.path(),
                "openai",
                agent_pid,
                broker_child.0.id(),
                true,
            );
            super::provider_credentials::assert_records(root.path(), "openai");
            println!("LIVE_REQUESTS=2 EXHAUSTED=1 ATTENTION=run_parked");
            fs::write(root.path().join("guard-provider-done"), b"").unwrap();
        } else if matches!(codex, CodexCase::Stock(_)) {
            fs::write(root.path().join("guard-provider-done"), b"").unwrap();
            tls_server.unwrap().1.join().unwrap();
        } else {
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
            let invalid_host = provider_exchange(
                &mut controller_peer_input,
                &mut controller_peer_output,
                address,
                "bad-host",
            );
            assert!(
                invalid_host.starts_with("HTTP/1.1 400 Bad Request"),
                "{invalid_host}"
            );
            assert!(invalid_host.contains("invalid_request"), "{invalid_host}");
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
        if park && !parked_seen {
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
    output
        .read_exact(&mut marker)
        .unwrap_or_else(|error| panic!("{variant} fixture response: {error}"));
    assert_eq!(marker, [0x1b]);
    let mut length = [0; 4];
    output.read_exact(&mut length).unwrap();
    let length = u32::from_be_bytes(length) as usize;
    assert!(length <= 64 * 1024);
    let mut answer = vec![0; length];
    output.read_exact(&mut answer).unwrap();
    String::from_utf8(answer).unwrap()
}

fn stock_acp_prompt(
    input: &mut std::os::unix::net::UnixStream,
    output: &mut std::os::unix::net::UnixStream,
    sessions: &Path,
) {
    output
        .set_read_timeout(Some(Duration::from_secs(30)))
        .unwrap();
    let mut reader = BufReader::new(output.try_clone().unwrap());
    let mut request = |id: u64, method: &str, params: serde_json::Value| {
        writeln!(
            input,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            assert!(
                reader.read_line(&mut line).unwrap() > 0,
                "ACP closed before {method} replied"
            );
            let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
            assert!(
                !frame.to_string().contains("Code Mode is unavailable"),
                "stock Codex started without its measured Code Mode host"
            );
            assert!(
                frame.get("error").is_none(),
                "ACP {method} refused: {frame}"
            );
            if frame.get("id").and_then(serde_json::Value::as_u64) == Some(id) {
                return frame["result"].clone();
            }
        }
    };
    let initialized = request(
        1,
        "initialize",
        serde_json::json!({
            "protocolVersion":1,
            "clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true},"terminal":true},
            "clientInfo":{"name":"louiselm-stock-gate","version":"1"}
        }),
    );
    assert_eq!(initialized["protocolVersion"], 1);
    let created = request(
        2,
        "session/new",
        serde_json::json!({
            "cwd":sessions.join("session/workspace"), "mcpServers":[]
        }),
    );
    let session_id = created["sessionId"].as_str().unwrap();
    let completed = request(
        3,
        "session/prompt",
        serde_json::json!({
            "sessionId":session_id,
            "prompt":[{"type":"text","text":"Reply with OFFLINE_STOCK_OK."}]
        }),
    );
    assert_eq!(completed["stopReason"], "end_turn", "{completed}");
}

fn stock_acp_live_prompts(
    input: &mut std::os::unix::net::UnixStream,
    output: &mut std::os::unix::net::UnixStream,
    sessions: &Path,
    root: &Path,
) {
    output
        .set_read_timeout(Some(Duration::from_mins(1)))
        .unwrap();
    let mut reader = BufReader::new(output.try_clone().unwrap());
    let mut request = |id: u64, method: &str, params: serde_json::Value| {
        writeln!(
            input,
            "{}",
            serde_json::json!({"jsonrpc":"2.0","id":id,"method":method,"params":params})
        )
        .unwrap();
        let mut line = String::new();
        let mut denied = false;
        loop {
            line.clear();
            assert!(
                reader.read_line(&mut line).unwrap() > 0,
                "ACP closed during {method}"
            );
            let frame: serde_json::Value = serde_json::from_str(&line).unwrap();
            denied |= line.contains("capability_denied");
            // Frame kinds and error fields only; never agent text (louiselm-4j6lt).
            eprintln!(
                "LIVE_FRAME id={} method={} update={} error={} stop={}",
                frame["id"],
                frame["method"],
                frame["params"]["update"]["sessionUpdate"],
                frame["error"],
                frame["result"]["stopReason"]
            );
            if frame.get("id").and_then(serde_json::Value::as_u64) == Some(id) {
                return (
                    frame["result"].clone(),
                    denied,
                    frame.get("error").is_some(),
                );
            }
        }
    };
    let (initialized, denied, errored) = request(
        1,
        "initialize",
        serde_json::json!({
            "protocolVersion":1,
            "clientCapabilities":{"fs":{"readTextFile":true,"writeTextFile":true},"terminal":true},
            "clientInfo":{"name":"louiselm-live-gate","version":"1"}
        }),
    );
    assert_eq!(initialized["protocolVersion"], 1);
    assert!(!denied && !errored);
    let (created, denied, errored) = request(
        2,
        "session/new",
        serde_json::json!({"cwd":sessions.join("session/workspace"),"mcpServers":[]}),
    );
    assert!(!denied && !errored);
    let session_id = created["sessionId"].as_str().unwrap();
    // Stock Codex may lower an above-ceiling effort before sending, so broker
    // refusals are proven by the raw-sender LiveFixture case (louiselm-4j6lt).
    for (id, config_id, value) in [(3, "reasoning_effort", "low"), (4, "model", "gpt-5.6-luna")] {
        let (_, denied, errored) = request(
            id,
            "session/set_config_option",
            serde_json::json!({"sessionId":session_id,"configId":config_id,"value":value}),
        );
        assert!(!denied && !errored, "ACP configuration was refused");
    }
    for id in [9, 10] {
        let (completed, denied, errored) = request(
            id,
            "session/prompt",
            serde_json::json!({"sessionId":session_id,"prompt":[{"type":"text","text":"Reply with OK. Do not use tools."}]}),
        );
        assert_eq!(
            completed["stopReason"], "end_turn",
            "live turn did not complete"
        );
        assert!(!denied && !errored, "live turn received a refusal");
    }
    assert_eq!(live_spent(root), 2);
    println!("LIVE_TURNS=2");
    writeln!(
        input,
        "{}",
        serde_json::json!({
            "jsonrpc":"2.0","id":11,"method":"session/prompt",
            "params":{"sessionId":session_id,"prompt":[{"type":"text","text":"Reply with OK."}]}
        })
    )
    .unwrap();
}

fn live_spent(root: &Path) -> usize {
    let attempts = root
        .join("state/authorizations/provider-requests/runs")
        .join(Digest::of(b"run").hex());
    if !attempts.exists() {
        return 0;
    }
    fs::read_dir(attempts).unwrap().count()
}
