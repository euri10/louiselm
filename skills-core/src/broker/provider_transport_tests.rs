#![allow(
    clippy::unwrap_used,
    reason = "In-memory HTTP fixtures assert deterministic protocol outcomes."
)]

use super::*;
use crate::provider_request::Frames;
use std::{
    io::Cursor,
    sync::{Arc, Mutex},
};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport,
};

const SECRET: &str = "sk-synthetic-broker-only-0000";

#[derive(Debug, Default)]
struct Wire {
    targets: Vec<String>,
    sent: Vec<u8>,
}

#[derive(Debug)]
struct FakeConnector {
    response: Vec<u8>,
    wire: Arc<Mutex<Wire>>,
}

impl Connector for FakeConnector {
    type Out = FakeStream;
    fn connect(
        &self,
        details: &ConnectionDetails<'_>,
        _chained: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        self.wire
            .lock()
            .unwrap()
            .targets
            .push(format!("{} via {}", details.uri, details.addrs[0]));
        assert!(details.config.proxy().is_none());
        Ok(Some(FakeStream {
            input: Cursor::new(self.response.clone()),
            buffers: LazyBuffers::new(8192, 8192),
            wire: Arc::clone(&self.wire),
        }))
    }
}

#[derive(Debug)]
struct FakeStream {
    input: Cursor<Vec<u8>>,
    buffers: LazyBuffers,
    wire: Arc<Mutex<Wire>>,
}

impl Transport for FakeStream {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }
    fn transmit_output(&mut self, amount: usize, _timeout: NextTimeout) -> Result<(), ureq::Error> {
        let output = &self.buffers.output()[..amount];
        self.wire.lock().unwrap().sent.extend_from_slice(output);
        Ok(())
    }
    fn await_input(&mut self, _timeout: NextTimeout) -> Result<bool, ureq::Error> {
        let count = self.input.read(self.buffers.input_append_buf())?;
        self.buffers.input_appended(count);
        Ok(count > 0)
    }
    fn is_open(&mut self) -> bool {
        false
    }
    // This in-memory peer substitutes for TLS. Certificate validation remains
    // the real Rustls connector's responsibility, not a claim made by this test.
    fn is_tls(&self) -> bool {
        true
    }
}

fn approved() -> ApprovedProviderRequests {
    ApprovedProviderRequests {
        provider: "openai".into(),
        upstream: "https://api.openai.com/v1/responses".into(),
        addresses: vec!["192.0.2.1".parse().unwrap()],
        max_run_requests: 5,
        expires_at_ms: 60_000,
    }
}

fn request() -> ProviderRequest {
    let body = r#"{"model":"gpt-5.6-luna","input":[],"stream":true}"#;
    let mut frames = Frames::new("127.0.0.1:1".into());
    frames
        .feed(
            format!(
                "POST /v1/responses HTTP/1.1\r\nhost: 127.0.0.1:1\r\naccept: text/event-stream\r\ncontent-type: application/json\r\nsession-id: s\r\ncontent-length: {}\r\n\r\n{body}",
                body.len()
            )
            .as_bytes(),
        )
        .unwrap();
    frames.next_request().unwrap().unwrap()
}

fn run(response: &[u8]) -> (Result<UpstreamResponse, BrokerError>, Wire) {
    let wire = Arc::new(Mutex::new(Wire::default()));
    let client = ureq::Agent::with_parts(
        config(),
        FakeConnector {
            response: response.to_vec(),
            wire: Arc::clone(&wire),
        },
        resolver(&approved()).unwrap(),
    );
    let result = exchange(&client, &approved(), SECRET, &request());
    let wire = std::mem::take(&mut *wire.lock().unwrap());
    (result, wire)
}

#[test]
fn request_carries_the_broker_bearer_once_to_the_pinned_destination() {
    let (result, wire) = run(
        b"HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: 12\r\n\r\nevent: done\n",
    );
    let mut response = result.unwrap();
    let mut body = String::new();
    response.body.read_to_string(&mut body).unwrap();
    assert_eq!(body, "event: done\n");
    assert_eq!(response.content_type.as_deref(), Some("text/event-stream"));
    assert_eq!(
        wire.targets,
        vec!["https://api.openai.com/v1/responses via 192.0.2.1:443"]
    );
    let sent = String::from_utf8(wire.sent).unwrap();
    assert!(
        sent.starts_with("POST /v1/responses HTTP/1.1\r\n"),
        "{sent}"
    );
    assert_eq!(
        sent.matches(&format!("authorization: Bearer {SECRET}"))
            .count(),
        1
    );
    assert!(sent.contains("session-id: s\r\n"));
    assert!(!sent.to_ascii_lowercase().contains("host: 127.0.0.1"));
    assert!(sent.ends_with(r#"{"model":"gpt-5.6-luna","input":[],"stream":true}"#));
}

#[test]
fn redirects_and_encoded_bodies_are_not_followed_or_relayed() {
    for response in [
        b"HTTP/1.1 307 Temporary Redirect\r\nlocation: https://attacker.invalid/v1/responses\r\ncontent-length: 0\r\n\r\n".as_slice(),
        b"HTTP/1.1 200 OK\r\ncontent-encoding: gzip\r\ncontent-length: 0\r\n\r\n".as_slice(),
    ] {
        let (result, wire) = run(response);
        assert!(
            matches!(result, Err(BrokerError::ProviderUnavailable)),
            "{:?}",
            result.as_ref().map(|r| r.status)
        );
        assert_eq!(wire.targets.len(), 1);
    }
}

#[test]
fn upstream_status_is_reported_not_retried() {
    for status in [
        "401 Unauthorized",
        "429 Too Many Requests",
        "500 Internal Server Error",
    ] {
        let (result, wire) =
            run(format!("HTTP/1.1 {status}\r\ncontent-length: 0\r\n\r\n").as_bytes());
        assert_eq!(
            result.unwrap().status.to_string(),
            status.split(' ').next().unwrap()
        );
        assert_eq!(wire.targets.len(), 1);
    }
}
