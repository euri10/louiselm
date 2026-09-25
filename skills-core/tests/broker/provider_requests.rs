//! Brokered Provider requests through the real launched `BrokerSession`, a real
//! credential store and a fake upstream (`louiselm-qbr.5.1.3.2.1`). No network.

use super::*;
use louiselm_skills::{
    broker::{
        BrokerSession,
        lifecycle::LifecycleCaller,
        provider_credentials::ProviderCredentialStore,
        provider_endpoint::serve_provider_connection,
        provider_extension::{ExtensionError, ExtensionRequest},
        provider_requests::HoldReason,
        provider_transport::{ProviderTransport, UpstreamResponse},
    },
    conformance::admission::Attendance,
    launch_protocol::{ChannelState, LifecycleAction, NextAction, SupervisorStatus},
    launch_receipt::{SessionState, SignedReceipt},
    provider_request::{ApprovedProviderRequests, Frames, ProviderRequest, ReasoningEffort},
};
use std::{
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    sync::Mutex,
};

#[path = "provider_handoff.rs"]
mod handoff;

const SECRET: &str = "sk-synthetic-broker-only-0000";
const HOST: &str = "127.0.0.1:40773";

fn body(model: &str) -> String {
    format!(
        r#"{{"model":"{model}","input":[],"tool_choice":"auto","parallel_tool_calls":false,"reasoning":{{"effort":"high","context":"all_turns"}},"store":false,"stream":true,"include":[],"prompt_cache_key":"k","text":{{"verbosity":"low"}},"client_metadata":{{}}}}"#
    )
}

fn frame() -> Vec<u8> {
    let body = body("gpt-5.6-luna");
    format!(
        "POST /v1/responses HTTP/1.1\r\naccept: text/event-stream\r\ncontent-type: application/json\r\nsession-id: s\r\nhost: {HOST}\r\ncontent-length: {}\r\n\r\n{body}",
        body.len()
    )
    .into_bytes()
}

fn parsed() -> ProviderRequest {
    let mut frames = Frames::new(HOST.into());
    frames.feed(&frame()).unwrap();
    frames.next_request().unwrap().unwrap()
}

fn with_policy(model: &str, effort: Option<&str>) -> ProviderRequest {
    let mut request = parsed();
    request.model = model.into();
    request.effort = effort.map(str::to_owned);
    request
}

fn approval(max_run_requests: u32) -> ApprovedProviderRequests {
    ApprovedProviderRequests {
        provider: "openai".into(),
        upstream: "https://api.openai.com/v1/responses".into(),
        addresses: vec!["192.0.2.1".parse().unwrap()],
        max_run_requests,
        models: vec!["gpt-5.6-luna".into()],
        max_effort: ReasoningEffort::High,
        expires_at_ms: 30_000,
    }
}

/// Body reader fed chunk by chunk, so a test can observe relay before completion.
struct Chunks(mpsc::Receiver<Vec<u8>>, Vec<u8>);

impl Read for Chunks {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        if self.1.is_empty() {
            match self.0.recv() {
                Ok(chunk) => self.1 = chunk,
                Err(_) => return Ok(0),
            }
        }
        let count = self.1.len().min(buffer.len());
        buffer[..count].copy_from_slice(&self.1[..count]);
        self.1.drain(..count);
        Ok(count)
    }
}

/// Upstream URL, bearer, forwarded headers and body of one attempt.
type Call = (String, String, Vec<(String, String)>, Vec<u8>);

#[derive(Default)]
struct FakeUpstream {
    calls: Mutex<Vec<Call>>,
    status: u16,
    stream: Mutex<Option<mpsc::Receiver<Vec<u8>>>>,
}

impl FakeUpstream {
    fn replying(status: u16) -> Self {
        Self {
            status,
            ..Self::default()
        }
    }
    fn calls(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

impl ProviderTransport for FakeUpstream {
    fn send(
        &self,
        approved: &ApprovedProviderRequests,
        bearer: &str,
        request: &ProviderRequest,
        _deadline: std::time::Instant,
    ) -> Result<UpstreamResponse, BrokerError> {
        self.calls.lock().unwrap().push((
            approved.upstream.clone(),
            bearer.to_owned(),
            request.headers.clone(),
            request.body.clone(),
        ));
        let body: Box<dyn Read + Send> = match self.stream.lock().unwrap().take() {
            Some(stream) => Box::new(Chunks(stream, Vec::new())),
            None => Box::new(std::io::Cursor::new(b"event: done\ndata: {}\n\n".to_vec())),
        };
        Ok(UpstreamResponse {
            status: self.status,
            content_type: Some("text/event-stream".into()),
            body,
        })
    }
}

struct Fixture {
    service: BrokerService,
    session: BrokerSession,
    peer: SeqpacketChannel,
    current: SupervisorStatus,
    credentials: ProviderCredentialStore,
    root: TempDir,
}

fn credentials(root: &Path, provider: Option<&str>) -> ProviderCredentialStore {
    let state = root.join("broker-state");
    let directory = ProviderCredentialStore::root_in(&state);
    std::fs::create_dir_all(&directory).unwrap();
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700)).unwrap();
    if let Some(provider) = provider {
        let file = directory.join(provider);
        std::fs::write(&file, SECRET).unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    ProviderCredentialStore::open(
        &state,
        rustix::process::getuid().as_raw(),
        rustix::process::getgid().as_raw(),
    )
    .unwrap()
}

fn fixture(permission: Option<ApprovedProviderRequests>, provider: Option<&str>) -> Fixture {
    attended_fixture(permission, provider, Attendance::Unattended)
}

fn attended_fixture(
    permission: Option<ApprovedProviderRequests>,
    provider: Option<&str>,
    attendance: Attendance,
) -> Fixture {
    let root = TempDir::new().unwrap();
    let socket = root.path().join("broker.sock");
    let request = request("provider-requests");
    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
    let mut approved = grant(&request);
    approved.provider_requests = permission;
    approved.conformance.attendance = attendance;
    authorizations.authorize(&approved, 1000).unwrap();
    let service = BrokerService::bind(
        &socket,
        authorizations,
        ReceiptStore::open(&root.path().join("receipts"), trusted_release()).unwrap(),
        AuditLog::open(&root.path().join("audit")).unwrap(),
        local_pin(),
    )
    .unwrap();
    let peer = thread::spawn(move || fake_supervisor(&socket, &request, 2000));
    let session = service
        .serve_launch(2000, verify_fixture_signature)
        .unwrap();
    let (binding, peer) = peer.join().unwrap();
    let current = lifecycle::status(&binding);
    Fixture {
        service,
        session,
        peer,
        current,
        credentials: credentials(root.path(), provider),
        root,
    }
}

impl Fixture {
    /// Admission that reaches the supervisor status exchange.
    fn admit(
        &mut self,
        upstream: &FakeUpstream,
        now_ms: u64,
    ) -> Result<UpstreamResponse, BrokerError> {
        thread::scope(|scope| {
            let (peer, current) = (&self.peer, &self.current);
            let answer = scope.spawn(move || lifecycle::answer_one_status_query(peer, current));
            let result = self.service.serve_provider_request(
                &mut self.session,
                &self.credentials,
                upstream,
                &parsed(),
                now_ms,
                verify_fixture_signature,
            );
            answer.join().unwrap();
            result
        })
    }

    /// Admission refused before any supervisor exchange.
    fn refuse(&mut self, upstream: &FakeUpstream, now_ms: u64) -> BrokerError {
        self.service
            .serve_provider_request(
                &mut self.session,
                &self.credentials,
                upstream,
                &parsed(),
                now_ms,
                verify_fixture_signature,
            )
            .map(|_| ())
            .unwrap_err()
    }
}

#[test]
fn admitted_request_reaches_only_the_configured_upstream_with_the_broker_key() {
    let mut fixture = fixture(Some(approval(5)), Some("openai"));
    let upstream = FakeUpstream::replying(200);
    let response = fixture.admit(&upstream, 2500).unwrap();
    assert_eq!(response.status, 200);
    let calls = upstream.calls.lock().unwrap();
    let (url, bearer, headers, body_bytes) = &calls[0];
    assert_eq!(url, "https://api.openai.com/v1/responses");
    assert_eq!(bearer, SECRET);
    assert_eq!(body_bytes, &body("gpt-5.6-luna").into_bytes());
    assert!(
        headers
            .iter()
            .all(|(name, value)| name != "authorization" && !value.contains(SECRET))
    );
    assert_eq!(fixture.service.provider_requests_spent("run-1").unwrap(), 1);
    drop(calls);
    // Receipts, audit, authorization and ledger records never hold the key;
    // only the custody file itself does.
    let mut pending = vec![fixture.root.path().to_path_buf()];
    let mut holders = Vec::new();
    while let Some(path) = pending.pop() {
        if path.is_dir() {
            pending.extend(
                fs::read_dir(&path)
                    .unwrap()
                    .map(|entry| entry.unwrap().path()),
            );
        } else if fs::read(&path)
            .is_ok_and(|bytes| bytes.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()))
        {
            holders.push(path);
        }
    }
    assert_eq!(
        holders,
        vec![
            ProviderCredentialStore::root_in(&fixture.root.path().join("broker-state"))
                .join("openai")
        ]
    );
}

#[test]
fn missing_permission_or_credential_spends_nothing_and_calls_nothing() {
    let upstream = FakeUpstream::replying(200);
    let mut unpermitted = fixture(None, Some("openai"));
    assert!(matches!(
        unpermitted.refuse(&upstream, 2500),
        BrokerError::InvalidGrant
    ));
    let mut keyless = fixture(Some(approval(5)), None);
    assert!(matches!(
        keyless.refuse(&upstream, 2500),
        BrokerError::Policy(error) if error.code == ErrorCode::InvalidRequest
    ));
    assert_eq!(upstream.calls(), 0);
    assert_eq!(keyless.service.provider_requests_spent("run-1").unwrap(), 0);
}

#[test]
fn exhausted_run_budget_is_refused_before_any_upstream_attempt() {
    let mut fixture = fixture(Some(approval(1)), Some("openai"));
    let upstream = FakeUpstream::replying(200);
    fixture.admit(&upstream, 2500).unwrap();
    assert!(matches!(
        fixture.admit(&upstream, 2600).map(|_| ()).unwrap_err(),
        BrokerError::ProviderBudgetExhausted
    ));
    assert_eq!(upstream.calls(), 1);
    assert_eq!(fixture.service.provider_requests_spent("run-1").unwrap(), 1);
}

#[test]
fn models_and_efforts_outside_the_grant_are_denied_before_any_spend() {
    let mut fixture = fixture(Some(approval(5)), Some("openai"));
    let upstream = FakeUpstream::replying(200);
    for (model, effort) in [
        ("gpt-6-astra", Some("high")),
        ("gpt-5.6-luna", Some("xhigh")),
        ("gpt-5.6-luna", None),
    ] {
        let error = fixture
            .service
            .serve_provider_request(
                &mut fixture.session,
                &fixture.credentials,
                &upstream,
                &with_policy(model, effort),
                2500,
                verify_fixture_signature,
            )
            .map(|_| ())
            .unwrap_err();
        assert!(
            matches!(&error, BrokerError::Policy(e) if e.code == ErrorCode::CapabilityDenied && !e.retryable),
            "{model} {effort:?}: {error:?}"
        );
    }
    assert_eq!(upstream.calls(), 0);
    assert_eq!(fixture.service.provider_requests_spent("run-1").unwrap(), 0);
}

#[test]
fn expired_permission_is_refused_without_an_attempt() {
    let mut fixture = fixture(Some(approval(5)), Some("openai"));
    let upstream = FakeUpstream::replying(200);
    assert!(matches!(
        fixture.refuse(&upstream, 30_000),
        BrokerError::Expired
    ));
    assert_eq!(upstream.calls(), 0);
}

#[test]
fn rejected_key_is_a_typed_refusal_with_no_retry() {
    for status in [401, 403] {
        let mut fixture = fixture(Some(approval(5)), Some("openai"));
        let upstream = FakeUpstream::replying(status);
        assert!(matches!(
            fixture.admit(&upstream, 2500).map(|_| ()).unwrap_err(),
            BrokerError::Policy(error) if error.code == ErrorCode::CredentialUnavailable
        ));
        assert_eq!(upstream.calls(), 1);
        // The attempt happened: its unit stays spent.
        assert_eq!(fixture.service.provider_requests_spent("run-1").unwrap(), 1);
    }
}

fn read_until(stream: &mut UnixStream, needle: &[u8]) -> Vec<u8> {
    let mut received = Vec::new();
    let mut buffer = [0; 1024];
    while !received
        .windows(needle.len())
        .any(|window| window == needle)
    {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0, "closed before {needle:?}: {received:?}");
        received.extend_from_slice(&buffer[..count]);
    }
    received
}

#[test]
fn endpoint_relays_admitted_chunks_as_they_arrive_and_never_the_key() {
    let mut fixture = fixture(Some(approval(5)), Some("openai"));
    let upstream = FakeUpstream::replying(200);
    let (chunks, stream) = mpsc::channel();
    *upstream.stream.lock().unwrap() = Some(stream);
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    thread::scope(|scope| {
        let fixture = &mut fixture;
        let upstream = &upstream;
        let endpoint = scope.spawn(move || {
            serve_provider_connection(server, HOST.into(), |request| {
                thread::scope(|inner| {
                    let (peer, current) = (&fixture.peer, &fixture.current);
                    let answer =
                        inner.spawn(move || lifecycle::answer_one_status_query(peer, current));
                    let result = fixture.service.serve_provider_request(
                        &mut fixture.session,
                        &fixture.credentials,
                        upstream,
                        request,
                        2500,
                        verify_fixture_signature,
                    );
                    answer.join().unwrap();
                    result
                })
            })
        });
        client.write_all(&frame()).unwrap();
        chunks.send(b"event: first\n\n".to_vec()).unwrap();
        let head = read_until(&mut client, b"event: first");
        assert!(head.starts_with(b"HTTP/1.1 200 OK\r\n"));
        assert!(
            head.windows(8)
                .any(|w| w.eq_ignore_ascii_case(b"chunked\r"))
        );
        // The upstream has not finished: the first chunk arrived on its own.
        chunks.send(b"event: last\n\n".to_vec()).unwrap();
        drop(chunks);
        let tail = read_until(&mut client, b"0\r\n\r\n");
        assert!(tail.windows(10).any(|w| w == b"event: las"));
        drop(client);
        endpoint.join().unwrap().unwrap();
        let all = [head, tail].concat();
        assert!(!all.windows(SECRET.len()).any(|w| w == SECRET.as_bytes()));
    });
    assert_eq!(upstream.calls(), 1);
}

#[test]
fn endpoint_refuses_malformed_or_denied_requests_with_a_typed_error() {
    for (bytes, expected, refusal) in [
        (
            b"GET / HTTP/1.1\r\nhost: x\r\n\r\n".to_vec(),
            "400",
            BrokerError::ProviderBudgetExhausted,
        ),
        (frame(), "403", BrokerError::ProviderBudgetExhausted),
        (frame(), "403", BrokerError::Expired),
        (
            frame(),
            "403",
            ProtocolError::new(ErrorCode::CapabilityDenied, None, None).into(),
        ),
    ] {
        let upstream = FakeUpstream::replying(200);
        let (mut client, server) = UnixStream::pair().unwrap();
        client
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let endpoint = thread::spawn(move || {
            let mut refusal = Some(refusal);
            serve_provider_connection(server, HOST.into(), move |_request| {
                Err(refusal.take().unwrap())
            })
        });
        client.write_all(&bytes).unwrap();
        let mut response = Vec::new();
        client.read_to_end(&mut response).unwrap();
        let text = String::from_utf8(response).unwrap();
        assert!(text.starts_with(&format!("HTTP/1.1 {expected} ")), "{text}");
        let (_, body) = text.split_once("\r\n\r\n").unwrap();
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        let error: ProtocolError = serde_json::from_value(body["error"].clone()).unwrap();
        error.validate().unwrap();
        if expected == "403" {
            // Budget and expiry refusals are final for the runtime: only an
            // operator extension changes the answer, so nothing retries.
            assert_eq!(error.code, ErrorCode::CapabilityDenied);
            assert!(!error.retryable);
            assert_eq!(error.next_action, NextAction::ContactOperator);
        }
        drop(client);
        let _ = endpoint.join().unwrap();
        assert_eq!(upstream.calls(), 0);
    }
}

impl Fixture {
    /// Settles the hold while the fake supervisor answers its status query and
    /// then serves the Park request, as the Session worker's idle tick would.
    fn settle_and_park(&mut self, now_ms: u64) -> Option<SignedReceipt> {
        thread::scope(|scope| {
            let (peer, current) = (&self.peer, &self.current);
            let supervisor = scope.spawn(move || {
                lifecycle::answer_one_status_query(peer, current);
                lifecycle::drive_lifecycle_peer(peer, current)
            });
            let receipt = self
                .service
                .settle_provider_hold(&mut self.session, now_ms, verify_fixture_signature)
                .unwrap();
            assert_eq!(receipt.as_ref(), Some(&supervisor.join().unwrap()));
            receipt
        })
    }

    fn attention_entries(&self) -> usize {
        fs::read_dir(
            self.root
                .path()
                .join("authorizations/attention-outbox/entries"),
        )
        .unwrap()
        .count()
    }

    fn parked(&self, receipt: &SignedReceipt) -> SupervisorStatus {
        let mut parked = self.current.clone();
        let head = louiselm_skills::launch_receipt::ReceiptHead {
            sequence: receipt.payload.sequence,
            digest: receipt.digest().to_string(),
        };
        parked.state = SessionState::Parked;
        parked.channel_state = ChannelState::Revoked;
        parked.launcher_head = Some(head.clone());
        parked.broker_head = Some(head);
        parked
    }
}

#[test]
fn exhaustion_holds_the_run_parks_the_session_and_raises_one_attention_item() {
    let mut fixture = fixture(Some(approval(1)), Some("openai"));
    let upstream = FakeUpstream::replying(200);
    fixture.admit(&upstream, 2500).unwrap();
    assert!(matches!(
        fixture.admit(&upstream, 2600).map(|_| ()).unwrap_err(),
        BrokerError::ProviderBudgetExhausted
    ));
    let hold = fixture.service.provider_hold("run-1").unwrap().unwrap();
    assert_eq!(hold.reason, HoldReason::Exhausted);
    // Held: refused before any supervisor exchange or upstream attempt.
    assert!(matches!(
        fixture.refuse(&upstream, 2700),
        BrokerError::ProviderBudgetExhausted
    ));
    assert_eq!(upstream.calls(), 1);
    assert_eq!(fixture.attention_entries(), 0);

    let receipt = fixture.settle_and_park(2800).unwrap();
    assert!(matches!(
        receipt.payload.outcome,
        louiselm_skills::launch_receipt::ReceiptOutcome::Park { .. }
    ));
    assert_eq!(
        fixture
            .service
            .receipts()
            .stored_bytes("provider-requests")
            .unwrap()
            .last(),
        Some(&receipt.canonical_bytes())
    );
    assert_eq!(fixture.attention_entries(), 1);

    // The next tick sees the Session Parked: the same Attention item, no new Park.
    let parked = fixture.parked(&receipt);
    thread::scope(|scope| {
        let peer = &fixture.peer;
        let parked = &parked;
        let supervisor = scope.spawn(move || lifecycle::answer_one_status_query(peer, parked));
        assert_eq!(
            fixture
                .service
                .settle_provider_hold(&mut fixture.session, 3800, verify_fixture_signature)
                .unwrap(),
            None
        );
        supervisor.join().unwrap();
    });
    assert_eq!(fixture.attention_entries(), 1);
    assert_eq!(fixture.service.provider_hold("run-1").unwrap(), Some(hold));
}

#[test]
fn a_held_run_cannot_be_resumed_or_offered_resume() {
    let mut fixture = fixture(Some(approval(1)), Some("openai"));
    let upstream = FakeUpstream::replying(200);
    fixture.admit(&upstream, 2500).unwrap();
    let _ = fixture.admit(&upstream, 2600);
    let receipt = fixture.settle_and_park(2700).unwrap();
    let operator = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    let mut resume = lifecycle::park(fixture.session.authorization());
    resume.request_id = "resume-1".into();
    resume.action = LifecycleAction::Resume;
    resume.expected_state = SessionState::Parked;
    resume.expected_receipt_sequence = Some(receipt.payload.sequence);
    // Refused before any supervisor exchange: no peer answers here.
    assert!(matches!(
        fixture
            .service
            .request_lifecycle(&mut fixture.session, &operator, &resume, 2800, verify_fixture_signature)
            .unwrap_err(),
        BrokerError::Policy(error) if error.code == ErrorCode::InvalidRequest
    ));
    let parked = fixture.parked(&receipt);
    let status = thread::scope(|scope| {
        let peer = &fixture.peer;
        let parked = &parked;
        let supervisor = scope.spawn(move || lifecycle::answer_one_status_query(peer, parked));
        let status = fixture
            .service
            .session_status(
                &mut fixture.session,
                &operator,
                2800,
                verify_fixture_signature,
            )
            .unwrap();
        supervisor.join().unwrap();
        status
    });
    assert!(!status.allowed_actions.contains(&LifecycleAction::Resume));
    assert!(status.allowed_actions.contains(&LifecycleAction::Disposal));
}

#[test]
fn permission_expiry_holds_the_run_with_or_without_a_request() {
    let upstream = FakeUpstream::replying(200);
    let mut requested = fixture(Some(approval(5)), Some("openai"));
    assert!(matches!(
        requested.refuse(&upstream, 30_000),
        BrokerError::Expired
    ));
    assert_eq!(
        requested
            .service
            .provider_hold("run-1")
            .unwrap()
            .map(|hold| (hold.reason, hold.held_at_ms)),
        Some((HoldReason::Expired, 30_000))
    );

    // An idle Session is parked by its tick once the permission lapses.
    let mut idle = fixture(Some(approval(5)), Some("openai"));
    assert_eq!(
        idle.service
            .settle_provider_hold(&mut idle.session, 29_999, verify_fixture_signature)
            .unwrap(),
        None
    );
    assert_eq!(idle.service.provider_hold("run-1").unwrap(), None);
    idle.settle_and_park(30_000).unwrap();
    assert_eq!(
        idle.service
            .provider_hold("run-1")
            .unwrap()
            .map(|hold| hold.reason),
        Some(HoldReason::Expired)
    );
    assert_eq!(idle.attention_entries(), 1);
    assert_eq!(upstream.calls(), 0);
}

#[test]
fn sessions_without_provider_permission_are_never_held() {
    let mut fixture = fixture(None, Some("openai"));
    assert_eq!(
        fixture
            .service
            .settle_provider_hold(&mut fixture.session, 1_000_000, verify_fixture_signature)
            .unwrap(),
        None
    );
    assert_eq!(fixture.service.provider_hold("run-1").unwrap(), None);
}

#[test]
fn a_stream_still_running_at_expiry_is_cut_locally_keeping_what_arrived() {
    let mut fixture = fixture(Some(approval(5)), Some("openai"));
    let upstream = FakeUpstream::replying(200);
    let (chunks, stream) = mpsc::channel();
    *upstream.stream.lock().unwrap() = Some(stream);
    let (mut client, server) = UnixStream::pair().unwrap();
    client
        .set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    // Admitted 2 s before the permission's 30 000 ms expiry: wide enough that a
    // loaded runner still relays the first chunk in time.
    let admitted_at = 28_000;
    thread::scope(|scope| {
        let fixture = &mut fixture;
        let upstream = &upstream;
        let endpoint = scope.spawn(move || {
            serve_provider_connection(server, HOST.into(), |request| {
                thread::scope(|inner| {
                    let (peer, current) = (&fixture.peer, &fixture.current);
                    let answer =
                        inner.spawn(move || lifecycle::answer_one_status_query(peer, current));
                    let result = fixture.service.serve_provider_request(
                        &mut fixture.session,
                        &fixture.credentials,
                        upstream,
                        request,
                        admitted_at,
                        verify_fixture_signature,
                    );
                    answer.join().unwrap();
                    result
                })
            })
        });
        client.write_all(&frame()).unwrap();
        // One request only: an uncut relay then ends the connection cleanly.
        client.shutdown(std::net::Shutdown::Write).unwrap();
        chunks.send(b"event: before\n\n".to_vec()).unwrap();
        let head = read_until(&mut client, b"event: before");
        // The upstream is still streaming when the permission expires.
        thread::sleep(Duration::from_millis(2_500));
        chunks.send(b"event: after\n\n".to_vec()).unwrap();
        // End the upstream too, so an uncut relay fails here instead of hanging.
        drop(chunks);
        let mut tail = Vec::new();
        client.read_to_end(&mut tail).unwrap();
        let all = [head, tail].concat();
        assert!(!all.windows(12).any(|w| w == b"event: after"));
        // No terminating chunk: the runtime sees an incomplete outcome.
        assert!(!all.ends_with(b"0\r\n\r\n"));
        assert!(matches!(
            endpoint.join().unwrap(),
            Err(BrokerError::ProviderUnavailable)
        ));
    });
    // One attempt, still spent; nothing retried or refunded.
    assert_eq!(upstream.calls(), 1);
    assert_eq!(fixture.service.provider_requests_spent("run-1").unwrap(), 1);
    // The worker's next tick holds the Run for expiry and Parks it.
    fixture.settle_and_park(30_000).unwrap();
    assert_eq!(
        fixture
            .service
            .provider_hold("run-1")
            .unwrap()
            .map(|hold| hold.reason),
        Some(HoldReason::Expired)
    );
}

fn extension(
    request_id: &str,
    additional_requests: u32,
    expires_at_ms: Option<u64>,
) -> ExtensionRequest {
    ExtensionRequest {
        request_id: request_id.into(),
        additional_requests,
        expires_at_ms,
    }
}

impl Fixture {
    fn extend(
        &self,
        request: &ExtensionRequest,
        uid: u32,
        now_ms: u64,
    ) -> Result<louiselm_skills::broker::provider_extension::ExtensionOutcome, ExtensionError> {
        self.service
            .extend_provider_budget(&self.session, uid, request, now_ms)
            .map_err(|error| match error {
                BrokerError::ProviderExtension(error) => error,
                other => panic!("{other:?}"),
            })
    }

    /// The operator Resumes the Parked Session; the fake supervisor then runs it.
    fn resume(&mut self, parked: &SignedReceipt, now_ms: u64) -> SignedReceipt {
        let parked_status = self.parked(parked);
        let mut resume = lifecycle::park(self.session.authorization());
        resume.request_id = format!("resume-{}", parked.payload.sequence);
        resume.action = LifecycleAction::Resume;
        resume.expected_state = SessionState::Parked;
        resume.expected_receipt_sequence = Some(parked.payload.sequence);
        let receipt = thread::scope(|scope| {
            let (peer, parked_status) = (&self.peer, &parked_status);
            let supervisor =
                scope.spawn(move || lifecycle::drive_lifecycle_peer(peer, parked_status));
            let receipt = self
                .service
                .request_lifecycle(
                    &mut self.session,
                    &LifecycleCaller::Operator {
                        uid: CONTROLLER_UID,
                    },
                    &resume,
                    now_ms,
                    verify_fixture_signature,
                )
                .unwrap();
            assert_eq!(receipt, supervisor.join().unwrap());
            receipt
        });
        let head = louiselm_skills::launch_receipt::ReceiptHead {
            sequence: receipt.payload.sequence,
            digest: receipt.digest().to_string(),
        };
        self.current.launcher_head = Some(head.clone());
        self.current.broker_head = Some(head);
        receipt
    }

    /// Spends the Run's only unit, is refused once and parks.
    fn exhaust_and_park(&mut self, upstream: &FakeUpstream) -> SignedReceipt {
        self.admit(upstream, 2500).unwrap();
        let _ = self.admit(upstream, 2600);
        self.settle_and_park(2700).unwrap()
    }
}

#[test]
fn only_an_operator_extension_lifts_the_hold_and_it_never_resumes() {
    let mut fixture = attended_fixture(Some(approval(1)), Some("openai"), Attendance::Interactive);
    let upstream = FakeUpstream::replying(200);
    // Nothing to lift before a hold exists.
    assert_eq!(
        fixture.extend(&extension("ext-1", 2, None), CONTROLLER_UID, 2400),
        Err(ExtensionError::NotHeld)
    );
    let receipt = fixture.exhaust_and_park(&upstream);
    assert_eq!(fixture.attention_entries(), 1);
    // Units that would not lift the exhaustion record nothing.
    assert_eq!(
        fixture.extend(&extension("ext-0", 0, None), CONTROLLER_UID, 2800),
        Err(ExtensionError::Insufficient)
    );
    let outcome = fixture
        .extend(&extension("ext-1", 2, None), CONTROLLER_UID, 2800)
        .unwrap();
    assert_eq!(
        (outcome.total_requests, outcome.spent, outcome.expires_at_ms),
        (3, 1, 30_000)
    );
    assert_eq!(fixture.service.provider_hold("run-1").unwrap(), None);
    // The Attention item is cleared, not deleted: one Upsert, one Clear.
    assert_eq!(fixture.attention_entries(), 2);
    // An exact retry answers the recorded extension; a changed one conflicts.
    assert_eq!(
        fixture
            .extend(&extension("ext-1", 2, None), CONTROLLER_UID, 9999)
            .unwrap()
            .extension,
        outcome.extension
    );
    assert_eq!(
        fixture.extend(&extension("ext-1", 5, None), CONTROLLER_UID, 2900),
        Err(ExtensionError::Conflict)
    );
    // Still Parked: the operator is now offered Resume, and must send it.
    let parked = fixture.parked(&receipt);
    let operator = LifecycleCaller::Operator {
        uid: CONTROLLER_UID,
    };
    let status = thread::scope(|scope| {
        let (peer, parked) = (&fixture.peer, &parked);
        let supervisor = scope.spawn(move || lifecycle::answer_one_status_query(peer, parked));
        let status = fixture
            .service
            .session_status(
                &mut fixture.session,
                &operator,
                2900,
                verify_fixture_signature,
            )
            .unwrap();
        supervisor.join().unwrap();
        status
    });
    assert!(status.allowed_actions.contains(&LifecycleAction::Resume));
    assert_eq!(status.state, SessionState::Parked);
}

#[test]
fn extended_units_are_spent_then_a_new_hold_needs_a_new_extension() {
    let mut fixture = attended_fixture(Some(approval(1)), Some("openai"), Attendance::Interactive);
    let upstream = FakeUpstream::replying(200);
    let first = fixture.exhaust_and_park(&upstream);
    fixture
        .extend(&extension("ext-1", 1, None), CONTROLLER_UID, 2800)
        .unwrap();
    // The operator Resumes; the Session spends the new unit.
    fixture.resume(&first, 2900);
    fixture.admit(&upstream, 3000).unwrap();
    assert!(matches!(
        fixture.admit(&upstream, 3100).map(|_| ()).unwrap_err(),
        BrokerError::ProviderBudgetExhausted
    ));
    let second = fixture.service.provider_hold("run-1").unwrap().unwrap();
    assert!(second.held_at_ms >= 3100);
    // A fresh Park identity and Attention item for the new hold.
    let again = fixture.settle_and_park(3200).unwrap();
    assert_ne!(again.payload.request_id, first.payload.request_id);
    assert_eq!(fixture.attention_entries(), 3);
    assert_eq!(upstream.calls(), 2);
    assert_eq!(fixture.service.provider_requests_spent("run-1").unwrap(), 2);
}

#[test]
fn an_expired_run_needs_a_later_expiry_within_the_launch() {
    let mut permission = approval(5);
    permission.expires_at_ms = 20_000;
    let mut fixture = attended_fixture(Some(permission), Some("openai"), Attendance::Interactive);
    let parked = fixture.settle_and_park(20_000).unwrap();
    for (request, refusal) in [
        (extension("ext-1", 3, None), ExtensionError::Insufficient),
        (
            extension("ext-2", 0, Some(20_000)),
            ExtensionError::ExpiryOutOfRange,
        ),
        // The launch itself expires at 30 000 ms.
        (
            extension("ext-3", 0, Some(30_001)),
            ExtensionError::ExpiryOutOfRange,
        ),
    ] {
        assert_eq!(
            fixture.extend(&request, CONTROLLER_UID, 20_100),
            Err(refusal)
        );
    }
    let outcome = fixture
        .extend(&extension("ext-4", 0, Some(25_000)), CONTROLLER_UID, 20_100)
        .unwrap();
    assert_eq!(outcome.expires_at_ms, 25_000);
    fixture.resume(&parked, 20_200);
    let upstream = FakeUpstream::replying(200);
    fixture.admit(&upstream, 21_000).unwrap();
    // The extended expiry is enforced exactly like the granted one.
    assert!(matches!(
        fixture.refuse(&upstream, 25_000),
        BrokerError::Expired
    ));
    assert_eq!(upstream.calls(), 1);
}

#[test]
fn only_the_controller_of_an_attended_session_may_extend() {
    let upstream = FakeUpstream::replying(200);
    let mut attended = attended_fixture(Some(approval(1)), Some("openai"), Attendance::Interactive);
    attended.exhaust_and_park(&upstream);
    assert_eq!(
        attended.extend(&extension("ext-1", 1, None), CONTROLLER_UID + 1, 2800),
        Err(ExtensionError::WrongOperator)
    );
    let mut unattended = fixture(Some(approval(1)), Some("openai"));
    unattended.exhaust_and_park(&upstream);
    assert_eq!(
        unattended.extend(&extension("ext-1", 1, None), CONTROLLER_UID, 2800),
        Err(ExtensionError::Unattended)
    );
    for held in [&attended, &unattended] {
        assert!(held.service.provider_hold("run-1").unwrap().is_some());
    }
}
