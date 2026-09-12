//! Required guest-only hostile conformance; ordinary cargo runs the verdict tests.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "Test fixtures fail explicitly on missing prerequisites or failed observations."
)]

#[path = "hostile/filesystem.rs"]
mod filesystem;
#[path = "hostile/fixture.rs"]
mod fixture;
#[path = "hostile/probe.rs"]
mod probe;
#[path = "hostile/processes.rs"]
mod processes;
#[path = "hostile/sockets.rs"]
mod sockets;

use probe::{Observation, Outcome};

fn check_pair(names: &[&str], outside: &[Observation], inside: &[Observation]) -> bool {
    louiselm_skills::conformance::pair(names, outside, inside).is_ok_and(|checks| {
        checks.iter().all(|check| {
            check.control == Outcome::Allowed && matches!(check.confined, Outcome::Denied(_))
        })
    })
}

#[test]
fn probe_child() {
    if std::env::var_os("LOUISELM_HOSTILE_CHILD").is_some() {
        probe::serve();
    }
}

#[test]
#[ignore = "guest root only: scripts/launcher-conformance requires all prerequisites"]
fn hostile_host_identity_matrix() {
    let fixture = fixture::Fixture::new();
    filesystem::run(&fixture);
    processes::run(&fixture);
    sockets::run(&fixture);
    fixture.finish();
}

#[test]
fn missing_duplicate_failed_and_contradictory_probes_never_pass() {
    assert!(!check_pair(&[], &[], &[]));
    let allowed = Observation {
        name: "operator-home".into(),
        outcome: Outcome::Allowed,
    };
    let denied = Observation {
        name: "operator-home".into(),
        outcome: Outcome::Denied("ENOENT".into()),
    };
    let error = Observation {
        name: "operator-home".into(),
        outcome: Outcome::Error("timeout".into()),
    };
    let yes = std::slice::from_ref(&allowed);
    let no = std::slice::from_ref(&denied);
    assert!(check_pair(&["operator-home"], yes, no));
    assert!(!check_pair(&["operator-home"], &[], no));
    assert!(!check_pair(&["operator-home"], yes, &[]));
    assert!(!check_pair(&["operator-home"], no, no));
    assert!(!check_pair(&["operator-home"], yes, yes));
    assert!(!check_pair(&["operator-home"], yes, &[error]));
    assert!(!check_pair(
        &["operator-home"],
        yes,
        &[denied.clone(), denied]
    ));
    assert!(!check_pair(
        &["operator-home"],
        &[allowed.clone(), allowed],
        &[Observation {
            name: "operator-home".into(),
            outcome: Outcome::Denied("ENOENT".into())
        }]
    ));
}
