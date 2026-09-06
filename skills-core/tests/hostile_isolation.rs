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
    !names.is_empty()
        && outside.len() == names.len()
        && inside.len() == names.len()
        && names.iter().all(|name| {
            let observations =
                |rows: &[Observation]| rows.iter().filter(|row| row.name == *name).count();
            observations(outside) == 1
                && observations(inside) == 1
                && outside
                    .iter()
                    .any(|row| row.name == *name && matches!(row.outcome, Outcome::Allowed))
                && inside
                    .iter()
                    .any(|row| row.name == *name && matches!(row.outcome, Outcome::Denied(_)))
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
}

#[test]
fn missing_duplicate_failed_and_contradictory_probes_never_pass() {
    assert!(!check_pair(&[], &[], &[]));
    let allowed = Observation {
        name: "sentinel".into(),
        outcome: Outcome::Allowed,
    };
    let denied = Observation {
        name: "sentinel".into(),
        outcome: Outcome::Denied("ENOENT".into()),
    };
    let error = Observation {
        name: "sentinel".into(),
        outcome: Outcome::Error("timeout".into()),
    };
    let yes = std::slice::from_ref(&allowed);
    let no = std::slice::from_ref(&denied);
    assert!(check_pair(&["sentinel"], yes, no));
    assert!(!check_pair(&["sentinel"], &[], no));
    assert!(!check_pair(&["sentinel"], yes, &[]));
    assert!(!check_pair(&["sentinel"], no, no));
    assert!(!check_pair(&["sentinel"], yes, yes));
    assert!(!check_pair(&["sentinel"], yes, &[error]));
    assert!(!check_pair(&["sentinel"], yes, &[denied.clone(), denied]));
    assert!(!check_pair(
        &["sentinel"],
        &[allowed.clone(), allowed],
        &[Observation {
            name: "sentinel".into(),
            outcome: Outcome::Denied("ENOENT".into())
        }]
    ));
}
