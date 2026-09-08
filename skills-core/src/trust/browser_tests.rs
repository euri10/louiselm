//! Loopback transport regressions and an opt-in virtual-browser fixture.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Loopback-only fixtures and assertions."
)]

use super::*;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

fn connect(port: u16) -> TcpStream {
    let stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream
}

fn exchange(mut stream: TcpStream, raw: &str) -> String {
    stream.write_all(raw.as_bytes()).unwrap();
    let mut output = String::new();
    stream.read_to_string(&mut output).unwrap();
    output
}

fn request(port: u16, raw: &str) -> String {
    exchange(connect(port), raw)
}

fn assert_listener_disposed(mut pending: TcpStream) {
    // Observe a connection queued on the original listener, never a released
    // port. Concurrent fork can retain its CLOEXEC descriptor until exec, so
    // allow bounded teardown while still failing if a descriptor leaks (b7us).
    match pending.read(&mut [0]) {
        Ok(0) => (),
        Err(error) if error.kind() == io::ErrorKind::ConnectionReset => (),
        result => panic!("listener did not dispose its queued connection: {result:?}"),
    }
}

fn post(port: u16, prefix: &str, route: &str) -> String {
    format!(
        "POST {prefix}{route} HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}"
    )
}

#[test]
fn bad_host_origin_framing_and_token_have_no_authority_then_cancel_disposes_listener() {
    let browser = Browser::bind().unwrap();
    let port = browser.port().unwrap();
    let prefix = browser.prefix.clone();
    let called = Arc::new(AtomicBool::new(false));
    let observed = Arc::clone(&called);
    let worker = thread::spawn(move || {
        browser.run("confirm", &json!({"sequence":7}), &json!({}), |_| {
            observed.store(true, Ordering::SeqCst);
            Ok(((), "committed"))
        })
    });
    let valid = post(port, &prefix, "finish");
    for invalid in [
        valid.replace(
            &format!("Origin: http://localhost:{port}"),
            "Origin: https://attacker.example",
        ),
        valid.replace(&format!("Host: localhost:{port}"), "Host: attacker.example"),
        valid.replace(
            "Content-Length: 2",
            "Content-Length: 2\r\nContent-Length: 2",
        ),
        valid.replace(
            "Content-Length: 2",
            "Transfer-Encoding: chunked\r\nContent-Length: 2",
        ),
        valid.replace("Content-Length: 2", "Content-Length: 999999"),
        valid.replace(
            "Content-Length: 2",
            "Sec-Fetch-Site: cross-site\r\nContent-Length: 2",
        ),
    ] {
        assert!(request(port, &invalid).starts_with("HTTP/1.1 400"));
    }
    assert!(request(port, &valid.replace(&prefix, "/wrong-token/")).starts_with("HTTP/1.1 404"));
    let page = request(
        port,
        &format!("GET {prefix} HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"),
    );
    assert!(page.contains("frame-ancestors 'none'"));
    assert!(page.contains("Never enter a paper phrase here"));
    let cancel = connect(port);
    let pending = connect(port);
    assert!(exchange(cancel, &post(port, &prefix, "cancel")).contains("trust unchanged"));
    assert!(matches!(
        worker.join().unwrap(),
        Err(RecoveryError::Cancelled)
    ));
    assert!(!called.load(Ordering::SeqCst));
    assert_listener_disposed(pending);
}

#[test]
fn completed_and_expired_servers_dispose_their_listener() {
    let browser = Browser::bind().unwrap();
    let port = browser.port().unwrap();
    let prefix = browser.prefix.clone();
    let finish = connect(port);
    let pending = connect(port);
    let worker = thread::spawn(move || {
        browser.run("confirm", &json!({}), &json!({}), |_| Ok((42, "committed")))
    });
    assert!(exchange(finish, &post(port, &prefix, "finish")).contains("committed"));
    assert_eq!(worker.join().unwrap().unwrap(), 42);
    assert_listener_disposed(pending);
    let mut browser = Browser::bind().unwrap();
    let pending = connect(browser.port().unwrap());
    browser.deadline = Instant::now();
    assert!(
        browser
            .run(
                "confirm",
                &json!({}),
                &json!({}),
                |_| -> Result<((), &'static str), RecoveryError> { panic!("expired callback") }
            )
            .is_err()
    );
    assert_listener_disposed(pending);
}

#[test]
fn late_browser_document_reports_only_the_shared_deadline_remaining() {
    // C14/se0i: paper/tunnel work consumed most of the five-minute setup
    // before Google registration. Public timing: issue comments 1298-1299,
    // codex/01a07d34-4adb-7491-8d0a-3f23cd0796e1; no real credential fixture.
    let deadline = Instant::now() + Duration::from_secs(10);
    let browser = Browser::bind().unwrap().until(deadline);
    let port = browser.port().unwrap();
    let prefix = browser.prefix.clone();
    let worker = thread::spawn(move || {
        browser.run(
            "register",
            &json!({"operation":"PUBLIC DEADLINE FIXTURE"}),
            &json!({"publicKey":{"timeout":300_000}}),
            |_| -> Result<((), &'static str), RecoveryError> { panic!("no approval") },
        )
    });
    let before_request = deadline.duration_since(Instant::now()).as_millis();
    let response = request(
        port,
        &format!("GET {prefix}ceremony.json HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"),
    );
    let document: Value = serde_json::from_str(response.split_once("\r\n\r\n").unwrap().1).unwrap();
    let remaining = document["remaining_ms"].as_u64();
    // A later reload must not reuse a budget cached when run() began. Only
    // compare ordering, not an exact wall-clock duration or scheduler latency.
    thread::sleep(Duration::from_millis(30));
    let later = request(
        port,
        &format!("GET {prefix}ceremony.json HTTP/1.1\r\nHost: localhost:{port}\r\n\r\n"),
    );
    let later: Value = serde_json::from_str(later.split_once("\r\n\r\n").unwrap().1).unwrap();
    // Dispose even when the assertion below is red.
    request(port, &post(port, &prefix, "cancel"));
    assert!(matches!(
        worker.join().unwrap(),
        Err(RecoveryError::Cancelled)
    ));
    assert!(
        remaining.is_some(),
        "ceremony must carry its remaining lifetime"
    );
    assert!(u128::from(remaining.unwrap()) <= before_request);
    assert!(later["remaining_ms"].as_u64().unwrap() < remaining.unwrap());
    assert_eq!(document["options"]["publicKey"]["timeout"], 300_000);
}

#[test]
fn expired_browser_reports_expiry_without_calling_finish() {
    let browser = Browser::bind().unwrap().until(Instant::now());
    let error = browser
        .run(
            "confirm",
            &json!({}),
            &json!({}),
            |_| -> Result<((), &'static str), RecoveryError> { panic!("expired callback") },
        )
        .unwrap_err();
    assert!(error.to_string().starts_with("recovery ceremony expired"));
}

#[test]
fn browser_failure_reports_only_known_codes_without_committing() {
    for (kind, body, expected) in [
        (
            "register",
            r#"{"code":"InvalidStateError"}"#,
            "registration failed (InvalidStateError)",
        ),
        (
            "authenticate",
            r#"{"code":"NotAllowedError"}"#,
            "authentication failed (NotAllowedError)",
        ),
        (
            "confirm",
            r#"{"code":"TypeError"}"#,
            "confirmation failed (TypeError)",
        ),
        (
            "register",
            r#"{"code":"UNSAFE FIXTURE DETAIL"}"#,
            "passkey recovery refused",
        ),
        (
            "register",
            r#"{"code":"InvalidStateError","message":"UNSAFE FIXTURE DETAIL"}"#,
            "passkey recovery refused",
        ),
        ("register", r#"{"code":null}"#, "passkey recovery refused"),
        ("register", "{", "passkey recovery refused"),
    ] {
        let browser = Browser::bind()
            .unwrap()
            .until(Instant::now() + Duration::from_secs(3));
        let port = browser.port().unwrap();
        let prefix = browser.prefix.clone();
        let worker = thread::spawn(move || {
            browser.run(
                kind,
                &json!({}),
                &json!({}),
                |_| -> Result<((), &'static str), RecoveryError> {
                    panic!("browser failure must not submit proof")
                },
            )
        });
        let reporting = connect(port);
        let queued = connect(port);
        let response = exchange(
            reporting,
            &format!(
                "POST {prefix}failed HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        );
        if response.starts_with("HTTP/1.1 404") {
            // Bound the red run before this endpoint exists.
            request(port, &post(port, &prefix, "cancel"));
        }
        let error = worker.join().unwrap().unwrap_err().to_string();
        assert_listener_disposed(queued);
        assert!(error.contains(expected), "{error}");
        assert!(!error.contains("UNSAFE FIXTURE DETAIL"));
        assert!(!response.contains("UNSAFE FIXTURE DETAIL"));
    }
}

#[test]
fn inherited_listener_delays_teardown_until_its_descriptor_closes() {
    let mut browser = Browser::bind().unwrap();
    let mut pending = connect(browser.port().unwrap());
    // fork and try_clone retain the same listener until the last descriptor closes.
    let inherited = browser.listener.try_clone().unwrap();
    browser.deadline = Instant::now();
    assert!(matches!(
        browser.run(
            "confirm",
            &json!({}),
            &json!({}),
            |_| -> Result<((), &'static str), RecoveryError> { panic!("expired callback") }
        ),
        Err(RecoveryError::Expired)
    ));
    pending.set_nonblocking(true).unwrap();
    assert!(
        matches!(pending.read(&mut [0]), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    drop(inherited);
    pending.set_nonblocking(false).unwrap();
    assert_listener_disposed(pending);
}

#[test]
#[ignore = "Opt-in isolated Chrome virtual-authenticator test; see README. Never uses installed trust or real credentials."]
fn virtual_browser() {
    use crate::{
        Store,
        sshsig::SkPolicy,
        trust::{
            TrustStore,
            passkey::{PendingAuthentication, PendingRegistration},
            recovery::RecoveryChange,
        },
    };
    let fixture = tempfile::tempdir().unwrap();
    let store = Store::open(fixture.path()).unwrap();
    let mut trust = TrustStore::bootstrap(
        &store,
        "PUBLIC BROWSER FIXTURE <script>throw 1</script>",
        "test-primary",
        "test-recovery",
        SkPolicy::none(),
        0,
    )
    .unwrap();
    let browser = Browser::bind().unwrap();
    let (mut pending, options) =
        PendingRegistration::start(&trust, browser.port().unwrap()).unwrap();
    println!("PUBLIC_FIXTURE_REGISTER={}", browser.url());
    let registration = browser
        .run(
            "register",
            &json!({"operation":"PUBLIC FIXTURE ONLY", "trust_domain":trust.trust_domain}),
            &serde_json::to_value(options).unwrap(),
            |value| {
                let response = serde_json::from_value(value).unwrap();
                pending.finish(&trust, &response).map(|candidate| {
                    (
                        candidate,
                        "Public fixture registration verified; no real authority changed.",
                    )
                })
            },
        )
        .unwrap();
    // Only this test fixture assigns a key directly; production uses authorized apply.
    trust.passkey = Some(registration.credential().clone());
    let original = trust.clone();
    let browser = Browser::bind().unwrap();
    let (mut pending, options) =
        PendingRegistration::start(&trust, browser.port().unwrap()).unwrap();
    println!("PUBLIC_FIXTURE_REPLACE={}", browser.url());
    // cl1p: the driver keeps the SAME virtual authenticator and its old key.
    // This exercises actual browser exclusion behavior, not a forged new ID.
    let candidate = browser
        .run(
            "register",
            &json!({"operation":"PUBLIC FIXTURE REPLACE", "trust_domain":trust.trust_domain}),
            &serde_json::to_value(options).unwrap(),
            |value| {
                pending
                    .finish(&trust, &serde_json::from_value(value).unwrap())
                    .map(|candidate| {
                        (
                            candidate,
                            "Public fixture registration verified; no real authority changed.",
                        )
                    })
            },
        )
        .unwrap();
    assert_ne!(candidate.credential(), registration.credential());
    assert_eq!(
        trust, original,
        "registration alone must not retire the old key"
    );
    let change = RecoveryChange::enroll_passkey(&trust, &candidate).unwrap();
    let browser = Browser::bind().unwrap();
    let (mut pending, options) =
        PendingAuthentication::start(&trust, &change, browser.port().unwrap()).unwrap();
    println!("PUBLIC_FIXTURE_AUTHENTICATE={}", browser.url());
    browser.run("authenticate", &serde_json::to_value(&change).unwrap(), &serde_json::to_value(options).unwrap(), |value| {
        pending.finish(&serde_json::from_value(value).unwrap())?;
        Ok(((), "Public fixture assertion verified. Exact change binding checked; no real authority changed."))
    }).unwrap();
    let browser = Browser::bind().unwrap();
    println!("PUBLIC_FIXTURE_CANCEL={}", browser.url());
    assert!(matches!(
        browser.run(
            "confirm",
            &json!({"operation":"CANCEL THIS PUBLIC FIXTURE"}),
            &json!({}),
            |_| -> Result<((), &'static str), RecoveryError> {
                panic!("cancelled browser must never submit")
            }
        ),
        Err(RecoveryError::Cancelled)
    ));
}
