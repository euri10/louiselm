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

fn request(port: u16, raw: &str) -> String {
    let mut stream = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).unwrap();
    stream
        .set_read_timeout(Some(Duration::from_secs(3)))
        .unwrap();
    stream.write_all(raw.as_bytes()).unwrap();
    let mut output = String::new();
    stream.read_to_string(&mut output).unwrap();
    output
}

fn post(port: u16, prefix: &str, route: &str) -> String {
    format!(
        "POST {prefix}{route} HTTP/1.1\r\nHost: localhost:{port}\r\nOrigin: http://localhost:{port}\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{{}}"
    )
}

#[test]
fn bad_host_origin_framing_and_token_have_no_authority_then_cancel_closes_port() {
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
    assert!(request(port, &post(port, &prefix, "cancel")).contains("trust unchanged"));
    assert!(matches!(
        worker.join().unwrap(),
        Err(RecoveryError::Cancelled)
    ));
    assert!(!called.load(Ordering::SeqCst));
    assert!(TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_err());
}

#[test]
fn completed_and_expired_servers_dispose_their_listener() {
    let browser = Browser::bind().unwrap();
    let port = browser.port().unwrap();
    let prefix = browser.prefix.clone();
    let worker = thread::spawn(move || {
        browser.run("confirm", &json!({}), &json!({}), |_| Ok((42, "committed")))
    });
    assert!(request(port, &post(port, &prefix, "finish")).contains("committed"));
    assert_eq!(worker.join().unwrap().unwrap(), 42);
    assert!(TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_err());
    let mut browser = Browser::bind().unwrap();
    let port = browser.port().unwrap();
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
    assert!(TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port)).is_err());
}

#[test]
#[ignore = "Opt-in isolated Chrome virtual-authenticator test; see README. Never uses installed trust or real credentials."]
fn virtual_browser() {
    use crate::{
        Store,
        sshsig::SkPolicy,
        trust::{
            TrustStore,
            paper::PaperPhrase,
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
    let paper = PaperPhrase::generate().unwrap();
    let change = RecoveryChange::new(&trust, vec![], Some(&paper)).unwrap();
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
