//! Brokered Provider requests through the real launched `BrokerSession`, a real
//! credential store and a fake upstream (`louiselm-qbr.5.1.3.2.1`). No network.

use super::*;
use louiselm_skills::{
    broker::{
        BrokerSession,
        provider_credentials::ProviderCredentialStore,
        provider_endpoint::serve_provider_connection,
        provider_transport::{ProviderTransport, UpstreamResponse},
    },
    launch_protocol::SupervisorStatus,
    provider_request::{ApprovedProviderRequests, Frames, ProviderRequest, ReasoningEffort},
};
use std::{
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, net::UnixStream},
    sync::Mutex,
};

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
    let root = TempDir::new().unwrap();
    let socket = root.path().join("broker.sock");
    let request = request("provider-requests");
    let authorizations =
        AuthorizationStore::open(&root.path().join("authorizations"), pool(4)).unwrap();
    let mut approved = grant(&request);
    approved.provider_requests = permission;
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
        assert!(text.contains("\"code\""), "{text}");
        drop(client);
        let _ = endpoint.join().unwrap();
        assert_eq!(upstream.calls(), 0);
    }
}
