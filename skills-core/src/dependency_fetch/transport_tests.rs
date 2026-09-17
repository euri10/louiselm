#![allow(
    clippy::unwrap_used,
    reason = "In-memory HTTP fixtures assert deterministic protocol outcomes."
)]

use super::*;
use crate::{
    Digest,
    dependency_fetch::{
        Attendance, Candidate, DependencyPolicy, DependencySession, StartingLockfile,
    },
};
use std::{io::Cursor, sync::Mutex};
use ureq::unversioned::transport::{
    Buffers, ConnectionDetails, Connector, LazyBuffers, NextTimeout, Transport,
};

#[derive(Debug)]
struct FakeConnector {
    response: Vec<u8>,
    targets: Arc<Mutex<Vec<String>>>,
}

impl Connector for FakeConnector {
    type Out = FakeStream;
    fn connect(
        &self,
        details: &ConnectionDetails<'_>,
        _chained: Option<()>,
    ) -> Result<Option<Self::Out>, ureq::Error> {
        self.targets.lock().unwrap().push(details.uri.to_string());
        assert!(details.config.proxy().is_none());
        assert_eq!(details.addrs[0], "192.0.2.1:443".parse().unwrap());
        Ok(Some(FakeStream {
            input: Cursor::new(self.response.clone()),
            buffers: LazyBuffers::new(8192, 8192),
        }))
    }
}

#[derive(Debug)]
struct FakeStream {
    input: Cursor<Vec<u8>>,
    buffers: LazyBuffers,
}
impl Transport for FakeStream {
    fn buffers(&mut self) -> &mut dyn Buffers {
        &mut self.buffers
    }
    fn transmit_output(
        &mut self,
        _amount: usize,
        _timeout: NextTimeout,
    ) -> Result<(), ureq::Error> {
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

fn candidate() -> Candidate {
    Candidate {
        name: "example".into(),
        version: "1.0.0".into(),
        source: Source::Registry {
            registry: "test".into(),
        },
        integrity: Some(Digest::of(b"archive").to_string()),
    }
}

fn owner() -> DependencySession {
    DependencySession::new(
        DependencyPolicy {
            session_id: "session".into(),
            run_id: "run".into(),
            envelope_revision: 1,
            attendance: Attendance::Unattended,
            starting: StartingLockfile::cargo(b"version = 4\n").unwrap(),
            preapproved: vec![candidate()],
            expires_at_ms: 60_000,
            max_fetches: 2,
            max_bytes: 1024,
        },
        1,
    )
    .unwrap()
}

fn client(response: &[u8], targets: &Arc<Mutex<Vec<String>>>) -> ureq::Agent {
    ureq::Agent::with_parts(
        config(Duration::from_secs(1)),
        FakeConnector {
            response: response.to_vec(),
            targets: Arc::clone(targets),
        },
        PinnedResolver {
            origin: url::Url::parse("https://registry.invalid/example").unwrap(),
            addresses: vec!["192.0.2.1".parse().unwrap()],
        },
    )
}

#[test]
fn dependency_http_refuses_redirects_without_disclosing_the_target() {
    let targets = Arc::default();
    let client = client(b"HTTP/1.1 302 Found\r\nLocation: https://secret.attacker.invalid/leak\r\nContent-Length: 0\r\n\r\n", &targets);
    let permit = owner().begin_fetch(&candidate(), 128, 2).unwrap();
    assert!(download(&client, "https://registry.invalid/example", &permit).is_err());
    assert_eq!(
        *targets.lock().unwrap(),
        vec!["https://registry.invalid/example"]
    );
}

#[test]
fn dependency_http_bounds_opaque_bytes_and_checks_revocation_before_io() {
    for (response, bound, success) in [
        (
            &b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\narchive"[..],
            7,
            true,
        ),
        (
            &b"HTTP/1.1 200 OK\r\nContent-Length: 7\r\n\r\narchive"[..],
            6,
            false,
        ),
        (
            &b"HTTP/1.1 200 OK\r\nContent-Encoding: gzip\r\nContent-Length: 7\r\n\r\narchive"[..],
            7,
            false,
        ),
        (
            &b"HTTP/1.1 500 Error\r\nContent-Length: 0\r\n\r\n"[..],
            7,
            false,
        ),
    ] {
        let targets = Arc::default();
        let client = client(response, &targets);
        let mut owner = owner();
        let permit = owner.begin_fetch(&candidate(), bound, 2).unwrap();
        let result = download(&client, "https://registry.invalid/example", &permit);
        assert_eq!(result.is_ok(), success, "{result:?}");
        if success {
            assert_eq!(result.unwrap(), b"archive");
        }
        owner.revoke();
        assert!(download(&client, "https://registry.invalid/example", &permit).is_err());
        assert_eq!(targets.lock().unwrap().len(), 1);
    }
}

#[test]
fn dependency_endpoint_scope_is_prebound_and_has_no_dns_fallback() {
    let targets = Arc::default();
    let client = client(b"", &targets);
    let permit = owner().begin_fetch(&candidate(), 128, 2).unwrap();
    assert!(download(&client, "https://other.invalid/example", &permit).is_err());
    assert!(targets.lock().unwrap().is_empty());
    for template in [
        "http://registry.invalid/{name}",
        "https://{name}.invalid/archive",
        "https://user:secret@registry.invalid/{name}",
        "https://registry.invalid/{unknown}",
    ] {
        assert!(
            HttpsTransport::new(
                vec![RegistryEndpoint {
                    registry: "test".into(),
                    archive_template: template.into(),
                    addresses: vec!["192.0.2.1".parse().unwrap()]
                }],
                Duration::from_secs(1)
            )
            .is_err()
        );
    }
}
